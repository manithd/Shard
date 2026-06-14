use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::AppConfig;
use crate::converter::{PdfRenderer, PopplerRenderer};
use crate::mirror::ScannedFile;
use crate::worker;

/// Which panel has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Settings,
    FileList,
}

/// Status of a single file.
#[derive(Debug, Clone)]
pub enum FileStatus {
    Queued,
    Processing,
    Done,
    Error(String),
    Skipped,
}

/// A file entry shown in the file list.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub relative_path: String,
    pub selected: bool,
    pub status: FileStatus,
    pub current_page: u32,
    pub total_pages: u32,
}

/// Overall TUI application state.
pub struct TuiState {
    pub config: AppConfig,
    pub scanned_files: Vec<ScannedFile>,
    pub files: Vec<FileEntry>,
    pub selected_panel: Panel,
    pub list_cursor: usize,
    pub processing: bool,
    pub processed_count: u32,
    pub error_count: u32,
    pub skipped_count: u32,
    pub log_lines: Vec<String>,
    pub finished: bool,
    pub summary: Option<String>,
    pub cancel_flag: Arc<AtomicBool>,
}

impl TuiState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config,
            scanned_files: Vec::new(),
            files: Vec::new(),
            selected_panel: Panel::FileList,
            list_cursor: 0,
            processing: false,
            processed_count: 0,
            error_count: 0,
            skipped_count: 0,
            log_lines: Vec::new(),
            finished: false,
            summary: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_files(&mut self, files: Vec<ScannedFile>) {
        self.scanned_files = files;
        self.files = self
            .scanned_files
            .iter()
            .map(|f| FileEntry {
                relative_path: f.relative_path.display().to_string(),
                selected: true,
                status: FileStatus::Queued,
                current_page: 0,
                total_pages: 0,
            })
            .collect();
        self.list_cursor = 0;
    }

    pub fn toggle_panel(&mut self) {
        self.selected_panel = match self.selected_panel {
            Panel::Settings => Panel::FileList,
            Panel::FileList => Panel::Settings,
        };
    }

    pub fn move_cursor(&mut self, delta: i32) {
        if self.selected_panel != Panel::FileList {
            return;
        }
        let len = self.files.len().max(1);
        self.list_cursor = (self.list_cursor as i32 + delta).rem_euclid(len as i32) as usize;
    }

    pub fn toggle_selected_file(&mut self) {
        if self.selected_panel != Panel::FileList {
            return;
        }
        if let Some(entry) = self.files.get_mut(self.list_cursor) {
            entry.selected = !entry.selected;
        }
    }

    pub fn select_all(&mut self, sel: bool) {
        for entry in self.files.iter_mut() {
            entry.selected = sel;
        }
    }

    pub fn apply_update(&mut self, update: worker::ProgressUpdate) {
        match update {
            worker::ProgressUpdate::Started { total } => {
                self.processing = true;
                self.processed_count = 0;
                self.error_count = 0;
                self.skipped_count = 0;
                self.log_lines.push(format!("Processing {total} files..."));
            }
            worker::ProgressUpdate::Processing {
                ref relative_path, ..
            } => {
                if let Some(entry) = self
                    .files
                    .iter_mut()
                    .find(|e| e.relative_path == *relative_path)
                {
                    entry.status = FileStatus::Processing;
                    entry.current_page = 0;
                    entry.total_pages = 0;
                }
            }
            worker::ProgressUpdate::PageProgress {
                ref relative_path,
                current_page,
                total_pages,
            } => {
                if let Some(entry) = self
                    .files
                    .iter_mut()
                    .find(|e| e.relative_path == *relative_path)
                {
                    entry.current_page = current_page;
                    entry.total_pages = total_pages;
                }
            }
            worker::ProgressUpdate::Completed {
                ref relative_path, ..
            } => {
                if let Some(entry) = self
                    .files
                    .iter_mut()
                    .find(|e| e.relative_path == *relative_path)
                {
                    entry.status = FileStatus::Done;
                }
                self.processed_count += 1;
            }
            worker::ProgressUpdate::Skipped {
                ref relative_path, ..
            } => {
                if let Some(entry) = self
                    .files
                    .iter_mut()
                    .find(|e| e.relative_path == *relative_path)
                {
                    entry.status = FileStatus::Skipped;
                }
                self.skipped_count += 1;
            }
            worker::ProgressUpdate::Error {
                ref relative_path,
                ref error_message,
                ..
            } => {
                if let Some(entry) = self
                    .files
                    .iter_mut()
                    .find(|e| e.relative_path == *relative_path)
                {
                    entry.status = FileStatus::Error(error_message.clone());
                }
                self.error_count += 1;
                self.log_lines
                    .push(format!("✗ {relative_path}: {error_message}"));
            }
            worker::ProgressUpdate::Log { message } => {
                self.log_lines.push(message);
            }
            worker::ProgressUpdate::Finished {
                success_count,
                error_count,
                skipped_count,
            } => {
                self.processing = false;
                self.finished = true;
                self.summary = Some(format!(
                    "Done — {success_count} OK, {error_count} errors, {skipped_count} skipped"
                ));
            }
        }

        // Trim log to last 100 lines.
        if self.log_lines.len() > 100 {
            self.log_lines.drain(0..self.log_lines.len() - 100);
        }
    }

    pub fn start_conversion(
        &mut self,
    ) -> Result<std::sync::mpsc::Receiver<worker::ProgressUpdate>, anyhow::Error> {
        self.cancel_flag.store(false, Ordering::Relaxed);
        self.finished = false;
        self.summary = None;

        let selected: Vec<ScannedFile> = self
            .scanned_files
            .iter()
            .zip(self.files.iter())
            .filter(|(_, entry)| entry.selected)
            .map(|(sf, _)| sf.clone())
            .collect();

        #[cfg(feature = "pdfium")]
        let renderer: Arc<dyn PdfRenderer> = match crate::converter::PdfiumRenderer::new() {
            Ok(r) => Arc::new(r),
            Err(_) => Arc::new(PopplerRenderer),
        };
        #[cfg(not(feature = "pdfium"))]
        let renderer: Arc<dyn PdfRenderer> = Arc::new(PopplerRenderer);

        let rx = worker::start_conversion(
            selected,
            self.config.output_path.clone(),
            renderer,
            self.config.format,
            self.config.dpi,
            self.config.quality,
            self.config.adaptive_encoding,
            self.config.quality_target,
            self.config.svg_precision,
            self.config.svg_no_text,
            self.config.svg_strip_background,
            self.config.overwrite,
            self.cancel_flag.clone(),
        );

        Ok(rx)
    }

    pub fn stop_conversion(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}
