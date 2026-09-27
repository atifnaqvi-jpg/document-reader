use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pdfium_render::prelude::*;

use crate::model::{AppError, DocumentMeta, Format};

use super::ReaderProvider;

const PDFIUM_ENV_VAR: &str = "PDFIUM_LIBRARY_PATH";
/// Bundled next to the executable on Windows (docs/P0-DESIGN.md §2).
const PDFIUM_BUNDLED_FILE: &str = "pdfium.dll";

/// Native PDFium rendering (DECISIONS Q1) — the OS webview PDF viewer is never used.
/// P0 reads metadata only; `render_page`/`extract_text` land in P1.
#[derive(Debug, Default)]
pub struct PdfProvider;

impl PdfProvider {
    fn ensure_file(path: &Path) -> Result<(), AppError> {
        if path.is_file() {
            Ok(())
        } else {
            Err(AppError::FileNotFound(path.display().to_string()))
        }
    }

    /// Resolution order from docs/P0-DESIGN.md §2: explicit env var, then a
    /// `pdfium.dll` beside the executable. Resolved once per process; the actual
    /// library binding is per document.
    fn library_path() -> Result<&'static PathBuf, AppError> {
        static RESOLVED: OnceLock<Result<PathBuf, AppError>> = OnceLock::new();

        RESOLVED
            .get_or_init(|| {
                if let Ok(from_env) = std::env::var(PDFIUM_ENV_VAR) {
                    let path = PathBuf::from(from_env);
                    if path.is_file() {
                        return Ok(path);
                    }
                    return Err(AppError::PdfiumUnavailable(format!(
                        "{PDFIUM_ENV_VAR} points at a missing file: {}",
                        path.display()
                    )));
                }

                if let Some(bundled) = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|dir| dir.join(PDFIUM_BUNDLED_FILE)))
                    .filter(|candidate| candidate.is_file())
                {
                    return Ok(bundled);
                }

                Err(AppError::PdfiumUnavailable(format!(
                    "set {PDFIUM_ENV_VAR} or place {PDFIUM_BUNDLED_FILE} next to the executable"
                )))
            })
            .as_ref()
            .map_err(|e| AppError::PdfiumUnavailable(e.to_string()))
    }

    fn bind() -> Result<Box<dyn PdfiumLibraryBindings>, AppError> {
        let library = Self::library_path()?;
        Pdfium::bind_to_library(library)
            .map_err(|e| AppError::PdfiumUnavailable(format!("{}: {e}", library.display())))
    }
}

impl ReaderProvider for PdfProvider {
    fn open(&self, path: &Path) -> Result<DocumentMeta, AppError> {
        Self::ensure_file(path)?;
        let bindings = Self::bind()?;

        let pdfium = Pdfium::new(bindings);
        let document = pdfium
            .load_pdf_from_file(&path.display().to_string(), None)
            .map_err(|e| AppError::Other(format!("failed to open PDF: {e}")))?;

        let metadata = document.metadata();
        let tag = |kind: PdfDocumentMetadataTagType| {
            metadata
                .get(kind)
                .map(|tag| tag.value().trim().to_owned())
                .filter(|value| !value.is_empty())
        };

        Ok(DocumentMeta {
            id: String::new(), // assigned by the library layer on import
            title: tag(PdfDocumentMetadataTagType::Title).or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned)
            }),
            author: tag(PdfDocumentMetadataTagType::Author),
            pages: Some(document.pages().len() as u32),
            format: Format::Pdf,
        })
    }

    fn render_page(
        &self,
        _doc: &DocumentMeta,
        _page: u32,
        _scale: f32,
    ) -> Result<Vec<u8>, AppError> {
        Err(AppError::RenderUnavailable)
    }

    fn extract_text(&self, _doc: &DocumentMeta, _page: u32) -> Result<String, AppError> {
        Err(AppError::RenderUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> DocumentMeta {
        DocumentMeta {
            id: "x".into(),
            title: None,
            author: None,
            pages: Some(1),
            format: Format::Pdf,
        }
    }

    #[test]
    fn missing_file_fails_before_touching_pdfium() {
        let error = PdfProvider
            .open(Path::new("definitely/not/here.pdf"))
            .unwrap_err();
        assert_eq!(error.code(), "FILE_NOT_FOUND");
    }

    #[test]
    fn absent_pdfium_reports_a_clear_error_rather_than_panicking() {
        let path = std::env::temp_dir().join("docreader-p0-not-a-real.pdf");
        std::fs::write(&path, b"%PDF-1.4\nnot really a pdf\n").unwrap();

        // Either pdfium is present (dev machine) or we get a typed, actionable
        // error — the one thing that must never happen is a panic.
        if let Err(error) = PdfProvider.open(&path) {
            assert!(
                matches!(error.code(), "PDFIUM_UNAVAILABLE" | "OTHER"),
                "unexpected error: {error}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn render_is_deferred_to_a_later_phase() {
        assert_eq!(
            PdfProvider.render_page(&doc(), 0, 1.0).unwrap_err().code(),
            "RENDER_UNAVAILABLE"
        );
        assert_eq!(
            PdfProvider.extract_text(&doc(), 0).unwrap_err().code(),
            "RENDER_UNAVAILABLE"
        );
    }
}
