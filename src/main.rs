//! High-performance PDF to WebP/SVG converter.
//!
//! Run with no arguments for a guided, interactive setup.
//! Run with --help to see all options for repeat/scripted use.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::Parser;
use console::style;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use shard::config::{AppConfig, OutputFormat};
#[cfg(feature = "pdfium")]
use shard::converter::PopplerRenderer;
use shard::converter::{MuPdfRenderer, PdfRenderer};
use shard::mirror;
use shard::worker;

/// Convert a folder of PDFs into optimized WebP images.
///
/// Run with no options for a step-by-step guided setup.
#[derive(Parser, Debug)]
#[command(name = "shard", version, about, long_about = None)]
struct Cli {
    /// Folder containing PDF files (searched recursively)
    #[arg(short, long)]
    source: Option<PathBuf>,

    /// Folder where converted images will be saved
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Image sharpness (90-200). Higher = sharper but larger files. Default: 150
    #[arg(long, default_value_t = 150)]
    dpi: u32,

    /// Re-convert files even if already converted
    #[arg(long)]
    overwrite: bool,

    /// Skip the interactive confirmation and run immediately
    #[arg(short = 'y', long)]
    yes: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    println!();
    println!(
        "  {} {}",
        style("shard — PDF → WebP Converter").bold().cyan(),
        style(env!("CARGO_PKG_VERSION")).dim()
    );
    println!("  {}", style("─────────────────────").cyan());
    println!();

    let cli = Cli::parse();

    let config = if cli.source.is_some() && cli.output.is_some() {
        build_config_from_args(&cli)
    } else {
        run_wizard(&cli)?
    };

    // Validate config.
    let errors = config.validate();
    if !errors.is_empty() {
        println!();
        println!("  {}", style("Something is not quite right:").red().bold());
        for e in &errors {
            println!("    • {e}");
        }
        println!();
        println!("  Run with {} for all options.", style("--help").yellow());
        return Ok(());
    }

    // Scan for files.
    let scanned = mirror::scan_pdf_files(&config.source_path)?;
    let total = scanned.len();

    if total == 0 {
        println!(
            "  {} No PDF files found in that folder.",
            style("!").yellow()
        );
        return Ok(());
    }

    // Final confirmation (unless --yes).
    if !cli.yes {
        println!();
        println!("  Ready to convert:");
        println!("    From:  {}", style(config.source_path.display()).cyan());
        println!("    To:    {}", style(config.output_path.display()).cyan());
        println!("    DPI:   {}", config.dpi);
        println!("    Files:");
        for f in &scanned {
            let rel = f.relative_path.display().to_string();
            let label = if rel.len() > 55 {
                format!("…{}", &rel[rel.len().saturating_sub(54)..])
            } else {
                rel
            };
            println!("      • {}", style(label).dim());
        }
        println!();
        let proceed = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("  Start conversion?")
            .default(true)
            .wait_for_newline(true)
            .interact()?;
        if !proceed {
            println!("  Cancelled. Nothing was converted.");
            return Ok(());
        }
    }

    // Check disk space.
    if let Err(e) = worker::check_disk_space(&config.output_path, total, 10, config.dpi) {
        println!("  {} Disk space check failed: {e}", style("✗").red());
        return Ok(());
    }

    run_conversion(config, scanned)
}

fn build_config_from_args(cli: &Cli) -> AppConfig {
    AppConfig {
        source_path: cli.source.clone().unwrap(),
        output_path: cli.output.clone().unwrap(),
        format: OutputFormat::Webp,
        dpi: cli.dpi,
        quality: 75,
        adaptive_encoding: true,
        quality_target: 85.0,
        svg_precision: 4,
        svg_no_text: false,
        svg_strip_background: true,
        overwrite: cli.overwrite,
    }
}

