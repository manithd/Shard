use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::Sender;

use image::RgbaImage;

use crate::error::AppError;
use crate::worker::ProgressUpdate;

/// Data for a single rendered PDF page.
#[derive(Debug, Clone)]
pub struct PageData {
    pub page_num: u32,
    pub rgba_data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Trait for PDF rendering backends.
pub trait PdfRenderer: Send + Sync {
    fn render_pdf(&self, pdf_path: &Path, dpi: u32) -> Result<Vec<PageData>, AppError>;
}

/// PDF renderer using pdftoppm (poppler-utils).
pub struct PopplerRenderer;

impl PdfRenderer for PopplerRenderer {
    fn render_pdf(&self, pdf_path: &Path, dpi: u32) -> Result<Vec<PageData>, AppError> {
        let temp_dir = tempfile::TempDir::new().map_err(|e| AppError::Io(e))?;
        let temp_path = temp_dir.path().to_path_buf();
        let output_prefix = temp_path.join("page");

        let mut child = std::process::Command::new("pdftoppm")
            .arg("-png")
            .arg("-r")
            .arg(dpi.to_string())
            .arg(pdf_path)
            .arg(&output_prefix)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                AppError::Pdf(format!(
                    "Failed to execute pdftoppm. Is poppler-utils installed? Error: {e}"
                ))
            })?;

        let mut stderr_buf = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_buf);
        }
        let status = child
            .wait()
            .map_err(|e| AppError::Pdf(format!("Failed to wait for pdftoppm: {e}")))?;

        if !status.success() {
            let detail = if stderr_buf.trim().is_empty() {
                "no error output".into()
            } else {
                stderr_buf.trim().to_string()
            };
            return Err(AppError::Pdf(format!(
                "pdftoppm failed for '{}': {detail}",
                pdf_path.display()
            )));
        }

        let mut entries: Vec<_> = fs::read_dir(&temp_path)
            .map_err(AppError::Io)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().is_some_and(|ext| ext == "png")
                    && e.path()
                        .file_stem()
                        .is_some_and(|s| s.to_string_lossy().starts_with("page"))
            })
            .collect();
        entries.sort_by_key(|e| e.path());

        if entries.is_empty() {
            return Err(AppError::Pdf(format!(
                "No pages rendered from '{}'.",
                pdf_path.display()
            )));
        }

        let mut pages: Vec<PageData> = Vec::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            let path = entry.path();
            let img = image::open(&path).map_err(|e| {
                AppError::Pdf(format!("Failed to load page {}: {e}", path.display()))
            })?;
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            pages.push(PageData {
                page_num: (i + 1) as u32,
                rgba_data: rgba.into_raw(),
                width: w,
                height: h,
            });
        }
        Ok(pages)
    }
}

/// PDF renderer using pdfium (feature-gated).
#[cfg(feature = "pdfium")]
pub struct PdfiumRenderer {
    pdfium: pdfium_render::Pdfium,
}

#[cfg(feature = "pdfium")]
impl PdfiumRenderer {
    pub fn new() -> Result<Self, AppError> {
        let pdfium = pdfium_render::Pdfium::bind_to_library(pdfium_render::Pdfium::library_path())
            .or_else(|_| Err(pdfium_render::Pdfium::Error::LibraryNotFound))
            .map_err(|e| AppError::Pdf(format!("Failed to load pdfium: {e}")))?;
        Ok(Self { pdfium })
    }
}

#[cfg(feature = "pdfium")]
impl PdfRenderer for PdfiumRenderer {
    fn render_pdf(&self, pdf_path: &Path, dpi: u32) -> Result<Vec<PageData>, AppError> {
        let doc = self
            .pdfium
            .load_pdf_from_file(pdf_path, None)
            .map_err(|e| AppError::Pdf(format!("Failed to load PDF: {e}")))?;
        let mut pages = Vec::with_capacity(doc.pages().len());
        for i in 0..doc.pages().len() {
            let page = doc
                .pages()
                .get(i)
                .map_err(|e| AppError::Pdf(format!("Page {i}: {e}")))?;
            let cfg = page.render_config();
            let bmp = page
                .render_with_config(&cfg.set_dpi(dpi.into()))
                .map_err(|e| AppError::Pdf(format!("Render page {i}: {e}")))?;
            pages.push(PageData {
                page_num: (i + 1) as u32,
                rgba_data: bmp.as_bytes().to_vec(),
                width: bmp.width() as u32,
                height: bmp.height() as u32,
            });
        }
        Ok(pages)
    }
}

