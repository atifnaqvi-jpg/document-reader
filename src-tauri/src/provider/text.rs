use std::path::Path;

use crate::model::{AppError, DocumentMeta, Format};

use super::ReaderProvider;

/// Plain text and Markdown share one provider: both are read whole-file and
/// paginated later (P1) by the UI layer, not by a layout engine.
#[derive(Debug, Default, Clone)]
pub struct TextProvider;

impl TextProvider {
    fn read(&self, path: &Path) -> Result<String, AppError> {
        if !path.is_file() {
            return Err(AppError::FileNotFound(path.display().to_string()));
        }
        let bytes = std::fs::read(path).map_err(|e| AppError::Other(e.to_string()))?;
        String::from_utf8(bytes).map_err(|_| AppError::NotText(path.display().to_string()))
    }
}

impl ReaderProvider for TextProvider {
    fn open(&self, path: &Path) -> Result<DocumentMeta, AppError> {
        self.read(path)?; // validates existence and UTF-8

        Ok(DocumentMeta {
            id: String::new(), // assigned by the library layer on import
            title: path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned),
            author: None,
            pages: None, // text reflows; page count is a view concern (P2)
            format: match path
                .extension()
                .and_then(|ext| ext.to_str())
                .and_then(Format::from_extension)
            {
                Some(format) => format,
                None => Format::Txt,
            },
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

    fn extract_text(&self, doc: &DocumentMeta, _page: u32) -> Result<String, AppError> {
        let path = Path::new(&doc.id);
        self.read(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("docreader-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn opens_utf8_text_and_uses_file_stem_as_title() {
        let path = write_temp("sample.txt", b"hello world");
        let meta = TextProvider.open(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("sample"));
        assert_eq!(meta.format, Format::Txt);
        assert_eq!(meta.pages, None);
        assert_eq!(meta.author, None);
    }

    #[test]
    fn infers_markdown_format() {
        let path = write_temp("notes.md", b"# heading");
        let meta = TextProvider.open(&path).unwrap();
        assert_eq!(meta.format, Format::Md);
    }

    #[test]
    fn rejects_invalid_utf8() {
        let path = write_temp("binary.txt", &[0xff, 0xfe, 0x00, 0x01]);
        let error = TextProvider.open(&path).unwrap_err();
        assert_eq!(error.code(), "NOT_TEXT");
    }

    #[test]
    fn rejects_missing_file() {
        let error = TextProvider
            .open(Path::new("definitely/not/here.txt"))
            .unwrap_err();
        assert_eq!(error.code(), "FILE_NOT_FOUND");
    }

    #[test]
    fn render_is_deferred_to_a_later_phase() {
        let path = write_temp("render.txt", b"abc");
        let meta = TextProvider.open(&path).unwrap();
        assert_eq!(
            TextProvider.render_page(&meta, 0, 1.0).unwrap_err().code(),
            "RENDER_UNAVAILABLE"
        );
    }
}
