use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use rayon::prelude::*;

use crate::config::OutputFormat;
use crate::converter::{self, PdfRenderer};
use crate::error::AppError;
use crate::mirror::{self, ScannedFile};

#[derive(Debug, Clone)]
pub enum ProgressUpdate {
    Started {
        total: usize,
    },
    Processing {
        index: usize,
        total: usize,
        relative_path: String,
    },
    PageProgress {
        relative_path: String,
        current_page: u32,
        total_pages: u32,
    },
    Completed {
        index: usize,
        relative_path: String,
        page_count: u32,
    },
    Skipped {
        index: usize,
        relative_path: String,
    },
    Error {
        index: usize,
        relative_path: String,
        error_message: String,
    },
    Finished {
        success_count: u32,
        error_count: u32,
        skipped_count: u32,
    },
    Log {
        message: String,
    },
}

fn estimate_required_bytes(file_count: usize, avg_pages_per_pdf: u32, dpi: u32) -> u64 {
    let scale_factor = (dpi as f64 / 72.0).powi(2);
    let bytes_per_page = (scale_factor * 500_000.0) as u64;
    let total_pages = file_count as u64 * avg_pages_per_pdf as u64;
    let margin = 20;
    (total_pages * bytes_per_page * (100 + margin)) / 100
}

pub fn check_disk_space(
    output_path: &Path,
    file_count: usize,
    avg_pages: u32,
    dpi: u32,
) -> Result<(), AppError> {
    let estimated = estimate_required_bytes(file_count, avg_pages, dpi);

    #[cfg(unix)]
    {
        use fs2::available_space;
        match available_space(output_path) {
            Ok(available) if available < estimated => {
                return Err(AppError::InsufficientDiskSpace {
                    need: estimated,
                    available,
                    path: output_path.to_path_buf(),
                });
            }
            _ => {}
        }
    }

    Ok(())
}

pub fn start_conversion(
    files: Vec<ScannedFile>,
    output_root: PathBuf,
    renderer: Arc<dyn PdfRenderer>,
    format: OutputFormat,
    dpi: u32,
    quality: u8,
    adaptive_encoding: bool,
    quality_target: f64,
    svg_precision: u8,
    svg_no_text: bool,
    svg_strip_background: bool,
    overwrite: bool,
    cancel_flag: Arc<AtomicBool>,
) -> Receiver<ProgressUpdate> {
    let (tx, rx) = mpsc::channel();
    let total = files.len();

    let _ = tx.send(ProgressUpdate::Started { total });

    std::thread::Builder::new()
        .name("conversion-worker".into())
        .spawn(move || {
            let results = process_files_in_parallel(
                &files,
                &output_root,
                &*renderer,
                format,
                dpi,
                quality,
                adaptive_encoding,
                quality_target,
                svg_precision,
                svg_no_text,
                svg_strip_background,
                overwrite,
                &cancel_flag,
                &tx,
            );

            let _ = tx.send(ProgressUpdate::Finished {
                success_count: results.0,
                error_count: results.1,
                skipped_count: results.2,
            });
        })
        .expect("Failed to spawn conversion worker thread");

    rx
}

#[allow(clippy::too_many_arguments)]
fn process_files_in_parallel(
    files: &[ScannedFile],
    output_root: &Path,
    renderer: &dyn PdfRenderer,
    format: OutputFormat,
    dpi: u32,
    quality: u8,
    adaptive_encoding: bool,
    quality_target: f64,
    svg_precision: u8,
    svg_no_text: bool,
    svg_strip_background: bool,
    overwrite: bool,
    cancel_flag: &AtomicBool,
    tx: &Sender<ProgressUpdate>,
) -> (u32, u32, u32) {
    use std::sync::Mutex;

    let success_count = Arc::new(Mutex::new(0u32));
    let error_count = Arc::new(Mutex::new(0u32));
    let skipped_count = Arc::new(Mutex::new(0u32));

    files.par_iter().enumerate().for_each(|(index, file)| {
        if cancel_flag.load(Ordering::Relaxed) {
            return;
        }

        let relative_path_str = file.relative_path.display().to_string();

        let _ = tx.send(ProgressUpdate::Processing {
            index,
            total: files.len(),
            relative_path: relative_path_str.clone(),
        });

        let doc_dir = mirror::mirror_doc_dir(output_root, &file.relative_path);

        // Skip if already converted
        if !overwrite && mirror::is_converted(output_root, &file.relative_path) {
            let _ = tx.send(ProgressUpdate::Skipped {
                index,
                relative_path: relative_path_str.clone(),
            });
            *skipped_count.lock().unwrap() += 1;
            return;
        }

        let _ = tx.send(ProgressUpdate::Log {
            message: format!("⟳ {relative_path_str}"),
        });

        // Dispatch based on format
        let result = converter::convert_pdf(
            renderer,
            &file.full_path,
            &doc_dir,
            &relative_path_str,
            format,
            dpi,
            quality,
            adaptive_encoding,
            quality_target,
            svg_precision,
            svg_no_text,
            svg_strip_background,
            overwrite,
            Some(tx),
        );

        match result {
            Ok(page_count) => {
                let _ = tx.send(ProgressUpdate::Completed {
                    index,
                    relative_path: relative_path_str.clone(),
                    page_count,
                });
                let _ = tx.send(ProgressUpdate::Log {
                    message: format!("✓ {relative_path_str} ({page_count} pages)"),
                });
                *success_count.lock().unwrap() += 1;
            }
            Err(e) => {
                let error_message = e.to_string();
                let _ = tx.send(ProgressUpdate::Error {
                    index,
                    relative_path: relative_path_str.clone(),
                    error_message: error_message.clone(),
                });
                let _ = tx.send(ProgressUpdate::Log {
                    message: format!("✗ {relative_path_str}: {error_message}"),
                });
                *error_count.lock().unwrap() += 1;

                let error_log_path = output_root.join("logs").join("errors.txt");
                if let Err(log_err) =
                    append_error_log(&error_log_path, &relative_path_str, &error_message)
                {
                    log::error!("Failed to write error log: {log_err}");
                }
            }
        }
    });

    let success = *success_count.lock().unwrap();
    let errors = *error_count.lock().unwrap();
    let skipped = *skipped_count.lock().unwrap();

    (success, errors, skipped)
}

fn append_error_log(log_path: &Path, relative_path: &str, error: &str) -> Result<(), AppError> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(AppError::Io)?;
    }

    let entry = format!("[{}] {}: {}\n", chrono_now(), relative_path, error);

    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|file| {
            use std::io::Write;
            let mut file = file;
            file.write_all(entry.as_bytes())
        })
        .map_err(AppError::Io)?;

    Ok(())
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    let days_since_epoch = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    let (year, month, day) = days_to_date(days_since_epoch as i64);

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hours, minutes, seconds
    )
}

fn days_to_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_days_to_date() {
        assert_eq!(days_to_date(0), (1970, 1, 1));
        assert_eq!(days_to_date(19723), (2024, 1, 1));
        assert_eq!(days_to_date(19889), (2024, 6, 15));
    }

    #[test]
    fn test_estimate_bytes() {
        let est = estimate_required_bytes(100, 10, 150);
        assert!(est > 0);
        assert!(est < u64::MAX);
    }
}