// ── Basic WebP encoding (non-adaptive fallback) ─────────────────

/// Encode a page as WebP with a fixed quality.
pub fn encode_page_to_webp(
    page: &PageData,
    output_path: &Path,
    quality: u8,
) -> Result<(), AppError> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(AppError::Io)?;
    }
    let img = RgbaImage::from_raw(page.width, page.height, page.rgba_data.clone())
        .ok_or_else(|| AppError::Webp(format!("Bad buffer {}x{}", page.width, page.height)))?;
    let raw = img.into_raw();
    let encoder = webp::Encoder::from_rgba(&raw, page.width, page.height);
    let mem = encoder.encode(quality as f32);
    let tmp = output_path.with_extension("webp.tmp");
    fs::write(&tmp, &mem[..]).map_err(AppError::Io)?;
    fs::rename(&tmp, output_path).map_err(AppError::Io)?;
    Ok(())
}

// ── Adaptive encoding with SSIMULACRA2 ──────────────────────────

/// Encode at a specific quality using the default encoder.
fn encode_at_quality(page: &PageData, quality: f32) -> Result<Vec<u8>, AppError> {
    let img = RgbaImage::from_raw(page.width, page.height, page.rgba_data.clone())
        .ok_or_else(|| AppError::Webp("Bad image buffer".into()))?;
    let raw = img.into_raw();
    let encoder = webp::Encoder::from_rgba(&raw, page.width, page.height);
    Ok(encoder.encode(quality).to_vec())
}

/// Decode WebP bytes back to RGBA for quality comparison.
fn decode_webp_to_rgba(data: &[u8]) -> Result<Vec<u8>, AppError> {
    use webp::Decoder;
    let decoder = Decoder::new(data);
    let decoded = decoder
        .decode()
        .ok_or_else(|| AppError::Webp("Failed to decode WebP for verification".into()))?;
    Ok(decoded.to_image().to_rgba8().into_raw())
}

/// Compute SSIMULACRA2 score between original and decoded RGBA.
fn compute_quality_score(orig: &[u8], decoded: &[u8], w: u32, h: u32) -> Result<f64, AppError> {
    use ssimulacra2::{compute_frame_ssimulacra2, LinearRgb};

    let make = |rgba: &[u8]| -> Result<LinearRgb, AppError> {
        let data: Vec<[f32; 3]> = rgba
            .chunks_exact(4)
            .map(|p| {
                [
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ]
            })
            .collect();
        LinearRgb::new(data, w as usize, h as usize)
            .map_err(|e| AppError::Webp(format!("LinearRgb creation error: {e:?}")))
    };

    let src = make(orig)?;
    let dst = make(decoded)?;
    compute_frame_ssimulacra2(src, dst)
        .map_err(|e| AppError::Webp(format!("SSIMULACRA2 error: {e:?}")))
}

/// Decode and compute score in one step (avoid duplicate decode).
fn score_encoded(page: &PageData, encoded: &[u8]) -> Result<f64, AppError> {
    let decoded = decode_webp_to_rgba(encoded)?;
    compute_quality_score(&page.rgba_data, &decoded, page.width, page.height)
}

/// Binary-search the minimum WebP quality meeting the SSIMULACRA2 target.
/// Uses the default encoder (no advanced config tuning).
pub fn encode_page_adaptive(
    page: &PageData,
    output_path: &Path,
    target_score: f64,
    min_quality: f32,
    max_quality: f32,
) -> Result<EncodeStats, AppError> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(AppError::Io)?;
    }

    // Binary search over quality
    let mut lo = min_quality;
    let mut hi = max_quality;
    let mut best_quality = max_quality;

    for _ in 0..6 {
        let mid = (lo + hi) / 2.0;
        if (hi - lo) < 2.0 {
            break;
        }
        let encoded = encode_at_quality(page, mid)?;
        let score = score_encoded(page, &encoded)?;

        if score >= target_score {
            best_quality = mid;
            hi = mid;
        } else {
            lo = mid;
        }
    }

    // Final encode at best quality (re-encode to ensure optimal)
    let final_bytes = encode_at_quality(page, best_quality)?;
    let final_score = score_encoded(page, &final_bytes).unwrap_or(0.0);

    // Write output (atomic temp → rename)
    let tmp = output_path.with_extension("webp.tmp");
    fs::write(&tmp, &final_bytes).map_err(AppError::Io)?;
    fs::rename(&tmp, output_path).map_err(AppError::Io)?;

    Ok(EncodeStats {
        quality_used: best_quality,
        ssimulacra2_score: final_score,
        file_size: final_bytes.len(),
    })
}

