//! PDF → WebP Converter
//!
//! A high-performance, cross-platform desktop application for converting
//! large volumes of educational PDF documents into WebP images.
//!
//! # Architecture
//!
//! - **UI**: Slint (native cross-platform rendering)
//! - **PDF Rendering**: Poppler (default) / Pdfium (optional feature-gated)
//! - **Image Encoding**: libwebp via `webp` crate
//! - **Concurrency**: Rayon (data-parallel page/file processing)
//!
//! # Features
//!
//! - `pdfium`: Use pdfium-render instead of poppler for PDF rendering
//! - `bundled-pdfium`: Auto-download and bundle pdfium binary
//!
//! # Usage
//!
//! ```bash
//! # Default (uses poppler-utils/pdftoppm)
//! cargo run
//!
//! # With pdfium (system-installed)
//! cargo run --features pdfium
//!
//! # With bundled pdfium
//! cargo run --features bundled-pdfium
//!
//! # Release build (optimized)
//! cargo build --release
//! ```

use pdf2webp::app::App;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize the logger.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!(
        "Starting PDF → WebP Converter v{}",
        env!("CARGO_PKG_VERSION")
    );

    // Create the application.
    let app = App::new()?;

    // Set up UI callbacks and timers.
    app.setup();

    // Run the Slint event loop.
    app.run()
}
