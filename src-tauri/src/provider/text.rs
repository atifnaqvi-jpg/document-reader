use std::path::Path;

use crate::model::{AppError, DocumentMeta, Format, LibraryEntry};

use super::ReaderProvider;

/// Plain text and Markdown share one provider: both are read whole-file and
/// paginated later (P2) by the UI layer, not by a layout engine.
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
        _entry: &LibraryEntry,
        _page: u32,
        _scale: f32,
    ) -> Result<Vec<u8>, AppError> {
        Err(AppError::RenderUnavailable)
    }

    /// Reads the file recorded in the library row. `LibraryEntry::path` is the only
    /// source of the filesystem location; `DocumentMeta::id` is a UUID.
    fn extract_text(&self, entry: &LibraryEntry, _page: u32) -> Result<String, AppError> {
        self.read(Path::new(&entry.path))
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
        let entry = entry_for(&path);
        assert_eq!(
            TextProvider.render_page(&entry, 0, 1.0).unwrap_err().code(),
            "RENDER_UNAVAILABLE"
        );
    }

    /// A `LibraryEntry` whose `id` is a UUID and whose `path` is the real file —
    /// the exact shape the library layer produces.
    fn entry_for(path: &Path) -> LibraryEntry {
        LibraryEntry {
            meta: DocumentMeta {
                id: uuid::Uuid::new_v4().to_string(),
                title: path.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
                author: None,
                pages: None,
                format: Format::Txt,
            },
            path: path.display().to_string(),
            missing: false,
            added_at: 1,
            last_opened_at: None,
        }
    }

    #[test]
    fn extract_text_reads_the_real_file_via_the_stored_path() {
        // Regression test for the P1.0 bug: extraction used `Path::new(&doc.id)`,
        // i.e. tried to open a UUID as a filename, so it could never succeed.
        let contents = b"the quick brown fox";
        let path = write_temp("extract.txt", contents);
        let entry = entry_for(&path);

        let text = TextProvider.extract_text(&entry, 0).unwrap();
        assert_eq!(text, "the quick brown fox");
    }

    #[test]
    fn extraction_follows_the_library_path_when_the_id_is_not_path_like() {
        // The id must have no bearing on which file is read.
        let path = write_temp("by-path.txt", b"located by path");
        let mut entry = entry_for(&path);
        entry.meta.id = "not-a-path-at-all".into();
        assert_eq!(
            TextProvider.extract_text(&entry, 0).unwrap(),
            "located by path"
        );
    }

    #[test]
    fn extraction_uses_the_path_even_when_the_id_looks_like_one() {
        // Guards the reverse mistake: a stale id that happens to look like a path
        // must not win over the stored path.
        let path = write_temp("authoritative.txt", b"from the stored path");
        let mut entry = entry_for(&path);
        entry.meta.id = "C:/somewhere/else.txt".into();
        assert_eq!(
            TextProvider.extract_text(&entry, 0).unwrap(),
            "from the stored path"
        );
    }

    #[test]
    fn extraction_reports_a_missing_file_rather_than_panicking() {
        let entry = LibraryEntry {
            meta: DocumentMeta {
                id: uuid::Uuid::new_v4().to_string(),
                title: None,
                author: None,
                pages: None,
                format: Format::Txt,
            },
            path: "definitely/not/here.txt".into(),
            missing: true,
            added_at: 1,
            last_opened_at: None,
        };
        assert_eq!(
            TextProvider.extract_text(&entry, 0).unwrap_err().code(),
            "FILE_NOT_FOUND"
        );
    }
}
