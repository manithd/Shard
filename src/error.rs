use std::path::PathBuf;

/// Application-level error types for the PDF-to-WebP converter.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Filesystem I/O error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Directory walk error.
    #[error("Walkdir error: {0}")]
    Walkdir(#[from] walkdir::Error),

    /// PDF processing error.
    #[error("PDF processing error: {0}")]
    Pdf(String),

    /// WebP encoding error.
    #[error("WebP encoding error: {0}")]
    Webp(String),

    /// Image processing error.
    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),

    /// Insufficient disk space on target drive.
    #[error(
        "Insufficient disk space: have {available} bytes, need approximately {need} bytes on {path:?}"
    )]
    InsufficientDiskSpace {
        need: u64,
        available: u64,
        path: PathBuf,
    },

    /// Invalid or malformed path.
    #[error("Invalid path: {0}")]
    InvalidPath(String),

    /// No PDF files found in source directory.
    #[error("No PDF files found in source directory")]
    NoPdfFiles,

    /// Source directory does not exist.
    #[error("Source directory does not exist: {0}")]
    SourceNotFound(PathBuf),

    /// Source path is not a directory.
    #[error("Source is not a directory: {0}")]
    SourceNotDir(PathBuf),

    /// Generic error message.
    #[error("{0}")]
    Custom(String),

    /// Cancelled by user.
    #[error("Operation cancelled by user")]
    Cancelled,

    /// Error writing to error log file.
    #[error("Failed to write error log: {0}")]
    LogError(String),
}

impl From<String> for AppError {
    fn from(msg: String) -> Self {
        AppError::Custom(msg)
    }
}

impl From<&str> for AppError {
    fn from(msg: &str) -> Self {
        AppError::Custom(msg.to_string())
    }
}