#[derive(Debug, Clone)]
pub struct EncodeStats {
    pub quality_used: f32,
    pub ssimulacra2_score: f64,
    pub file_size: usize,
}

// ── Main WebP conversion dispatcher ─────────────────────────────

/// Convert a PDF file to WebP images, optionally using adaptive encoding.
pub fn convert_pdf_to_webp(
    renderer: &dyn PdfRenderer,
    pdf_path: &Path,
    output_dir: &Path,
    relative_path: &str,
    dpi: u32,
    quality: u8,
    adaptive: bool,
    quality_target: f64,
    overwrite: bool,
    progress_tx: Option<&Sender<ProgressUpdate>>,
) -> Result<u32, AppError> {
    fs::create_dir_all(output_dir).map_err(AppError::Io)?;
    let pages = renderer.render_pdf(pdf_path, dpi)?;
    let page_count = pages.len() as u32;

    for page in &pages {
        let output_path = output_dir.join(format!("page-{:03}.webp", page.page_num));
        if output_path.exists() && !overwrite {
            continue;
        }

        if adaptive {
            let stats = encode_page_adaptive(page, &output_path, quality_target, 40.0, 95.0)?;
            if let Some(tx) = progress_tx {
                let _ = tx.send(ProgressUpdate::Log {
                    message: format!(
                        "  page {}: q={:.0} ssim2={:.1} ({} KB)",
                        page.page_num,
                        stats.quality_used,
                        stats.ssimulacra2_score,
                        stats.file_size / 1024
                    ),
                });
            }
        } else {
            encode_page_to_webp(page, &output_path, quality)?;
        }

        // Emit page progress after each page is written
        if let Some(tx) = progress_tx {
            let _ = tx.send(ProgressUpdate::PageProgress {
                relative_path: relative_path.to_string(),
                current_page: page.page_num,
                total_pages: page_count,
            });
        }
    }

    Ok(page_count)
}

