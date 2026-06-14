use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Output format for converted pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputFormat {
    Webp,
    Svg,
}

/// Application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub format: OutputFormat,
    pub dpi: u32,
    pub quality: u8,
    pub adaptive_encoding: bool,
    pub quality_target: f64,
    pub svg_precision: u8,
    pub svg_no_text: bool,
    pub svg_strip_background: bool,
    pub overwrite: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            source_path: PathBuf::new(),
            output_path: PathBuf::new(),
            format: OutputFormat::Webp,
            dpi: 150,
            quality: 75,
            adaptive_encoding: true,
            quality_target: 85.0,
            svg_precision: 4,
            svg_no_text: false,
            svg_strip_background: true,
            overwrite: false,
        }
    }
}

impl AppConfig {
    /// Validate the configuration. Returns a list of error messages (empty = valid).
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.source_path.as_os_str().is_empty() {
            errors.push("Source path is not set".into());
        } else if !self.source_path.exists() {
            errors.push(format!(
                "Source path does not exist: {}",
                self.source_path.display()
            ));
        } else if !self.source_path.is_dir() {
            errors.push(format!(
                "Source path is not a directory: {}",
                self.source_path.display()
            ));
        }

        if self.output_path.as_os_str().is_empty() {
            errors.push("Output path is not set".into());
        }

        match self.format {
            OutputFormat::Webp => {
                if self.dpi < 90 || self.dpi > 200 {
                    errors.push("DPI must be between 90 and 200".into());
                }
                if self.quality == 0 || self.quality > 100 {
                    errors.push("Quality must be between 1 and 100".into());
                }
            }
            OutputFormat::Svg => {
                if self.svg_precision < 1 || self.svg_precision > 10 {
                    errors.push("SVG precision must be between 1 and 10".into());
                }
            }
        }

        errors
    }
}
