use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Txt,
    Md,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Txt => "txt",
            Format::Md => "md",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "pdf" => Some(Format::Pdf),
            "txt" => Some(Format::Txt),
            "md" | "markdown" => Some(Format::Md),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMeta {
    pub id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub pages: Option<u32>,
    pub format: Format,
}

/// A library row: `DocumentMeta` plus the bookkeeping the UI needs to show
/// moved/deleted files without a second round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEntry {
    #[serde(flatten)]
    pub meta: DocumentMeta,
    pub path: String,
    pub missing: bool,
    pub added_at: i64,
    pub last_opened_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub id: String,
    pub document_id: String,
    pub page: Option<u32>,
    pub offset: Option<u32>,
    pub label: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub document_id: String,
    pub page: u32,
    pub snippet: String,
}

/// A rendered page as it crosses to the frontend (P1-D2): encoded pixel bytes plus
/// dimensions, so the UI can size its container without a second round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedPage {
    /// Base64-encoded PNG, not a raw byte array. Tauri serialises `Vec<u8>` as a JSON
    /// array of numbers, which turns a 200 KB PNG into ~800 KB of IPC payload plus
    /// parse cost; base64 costs ~33% and the frontend can build a `data:` URL directly.
    pub bytes: String,
    /// Pixels.
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("not a readable text file: {0}")]
    NotText(String),
    #[error("pdfium library unavailable: {0}")]
    PdfiumUnavailable(String),
    #[error("page rendering is available in a later phase")]
    RenderUnavailable,
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    Other(String),
}

impl AppError {
    /// Stable, machine-readable identifier sent to the frontend.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::FileNotFound(_) => "FILE_NOT_FOUND",
            AppError::UnsupportedFormat(_) => "UNSUPPORTED_FORMAT",
            AppError::NotText(_) => "NOT_TEXT",
            AppError::PdfiumUnavailable(_) => "PDFIUM_UNAVAILABLE",
            AppError::RenderUnavailable => "RENDER_UNAVAILABLE",
            AppError::Db(_) => "DB_ERROR",
            AppError::Other(_) => "OTHER",
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("AppError", 2)?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_from_extension_is_case_insensitive() {
        assert_eq!(Format::from_extension("PDF"), Some(Format::Pdf));
        assert_eq!(Format::from_extension("md"), Some(Format::Md));
        assert_eq!(Format::from_extension("MARKDOWN"), Some(Format::Md));
        assert_eq!(Format::from_extension("docx"), None);
    }

    #[test]
    fn format_round_trips_through_str() {
        for format in [Format::Pdf, Format::Txt, Format::Md] {
            assert_eq!(Format::from_extension(format.as_str()), Some(format));
        }
    }

    #[test]
    fn errors_serialize_with_code_and_message() {
        let json = serde_json::to_value(AppError::FileNotFound("/tmp/x.pdf".into())).unwrap();
        assert_eq!(json["code"], "FILE_NOT_FOUND");
        assert_eq!(json["message"], "file not found: /tmp/x.pdf");

        let json = serde_json::to_value(AppError::RenderUnavailable).unwrap();
        assert_eq!(json["code"], "RENDER_UNAVAILABLE");
    }

    #[test]
    fn library_entry_flattens_meta() {
        let entry = LibraryEntry {
            meta: DocumentMeta {
                id: "id".into(),
                title: Some("T".into()),
                author: None,
                pages: Some(3),
                format: Format::Pdf,
            },
            path: "C:/docs/t.pdf".into(),
            missing: false,
            added_at: 1,
            last_opened_at: None,
        };
        let json = serde_json::to_value(entry).unwrap();
        assert_eq!(json["id"], "id");
        assert_eq!(json["format"], "pdf");
        assert_eq!(json["path"], "C:/docs/t.pdf");
    }
}