/// Get page count via pdfinfo.
pub fn get_pdf_page_count(pdf_path: &Path) -> Result<u32, AppError> {
    let out = std::process::Command::new("pdfinfo")
        .arg(pdf_path)
        .output()
        .map_err(|e| AppError::Pdf(format!("pdfinfo failed: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Pages:") {
            return rest
                .trim()
                .parse::<u32>()
                .map_err(|_| AppError::Pdf("Failed to parse page count".into()));
        }
    }
    Err(AppError::Pdf("Could not determine page count".into()))
}

/// Convert a PDF file to per-page SVG files using pdftocairo + usvg.
pub fn convert_pdf_to_svg(
    pdf_path: &Path,
    output_dir: &Path,
    svg_precision: u8,
    _svg_convert_text_to_paths: bool,
    svg_strip_background: bool,
    overwrite: bool,
) -> Result<u32, AppError> {
    fs::create_dir_all(output_dir).map_err(AppError::Io)?;
    let page_count = get_pdf_page_count(pdf_path)?;

    for page_num in 1..=page_count {
        let output_path = output_dir.join(format!("page-{page_num:03}.svg"));
        if output_path.exists() && !overwrite {
            continue;
        }

        let temp_svg = output_path.with_extension("svg.raw");
        let status = std::process::Command::new("pdftocairo")
            .arg("-svg")
            .arg("-f")
            .arg(page_num.to_string())
            .arg("-l")
            .arg(page_num.to_string())
            .arg(pdf_path)
            .arg(&temp_svg)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .map_err(|e| AppError::Pdf(format!("pdftocairo failed: {e}")))?;

        if !status.success() {
            return Err(AppError::Pdf(format!(
                "pdftocairo failed for '{}' page {}",
                pdf_path.display(),
                page_num
            )));
        }

        let svg_data = fs::read(&temp_svg).map_err(AppError::Io)?;
        let processed = if svg_strip_background {
            strip_background_rect_bytes(&svg_data)
        } else {
            svg_data
        };

        let tree = usvg::Tree::from_data(&processed, &usvg::Options::default())
            .map_err(|e| AppError::Custom(format!("usvg error: {e}")))?;

        let precision = svg_precision.clamp(1, 4);
        let xml = tree.to_string(&usvg::WriteOptions {
            coordinates_precision: precision,
            ..usvg::WriteOptions::default()
        });
        fs::write(&output_path, xml).map_err(AppError::Io)?;
        let _ = fs::remove_file(&temp_svg);
    }
    Ok(page_count)
}

// ── Background removal helper ───────────────────────────────────

fn strip_background_rect_bytes(svg_data: &[u8]) -> Vec<u8> {
    let s = match std::str::from_utf8(svg_data) {
        Ok(s) => s,
        Err(_) => return svg_data.to_vec(),
    };
    let doc = match roxmltree::Document::parse(s) {
        Ok(d) => d,
        Err(_) => return svg_data.to_vec(),
    };
    let svg_node = match doc.root().descendants().find(|n| n.has_tag_name("svg")) {
        Some(n) => n,
        None => return svg_data.to_vec(),
    };

    let (page_w, page_h) = parse_view_box(svg_node);
    if page_w == 0.0 || page_h == 0.0 {
        return svg_data.to_vec();
    }

    let root_children: Vec<_> = svg_node.children().collect();
    let first_g = root_children.iter().find(|n| n.has_tag_name("g")).cloned();
    let candidates: Vec<_> = if let Some(g) = first_g {
        g.children().filter(|n| n.has_tag_name("rect")).collect()
    } else {
        root_children
            .iter()
            .filter(|n| n.has_tag_name("rect"))
            .cloned()
            .collect()
    };

    for rect in &candidates {
        let x = attr_float(*rect, "x").unwrap_or(0.0);
        let y = attr_float(*rect, "y").unwrap_or(0.0);
        let w = attr_float(*rect, "width").unwrap_or(0.0);
        let h = attr_float(*rect, "height").unwrap_or(0.0);
        let fill = rect.attribute("fill").unwrap_or_default().to_string();

        let covers = w >= page_w * 0.98 && h >= page_h * 0.98;
        let origin = x <= 2.0 && y <= 2.0;
        let white = fill.eq_ignore_ascii_case("white")
            || fill.eq_ignore_ascii_case("#fff")
            || fill.eq_ignore_ascii_case("#ffffff")
            || fill.eq_ignore_ascii_case("rgb(255,255,255)");

        if covers && origin && white {
            let range = rect.range();
            let mut result = Vec::with_capacity(svg_data.len() - range.len());
            result.extend_from_slice(&svg_data[..range.start]);
            result.extend_from_slice(&svg_data[range.end..]);
            return result;
        }
    }
    svg_data.to_vec()
}

fn parse_view_box(node: roxmltree::Node) -> (f32, f32) {
    let vb = node.attribute("viewBox").map(|s| s.to_string());
    if let Some(vb_str) = vb {
        let parts: Vec<f32> = vb_str
            .split_whitespace()
            .filter_map(|s| s.parse::<f32>().ok())
            .collect();
        if parts.len() >= 4 {
            return (parts[2], parts[3]);
        }
    }
    let w = node
        .attribute("width")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(612.0);
    let h = node
        .attribute("height")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(792.0);
    (w, h)
}

fn attr_float(node: roxmltree::Node, name: &str) -> Option<f32> {
    node.attribute(name).and_then(|v| v.parse::<f32>().ok())
}

/// Dispatch to format-specific converter.
pub fn convert_pdf(
    renderer: &dyn PdfRenderer,
    pdf_path: &Path,
    output_dir: &Path,
    relative_path: &str,
    format: crate::config::OutputFormat,
    dpi: u32,
    quality: u8,
    adaptive: bool,
    quality_target: f64,
    svg_precision: u8,
    svg_no_text: bool,
    svg_strip_bg: bool,
    overwrite: bool,
    progress_tx: Option<&Sender<ProgressUpdate>>,
) -> Result<u32, AppError> {
    match format {
        crate::config::OutputFormat::Webp => convert_pdf_to_webp(
            renderer,
            pdf_path,
            output_dir,
            relative_path,
            dpi,
            quality,
            adaptive,
            quality_target,
            overwrite,
            progress_tx,
        ),
        crate::config::OutputFormat::Svg => convert_pdf_to_svg(
            pdf_path,
            output_dir,
            svg_precision,
            svg_no_text,
            svg_strip_bg,
            overwrite,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    #[test]
    fn test_page_format() {
        assert_eq!(
            PathBuf::from("/out/page-001.webp"),
            PathBuf::from("/out").join(format!("page-{:03}.webp", 1))
        );
        assert_eq!(
            PathBuf::from("/out/page-042.webp"),
            PathBuf::from("/out").join(format!("page-{:03}.webp", 42))
        );
    }
}
