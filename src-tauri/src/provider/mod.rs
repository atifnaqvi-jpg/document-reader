use std::path::Path;
use std::sync::Arc;

use crate::model::{AppError, DocumentMeta, Format};

/// The single architectural seam (DECISIONS §5). Every format lives behind this
/// trait so the rest of the app never learns about PDFs or Markdown.
pub trait ReaderProvider: Send + Sync + std::fmt::Debug {
    fn open(&self, path: &Path) -> Result<DocumentMeta, AppError>;

    fn render_page(&self, doc: &DocumentMeta, page: u32, scale: f32) -> Result<Vec<u8>, AppError>;

    fn extract_text(&self, doc: &DocumentMeta, page: u32) -> Result<String, AppError>;
}

/// OCR slots in behind the same shape later (DECISIONS §5); the v1 implementation
/// is the PDFium text layer.
pub trait TextExtractor: Send + Sync + std::fmt::Debug {
    fn extract(&self, path: &Path, page: u32) -> Result<Option<String>, AppError>;
}

mod pdf;
mod text;

pub use pdf::PdfProvider;
pub use text::TextProvider;

pub fn provider_for(path: &Path) -> Result<Arc<dyn ReaderProvider>, AppError> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => match Format::from_extension(ext) {
            Some(Format::Pdf) => Ok(Arc::new(PdfProvider)),
            Some(_) => Ok(Arc::new(TextProvider)),
            None => Err(AppError::UnsupportedFormat(ext.to_string())),
        },
        None => Err(AppError::UnsupportedFormat("<none>".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_pdf_extension_to_pdf_provider() {
        let provider = provider_for(Path::new("C:/docs/report.PDF")).unwrap();
        // PdfProvider fails fast on a missing file, before any pdfium binding.
        let error = provider
            .open(Path::new("C:/definitely/missing.pdf"))
            .unwrap_err();
        assert_eq!(error.code(), "FILE_NOT_FOUND");
    }

    #[test]
    fn maps_text_extensions_to_text_provider() {
        for name in ["notes.txt", "readme.MD", "guide.Markdown"] {
            let provider = provider_for(Path::new(name)).unwrap();
            let error = provider.open(Path::new("missing-file")).unwrap_err();
            assert_eq!(error.code(), "FILE_NOT_FOUND", "for {name}");
        }
    }

    #[test]
    fn rejects_unknown_formats() {
        let error = provider_for(Path::new("book.docx")).unwrap_err();
        assert_eq!(error.code(), "UNSUPPORTED_FORMAT");
        assert_eq!(error.to_string(), "unsupported format: docx");
    }

    #[test]
    fn rejects_missing_extension() {
        let error = provider_for(Path::new("Makefile")).unwrap_err();
        assert_eq!(error.code(), "UNSUPPORTED_FORMAT");
        assert_eq!(error.to_string(), "unsupported format: <none>");
    }
}
