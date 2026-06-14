use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode};

use crate::config::{AppConfig, OutputFormat};
use crate::converter::{PdfRenderer, PopplerRenderer};
use crate::worker::{self, ProgressUpdate};
use crate::{FileItem, MainWindow};

/// The main application controller.
pub struct App {
    window: MainWindow,
    file_data: Arc<Mutex<Vec<FileItem>>>,
    cancel_flag: Arc<AtomicBool>,
    progress_rx: Arc<Mutex<Option<Receiver<ProgressUpdate>>>>,
    poll_timer: Timer,
}

fn refresh_model(window: &MainWindow, items: &[FileItem]) {
    window.set_file_model(ModelRc::from(items));
}

impl App {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let window = MainWindow::new()?;
        refresh_model(&window, &[]);
        Ok(Self {
            window,
            file_data: Arc::new(Mutex::new(Vec::new())),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            progress_rx: Arc::new(Mutex::new(None)),
            poll_timer: Timer::default(),
        })
    }

    pub fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.window.run()?;
        Ok(())
    }

    pub fn setup(&self) {
        self.setup_callbacks();
        self.setup_poller();
    }

    fn setup_callbacks(&self) {
        let w = self.window.as_weak();
        let fd = self.file_data.clone();
        let cf = self.cancel_flag.clone();
        let prx = self.progress_rx.clone();

        // Source folder
        let w1 = w.clone();
        self.window.on_select_source_folder(move || {
            if let Some(f) = rfd::FileDialog::new()
                .set_title("Select Source Folder")
                .pick_folder()
            {
                w1.upgrade().map(|win| {
                    win.set_source_path(SharedString::from(f.to_string_lossy().as_ref()));
                    win.set_status_text(SharedString::from("Source selected. Click Scan."));
                });
            }
        });

        // Output folder
        let w2 = w.clone();
        self.window.on_select_output_folder(move || {
            if let Some(f) = rfd::FileDialog::new()
                .set_title("Select Output Folder")
                .pick_folder()
            {
                w2.upgrade().map(|win| {
                    win.set_output_path(SharedString::from(f.to_string_lossy().as_ref()));
                    win.set_status_text(SharedString::from("Output set. Ready to scan."));
                });
            }
        });

        // Scan
        let w3 = w.clone();
        let fd3 = fd.clone();
        self.window.on_scan_source(move || {
            let win = match w3.upgrade() {
                Some(x) => x,
                None => return,
            };
            let src = PathBuf::from(win.get_source_path().as_str());
            win.set_scanning(true);
            win.set_status_text(SharedString::from("Scanning for PDFs…"));

            let w = w3.clone();
            let fd = fd3.clone();
            std::thread::spawn(move || {
                let result = crate::mirror::scan_pdf_files(&src);
                let _ = slint::invoke_from_event_loop(move || {
                    let win = match w.upgrade() {
                        Some(x) => x,
                        None => return,
                    };
                    match result {
                        Ok(files) => {
                            let n = files.len();
                            let items: Vec<FileItem> = files
                                .iter()
                                .map(|f| FileItem {
                                    relative_path: SharedString::from(
                                        f.relative_path.to_string_lossy().as_ref(),
                                    ),
                                    status: SharedString::from("pending"),
                                    selected: true,
                                    current_page: 0,
                                    total_pages: 0,
                                    error_message: SharedString::default(),
                                })
                                .collect();
                            *fd.lock().unwrap() = items.clone();
                            refresh_model(&win, &items);
                            win.set_total_files(n as i32);
                            win.set_selected_count(n as i32);
                            win.set_scanning(false);
                            win.set_status_text(SharedString::from(format!(
                                "Found {n} PDFs. Click Start to convert."
                            )));
                        }
                        Err(e) => {
                            win.set_scanning(false);
                            win.set_status_text(SharedString::from(format!("Scan error: {e}")));
                        }
                    }
                });
            });
        });

        // Start conversion
        let w4 = w.clone();
        let cf4 = cf.clone();
        let prx4 = prx.clone();
        self.window.on_start_conversion(move || {
            let win = match w4.upgrade() {
                Some(x) => x,
                None => return,
            };

            let cfg = AppConfig {
                source_path: PathBuf::from(win.get_source_path().as_str()),
                output_path: PathBuf::from(win.get_output_path().as_str()),
                format: if win.get_output_format_index() == 1 {
                    OutputFormat::Svg
                } else {
                    OutputFormat::Webp
                },
                dpi: win.get_dpi_value() as u32,
                quality: win.get_quality_value() as u8,
                adaptive_encoding: win.get_adaptive_encoding(),
                quality_target: win.get_quality_target() as f64,
                svg_precision: win.get_svg_precision() as u8,
                svg_no_text: win.get_svg_embed_fonts_as_paths(),
                svg_strip_background: win.get_svg_strip_background(),
                overwrite: win.get_overwrite(),
            };

            let errs = cfg.validate();
            if !errs.is_empty() {
                win.set_status_text(SharedString::from(format!(
                    "Config error: {}",
                    errs.join(", ")
                )));
                return;
            }

            let files = match crate::mirror::scan_pdf_files(&cfg.source_path) {
                Ok(f) => f,
                Err(e) => {
                    win.set_status_text(SharedString::from(format!("Scan error: {e}")));
                    return;
                }
            };

            let total = files.len();
            if total == 0 {
                win.set_status_text(SharedString::from("No files"));
                return;
            }

            if let Err(e) = worker::check_disk_space(&cfg.output_path, total, 10, cfg.dpi) {
                win.set_status_text(SharedString::from(format!("Disk space: {e}")));
                return;
            }

            cf4.store(false, Ordering::Relaxed);
            win.set_show_dialog(false);
            win.set_processing(true);
            win.set_processed_count(0);
            win.set_error_count(0);
            win.set_skipped_count(0);
            win.set_status_text(SharedString::from(format!(
                "Processing {total} files at {} DPI…",
                cfg.dpi
            )));

            #[cfg(feature = "pdfium")]
            let renderer: Arc<dyn PdfRenderer> = match crate::converter::PdfiumRenderer::new() {
                Ok(r) => Arc::new(r),
                Err(e) => Arc::new(PopplerRenderer),
            };
            #[cfg(not(feature = "pdfium"))]
            let renderer: Arc<dyn PdfRenderer> = Arc::new(PopplerRenderer);

            let rx = worker::start_conversion(
                files,
                cfg.output_path,
                renderer,
                cfg.format,
                cfg.dpi,
                cfg.quality,
                cfg.adaptive_encoding,
                cfg.quality_target,
                cfg.svg_precision,
                cfg.svg_no_text,
                cfg.svg_strip_background,
                cfg.overwrite,
                cf4.clone(),
            );
            *prx4.lock().unwrap() = Some(rx);
        });

        // Stop
        let cf5 = cf.clone();
        self.window.on_stop_conversion(move || {
            cf5.store(true, Ordering::Relaxed);
        });

        // Toggle file
        let w6 = w.clone();
        let fd6 = fd.clone();
        self.window.on_toggle_file(move |idx: i32| {
            let win = match w6.upgrade() {
                Some(x) => x,
                None => return,
            };
            let i = idx as usize;
            let mut guard = fd6.lock().unwrap();
            if i < guard.len() {
                guard[i].selected = !guard[i].selected;
                refresh_model(&win, &guard);
                win.set_selected_count(guard.iter().filter(|x| x.selected).count() as i32);
            }
        });

        // Toggle all
        let w7 = w.clone();
        let fd7 = fd.clone();
        self.window.on_toggle_all_files(move |sel: bool| {
            let win = match w7.upgrade() {
                Some(x) => x,
                None => return,
            };
            let mut guard = fd7.lock().unwrap();
            for item in guard.iter_mut() {
                item.selected = sel;
            }
            refresh_model(&win, &guard);
            win.set_selected_count(guard.iter().filter(|x| x.selected).count() as i32);
        });
    }

    fn setup_poller(&self) {
        let w = self.window.as_weak();
        let fd = self.file_data.clone();
        let cf = self.cancel_flag.clone();
        let prx = self.progress_rx.clone();

        self.poll_timer
            .start(TimerMode::Repeated, Duration::from_millis(100), move || {
                let win = match w.upgrade() {
                    Some(x) => x,
                    None => return,
                };
                let mut rx_guard = prx.lock().unwrap();
                let rx = match rx_guard.as_mut() {
                    Some(rx) => rx,
                    None => return,
                };

                let mut finished = false;
                let mut model_dirty = false;

                while let Ok(update) = rx.try_recv() {
                    match update {
                        ProgressUpdate::Started { total } => {
                            win.set_total_files(total as i32);
                            win.set_status_text(SharedString::from(format!(
                                "Processing 0/{total}…"
                            )));
                        }
                        ProgressUpdate::Processing { relative_path, .. } => {
                            let mut guard = fd.lock().unwrap();
                            if let Some(item) = guard
                                .iter_mut()
                                .find(|i| i.relative_path.as_str() == relative_path)
                            {
                                item.status = SharedString::from("processing");
                                item.current_page = 0;
                                item.total_pages = 0;
                                model_dirty = true;
                            }
                            let done = win.get_processed_count();
                            let total = win.get_total_files();
                            let pct = if total > 0 { (done * 100) / total } else { 0 };
                            win.set_status_text(SharedString::from(format!(
                                "Processing: {relative_path}  {done}/{total}  {pct}%"
                            )));
                        }
                        ProgressUpdate::PageProgress {
                            relative_path,
                            current_page,
                            total_pages,
                        } => {
                            let mut guard = fd.lock().unwrap();
                            if let Some(item) = guard
                                .iter_mut()
                                .find(|i| i.relative_path.as_str() == relative_path)
                            {
                                item.current_page = current_page as i32;
                                item.total_pages = total_pages as i32;
                                model_dirty = true;
                            }
                            let done = win.get_processed_count();
                            let total = win.get_total_files();
                            let pct = if total > 0 { (done * 100) / total } else { 0 };
                            win.set_status_text(SharedString::from(format!(
                                "Processing: {relative_path}, Converted page {current_page}/{total_pages}  {done}/{total}  {pct}%"
                            )));
                        }
                        ProgressUpdate::Completed { relative_path, .. } => {
                            let mut guard = fd.lock().unwrap();
                            if let Some(item) = guard
                                .iter_mut()
                                .find(|i| i.relative_path.as_str() == relative_path)
                            {
                                item.status = SharedString::from("done");
                                model_dirty = true;
                            }
                            win.set_processed_count(win.get_processed_count() + 1);
                            let done = win.get_processed_count();
                            let total = win.get_total_files();
                            win.set_status_text(SharedString::from(format!(
                                "Done {done}/{total}: {relative_path}"
                            )));
                        }
                        ProgressUpdate::Skipped { relative_path, .. } => {
                            let mut guard = fd.lock().unwrap();
                            if let Some(item) = guard
                                .iter_mut()
                                .find(|i| i.relative_path.as_str() == relative_path)
                            {
                                item.status = SharedString::from("skipped");
                                model_dirty = true;
                            }
                            win.set_skipped_count(win.get_skipped_count() + 1);
                            win.set_status_text(SharedString::from(format!(
                                "Skipped: {relative_path}"
                            )));
                        }
                        ProgressUpdate::Error {
                            relative_path,
                            error_message,
                            ..
                        } => {
                            let mut guard = fd.lock().unwrap();
                            if let Some(item) = guard
                                .iter_mut()
                                .find(|i| i.relative_path.as_str() == relative_path)
                            {
                                item.status = SharedString::from("error");
                                item.error_message = SharedString::from(error_message.as_str());
                                model_dirty = true;
                            }
                            win.set_error_count(win.get_error_count() + 1);
                            win.set_status_text(SharedString::from(format!(
                                "Error: {relative_path}"
                            )));
                        }
                        ProgressUpdate::Log { .. } => {} // log messages not displayed
                        ProgressUpdate::Finished { .. } => {
                            finished = true;
                        }
                    }
                }

                if model_dirty {
                    let guard = fd.lock().unwrap();
                    refresh_model(&win, &guard);
                }

                if finished {
                    win.set_processing(false);
                    cf.store(false, Ordering::Relaxed);
                    let ok = win.get_processed_count();
                    let err = win.get_error_count();
                    let skip = win.get_skipped_count();
                    let total = win.get_total_files();
                    win.set_status_text(SharedString::from(format!(
                        "Complete — {ok}/{total} OK, {err} errors, {skip} skipped"
                    )));
                    win.set_show_dialog(true);
                }
            });
    }
}