/// Interactive guided setup.
fn run_wizard(cli: &Cli) -> anyhow::Result<AppConfig> {
    let mut config = AppConfig::default();

    println!("  This tool turns PDF files into smaller WebP images for the web.");
    println!();

    // ── Source folder ──────────────────────────────────────
    config.source_path = match &cli.source {
        Some(p) => p.clone(),
        None => {
            println!("  {} Where are your PDF files?", style("Step 1").bold());
            println!("    Tip: drag a folder into this window to fill in the path.");
            let input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("  Folder with PDFs")
                .validate_with(|input: &String| -> Result<(), &str> {
                    let path = PathBuf::from(input.trim().trim_matches('\''));
                    if path.is_dir() {
                        Ok(())
                    } else {
                        Err("That doesn't look like a folder. Please check the path.")
                    }
                })
                .interact_text()?;
            PathBuf::from(input.trim().trim_matches('\''))
        }
    };

    // Quick scan.
    let scanned = mirror::scan_pdf_files(&config.source_path)?;
    println!(
        "    {} Found {} PDF file(s):",
        style("✓").green(),
        scanned.len()
    );
    for f in &scanned {
        println!("       • {}", f.relative_path.display());
    }
    if scanned.is_empty() {
        println!(
            "    {} No PDFs found. Double-check the path.",
            style("!").yellow()
        );
        std::process::exit(0);
    }
    println!();

    // ── Output folder ──────────────────────────────────────
    config.output_path = match &cli.output {
        Some(p) => p.clone(),
        None => {
            println!(
                "  {} Where should the converted images go?",
                style("Step 2").bold()
            );
            println!("    Tip: a new folder will be created automatically if needed.");
            let default_out = config
                .source_path
                .parent()
                .unwrap_or(&config.source_path)
                .join("webp-output");
            let input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("  Output folder")
                .default(default_out.to_string_lossy().to_string())
                .interact_text()?;
            PathBuf::from(input.trim().trim_matches('\''))
        }
    };
    println!();

    // ── Quality preset ─────────────────────────────────────
    println!("  {} How should images look?", style("Step 3").bold());
    let preset = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("  Choose a quality level")
        .items(&[
            "150 DPI (Medium) — balanced size and quality",
            "200 DPI (High) — sharpest output, larger files",
            "90 DPI (Low) — smallest files, slightly softer",
            "Custom DPI — enter your own value (50-300)",
        ])
        .default(0)
        .interact()?;

    match preset {
        0 => {
            config.dpi = 150;
            config.adaptive_encoding = true;
            config.quality_target = 85.0;
        }
        1 => {
            config.dpi = 200;
            config.adaptive_encoding = true;
            config.quality_target = 92.0;
        }
        2 => {
            config.dpi = 90;
            config.adaptive_encoding = true;
            config.quality_target = 75.0;
        }
        3 => {
            let input: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("  Enter DPI (50-300)")
                .default("150".into())
                .validate_with(|input: &String| -> Result<(), &str> {
                    let v: u32 = input.trim().parse().map_err(|_| "Please enter a number")?;
                    if v >= 50 && v <= 300 {
                        Ok(())
                    } else {
                        Err("DPI must be between 50 and 300")
                    }
                })
                .interact_text()?;
            config.dpi = input.trim().parse().unwrap_or(150);
            config.adaptive_encoding = true;
            config.quality_target = 85.0;
        }
        _ => unreachable!(),
    }
    println!();

    config.overwrite = cli.overwrite;
    Ok(config)
}

