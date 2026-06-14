use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::error::AppError;

/// A PDF file discovered during directory scanning.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// Absolute path to the PDF file on disk.
    pub full_path: PathBuf,
    /// Path relative to the source root directory.
    pub relative_path: PathBuf,
}

/// Scan a directory recursively for PDF files (case-insensitive extension match).
///
/// Returns a sorted vector of `ScannedFile` entries, ordered by relative path.
/// Returns `AppError::NoPdfFiles` if no PDFs are found.
///
/// # Errors
///
/// Returns `SourceNotFound` if the source directory does not exist,
/// `SourceNotDir` if the path is not a directory, or
/// `Walkdir` for filesystem errors during traversal.
pub fn scan_pdf_files(source_dir: &Path) -> Result<Vec<ScannedFile>, AppError> {
    if !source_dir.exists() {
        return Err(AppError::SourceNotFound(source_dir.to_path_buf()));
    }
    if !source_dir.is_dir() {
        return Err(AppError::SourceNotDir(source_dir.to_path_buf()));
    }

    let mut files: Vec<ScannedFile> = Vec::new();

    for entry in WalkDir::new(source_dir)
        .follow_links(false)
        .sort_by(|a, b| {
            a.file_name()
                .to_ascii_lowercase()
                .cmp(&b.file_name().to_ascii_lowercase())
        })
    {
        let entry = entry.map_err(AppError::Walkdir)?;

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let is_pdf = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));

        if !is_pdf {
            continue;
        }

        let full_path = path.to_path_buf();
        // Safety: path is guaranteed to be under source_dir because WalkDir started there.
        let relative_path = path.strip_prefix(source_dir).unwrap_or(path).to_path_buf();

        files.push(ScannedFile {
            full_path,
            relative_path,
        });
    }

    if files.is_empty() {
        return Err(AppError::NoPdfFiles);
    }

    Ok(files)
}

/// Build the mirrored output path for a single page within a document's output directory.
///
/// Example:
/// `source/A/B/C/doc.pdf`, page 1 → `output/A/B/C/doc/page-001.webp`
pub fn mirror_page_path(output_root: &Path, relative_pdf_path: &Path, page_num: u32) -> PathBuf {
    let parent = relative_pdf_path.parent().unwrap_or(Path::new(""));
    let stem = relative_pdf_path.file_stem().unwrap_or_default();

    let doc_dir = output_root.join(parent).join(stem);
    let page_filename = format!("page-{page_num:03}.webp");
    doc_dir.join(page_filename)
}

/// Build the mirrored output directory for a document (parent of all page images).
///
/// Example:
/// `source/A/B/C/doc.pdf` → `output/A/B/C/doc/`
pub fn mirror_doc_dir(output_root: &Path, relative_pdf_path: &Path) -> PathBuf {
    let parent = relative_pdf_path.parent().unwrap_or(Path::new(""));
    let stem = relative_pdf_path.file_stem().unwrap_or_default();
    output_root.join(parent).join(stem)
}

/// Check whether a PDF appears to have been already converted.
///
/// Returns `true` if the mirrored output directory exists and contains
/// at least one `.webp` file.
pub fn is_converted(output_root: &Path, relative_pdf_path: &Path) -> bool {
    let doc_dir = mirror_doc_dir(output_root, relative_pdf_path);
    if !doc_dir.exists() {
        return false;
    }

    doc_dir.read_dir().is_ok_and(|entries| {
        entries.flatten().any(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("webp"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mirror_page_path_unix() {
        let output = Path::new("/output");
        let rel = Path::new("physics/2019/en/pp1.pdf");
        let result = mirror_page_path(output, rel, 1);
        assert_eq!(
            result,
            PathBuf::from("/output/physics/2019/en/pp1/page-001.webp")
        );
    }

    #[test]
    fn test_mirror_page_path_page_12() {
        let output = Path::new("/out");
        let rel = Path::new("doc.pdf");
        let result = mirror_page_path(output, rel, 12);
        assert_eq!(result, PathBuf::from("/out/doc/page-012.webp"));
    }

    #[test]
    fn test_mirror_doc_dir() {
        let output = Path::new("/output");
        let rel = Path::new("physics/2019/en/pp1.pdf");
        let result = mirror_doc_dir(output, rel);
        assert_eq!(result, PathBuf::from("/output/physics/2019/en/pp1"));
    }

    #[test]
    fn test_mirror_doc_dir_root_level() {
        let output = Path::new("/output");
        let rel = Path::new("doc.pdf");
        let result = mirror_doc_dir(output, rel);
        assert_eq!(result, PathBuf::from("/output/doc"));
    }

    #[test]
    fn test_is_converted_no_dir() {
        let output = Path::new("/tmp/nonexistent_output");
        let rel = Path::new("test.pdf");
        assert!(!is_converted(output, rel));
    }
}
