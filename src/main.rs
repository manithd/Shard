//! PDF → WebP Converter (CLI)
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

use pdf2webp::config::{AppConfig, OutputFormat};
use pdf2webp::converter::{PdfRenderer, PopplerRenderer};
use pdf2webp::mirror;
use pdf2webp::worker;

/// Convert a folder of PDFs into optimized WebP images.
///
/// Run with no options for a step-by-step guided setup.
#[derive(Parser, Debug)]
#[command(name = "pdf2webp", version, about, long_about = None)]
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

    /// Skip the interactive TUI and use the plain CLI wizard instead
    #[arg(long)]
    no_tui: bool,

    /// Skip the interactive confirmation and run immediately
    #[arg(short = 'y', long)]
    yes: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let cli = Cli::parse();

    if !cli.no_tui && (atty::is(atty::Stream::Stdout) || atty::is(atty::Stream::Stdin)) {
        // TUI mode (default when running interactively).
        run_tui_mode(cli)
    } else {
        // Plain CLI mode (for scripts, CI, or --no-tui).
        run_cli_mode(cli)
    }
}

/// Launch the interactive TUI.
fn run_tui_mode(cli: Cli) -> anyhow::Result<()> {
    let mut config = pdf2webp::config::AppConfig::default();

    // If source was given via CLI flags, scan now so the TUI starts with files.
    if let Some(ref src) = cli.source {
        config.source_path = src.clone();
    }
    if let Some(ref out) = cli.output {
        config.output_path = out.clone();
    }
    if cli.overwrite {
        config.overwrite = true;
    }

    let mut state = pdf2webp::tui::TuiState::new(config);

    // If source is set, scan and populate the file list immediately.
    if !state.config.source_path.as_os_str().is_empty() {
        match pdf2webp::mirror::scan_pdf_files(&state.config.source_path) {
            Ok(files) => {
                state.set_files(files);
            }
            Err(e) => {
                state.log_lines.push(format!("Scan error: {e}"));
            }
        }
    }

    pdf2webp::tui::run_tui(state)
}

/// Plain CLI wizard (same as the previous implementation).
fn run_cli_mode(cli: Cli) -> anyhow::Result<()> {
    println!();
    println!(
        "  {} {}",
        style("PDF → WebP Converter").bold().cyan(),
        style(env!("CARGO_PKG_VERSION")).dim()
    );
    println!("  {}", style("─────────────────────").cyan());
    println!();

    let config = if cli.source.is_some() && cli.output.is_some() {
        build_config_from_args(&cli)
    } else {
        run_wizard(&cli)?
    };
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
        println!("    Files: {} PDF(s) found", total);
        println!("    DPI:   {}", config.dpi);
        println!();
        let proceed = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("  Start conversion?")
            .default(true)
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
        "    {} Found {} PDF file(s).",
        style("✓").green(),
        scanned.len()
    );
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
            "Balanced (recommended) — sharp on screens, smaller files",
            "Maximum sharpness — largest files, best for zooming in",
            "Smallest files — good for quick previews, slightly softer",
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
        _ => unreachable!(),
    }
    println!();

    config.overwrite = cli.overwrite;
    Ok(config)
}

fn run_conversion(
    config: AppConfig,
    scanned: Vec<pdf2webp::mirror::ScannedFile>,
) -> anyhow::Result<()> {
    let total = scanned.len();

    // Create renderer.
    #[cfg(feature = "pdfium")]
    let renderer: Arc<dyn PdfRenderer> = match pdf2webp::converter::PdfiumRenderer::new() {
        Ok(r) => Arc::new(r),
        Err(_) => {
            println!(
                "  {} Pdfium not available, using poppler fallback.",
                style("!").yellow()
            );
            Arc::new(PopplerRenderer)
        }
    };
    #[cfg(not(feature = "pdfium"))]
    let renderer: Arc<dyn PdfRenderer> = Arc::new(PopplerRenderer);

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
                let pb = multi.insert_before(&overall, ProgressBar::new(0));
                pb.set_style(
                    ProgressStyle::with_template("    {prefix} [{bar:16.green/black}] {msg}")
                        .unwrap()
                        .progress_chars("█▉▊▋▌▍▎▏ "),
                );
                pb.set_prefix(relative_path.clone());
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
                if let Some(pb) = file_bars.remove(&relative_path) {
                    pb.finish_with_message(style("✗ error").red().to_string());
                }
                multi
                    .println(format!(
                        "  {} {}: {}",
                        style("✗").red(),
                        relative_path,
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

    Ok(())
}