fn run_conversion(
    config: AppConfig,
    scanned: Vec<shard::mirror::ScannedFile>,
) -> anyhow::Result<()> {
    let total = scanned.len();

    // Collect original file sizes before passing ownership.
    let file_sizes: Vec<(String, u64)> = scanned
        .iter()
        .map(|f| {
            let name = f.relative_path.display().to_string();
            let size = std::fs::metadata(&f.full_path)
                .map(|m| m.len())
                .unwrap_or(0);
            (name, size)
        })
        .collect();

    // Create renderer.
    #[cfg(feature = "pdfium")]
    let renderer: Arc<dyn PdfRenderer> = match shard::converter::PdfiumRenderer::new() {
        Ok(r) => Arc::new(r),
        Err(_) => {
            println!(
                "  {} Pdfium not available, using poppler fallback.",
                style("!").yellow()
            );
            Arc::new(PopplerRenderer)
        }
    };
    // Use MuPDF as primary renderer (pipelined, in-memory).
    // If MuPDF is unavailable at runtime, the pipeline will
    // gracefully fall back to the configured PdfRenderer (Poppler).
    #[cfg(not(feature = "pdfium"))]
    let renderer: Arc<dyn PdfRenderer> = Arc::new(MuPdfRenderer);

    let cancel_flag = Arc::new(AtomicBool::new(false));

    let rx = worker::start_conversion(
        scanned,
        config.output_path.clone(),
        renderer,
        config.format,
        config.dpi,
        config.quality,
        config.adaptive_encoding,
        config.quality_target,
        config.svg_precision,
        config.svg_no_text,
        config.svg_strip_background,
        config.overwrite,
        cancel_flag,
    );

    // ── Progress display ───────────────────────────────────
    let multi = MultiProgress::new();
    let overall = multi.add(ProgressBar::new(total as u64));
    overall.set_style(
        ProgressStyle::with_template(
            "  {prefix:.bold} [{bar:30.cyan/blue}] {pos}/{len} files  {msg}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ "),
    );
    overall.set_prefix("Overall");
    overall.set_message("starting...");

    let mut file_bars: std::collections::HashMap<String, ProgressBar> =
        std::collections::HashMap::new();
    let (mut ok, mut err, mut skipped) = (0u32, 0u32, 0u32);

    for update in rx {
        match update {
            worker::ProgressUpdate::Started { total } => {
                overall.set_length(total as u64);
            }
            worker::ProgressUpdate::Processing { relative_path, .. } => {
                let fname = std::path::Path::new(&relative_path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| relative_path.clone());
                let pb = multi.insert_before(&overall, ProgressBar::new(0));
                pb.set_style(
                    ProgressStyle::with_template("    {prefix} [{bar:16.green/black}] {msg}")
                        .unwrap()
                        .progress_chars("█▉▊▋▌▍▎▏ "),
                );
                pb.set_prefix(fname.clone());
                pb.set_message("...");
                file_bars.insert(relative_path, pb);
            }
            worker::ProgressUpdate::PageProgress {
                relative_path,
                current_page,
                total_pages,
            } => {
                if let Some(pb) = file_bars.get(&relative_path) {
                    pb.set_length(total_pages as u64);
                    pb.set_position(current_page as u64);
                    pb.set_message(format!("page {current_page}/{total_pages}"));
                }
            }
            worker::ProgressUpdate::Completed {
                relative_path,
                page_count,
                ..
            } => {
                if let Some(pb) = file_bars.remove(&relative_path) {
                    pb.finish_with_message(format!("✓ {page_count} pages"));
                    pb.reset();
                }
                ok += 1;
                overall.inc(1);
                overall.set_message(format!("{ok} done, {err} errors, {skipped} skipped"));
            }
            worker::ProgressUpdate::Skipped { relative_path, .. } => {
                file_bars.remove(&relative_path);
                skipped += 1;
                overall.inc(1);
                overall.set_message(format!("{ok} done, {err} errors, {skipped} skipped"));
                let _ = relative_path;
            }
            worker::ProgressUpdate::Error {
                relative_path,
                error_message,
                ..
            } => {
                let fname = std::path::Path::new(&relative_path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| relative_path.clone());
                if let Some(pb) = file_bars.remove(&relative_path) {
                    pb.finish_with_message(style("✗ error").red().to_string());
                }
                multi
                    .println(format!(
                        "  {} {}: {}",
                        style("✗").red(),
                        fname,
                        error_message
                    ))
                    .ok();
                err += 1;
                overall.inc(1);
                overall.set_message(format!("{ok} done, {err} errors, {skipped} skipped"));
            }
            worker::ProgressUpdate::Log { .. } => {}
            worker::ProgressUpdate::Finished { .. } => {
                overall.finish_with_message("complete");
            }
        }
    }

    // Clear remaining per-file bars.
    for (_, pb) in file_bars.drain() {
        pb.finish_and_clear();
    }
    multi.clear().ok();

    // ── Results summary ────────────────────────────────────
    println!();
    println!("  {}", style("Done!").bold().green());
    println!("    {} converted", style(ok).green());
    if skipped > 0 {
        println!("    {} already up to date (skipped)", style(skipped).dim());
    }
    if err > 0 {
        println!("    {} had errors — see messages above", style(err).red());
    }
    println!();
    println!(
        "    Your images are in: {}",
        style(config.output_path.display()).cyan()
    );
    println!();

    // ── Size comparison table ───────────────────────────────
    if ok > 0 {
        println!("  {}", style("Size comparison:").bold().dim());
        println!(
            "  {:<50} {:>10} {:>10} {:>10}",
            style("File").dim(),
            style("PDF").dim(),
            style("WebP").dim(),
            style("Saved").dim()
        );
        println!("  {}", style("─".repeat(82)).dim());

        let mut total_pdf: u64 = 0;
        let mut total_webp: u64 = 0;

        for (rel_path, pdf_size) in &file_sizes {
            let doc_dir =
                shard::mirror::mirror_doc_dir(&config.output_path, std::path::Path::new(rel_path));
            let webp_size: u64 = std::fs::read_dir(&doc_dir)
                .ok()
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().is_some_and(|ext| ext == "webp"))
                        .filter_map(|e| e.metadata().ok())
                        .map(|m| m.len())
                        .sum()
                })
                .unwrap_or(0);

            // Shorten the filename if needed.
            let label = if rel_path.len() > 47 {
                format!("…{}", &rel_path[rel_path.len().saturating_sub(46)..])
            } else {
                rel_path.clone()
            };

            let saved = if *pdf_size > 0 {
                (1.0 - webp_size as f64 / *pdf_size as f64) * 100.0
            } else {
                0.0
            };

            println!(
                "  {:<50} {:>8} {:>8} {:>7.1}%",
                style(label).dim(),
                format_size(*pdf_size),
                format_size(webp_size),
                saved
            );

            total_pdf += pdf_size;
            total_webp += webp_size;
        }

        let total_saved = if total_pdf > 0 {
            (1.0 - total_webp as f64 / total_pdf as f64) * 100.0
        } else {
            0.0
        };
        println!("  {}", style("─".repeat(82)).dim());
        println!(
            "  {:<50} {:>8} {:>8} {:>7.1}%",
            style("Total").bold(),
            format_size(total_pdf),
            format_size(total_webp),
            total_saved
        );
        println!();
    }

    Ok(())
}

/// Format bytes as a human-readable string (KB / MB).
fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
