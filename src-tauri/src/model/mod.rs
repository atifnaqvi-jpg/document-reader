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
    /// The file declares `/Encrypt` (P2.4 T7).
    ///
    /// Read from the file itself at import (lopdf's metadata loader reports it without
    /// needing the password), because a PDF with an empty user password still *opens* —
    /// what makes it read-only is the trailer, not whether a prompt was survived. Kept on
    /// the meta so it flattens into [`LibraryEntry`] and the frontend gets the banner from
    /// the ordinary library list, with no extra round trip.
    pub encrypted: bool,
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

/// Where a search hit came from (P2-D5).
///
/// The two live in different FTS5 tables and mean different things to the user:
/// a `Document` hit is the file's own text ("the book says this"), an
/// `Annotation` hit is something the user wrote ("I marked this"). Merging them
/// into one untagged list would make a search for a note the user wrote look
/// like it came from the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchOrigin {
    Document,
    Annotation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub document_id: String,
    pub page: u32,
    pub snippet: String,
    /// Labels the hit as document text or user annotation (P2-D5).
    pub origin: SearchOrigin,
    /// For an annotation hit, the annotation it came from, so the UI can select
    /// or scroll to it. `None` for document hits.
    pub annotation_id: Option<String>,
}

/// How a free-text annotation is anchored (D-A4). The four names are the report's
/// own: a sticky note is `note`, and these are the three that are set *in* the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FreeTextMode {
    /// A framed box that does not grow to fit its text.
    Textbox,
    /// A box with a leader line to the point it refers to.
    Callout,
    /// Text typed anywhere, with no visible box.
    Typewriter,
}

/// Tools a user can place on a page (P2-D4). `atype` in the database is the
/// serialised form of this enum, so adding a tool never needs a migration --
/// only the JSON blob's `v` may need to grow (see [`Annotation::data`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind {
    Highlight,
    Underline,
    Strikeout,
    /// P3.1 (D-A1): a wavy underline. Same `/QuadPoints` geometry as `underline`.
    Squiggly,
    Note,
    /// P3.1 (D-A4): any of the three in-page text tools; the mode picks the dictionary
    /// subtype and whether an appearance stream is written (D-A8).
    FreeText(FreeTextMode),
    Ink,
    /// P3.1 (D-A4): a straight line, or the same line with a head at its second end.
    Line { arrow: bool },
    /// P3.1 (D-A4): the old `shape` name, which `parse` aliases here on read.
    Rect,
    Oval,
}

impl AnnotationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AnnotationKind::Highlight => "highlight",
            AnnotationKind::Underline => "underline",
            AnnotationKind::Strikeout => "strikeout",
            AnnotationKind::Squiggly => "squiggly",
            AnnotationKind::Note => "note",
            AnnotationKind::FreeText(FreeTextMode::Textbox) => "textbox",
            AnnotationKind::FreeText(FreeTextMode::Callout) => "callout",
            AnnotationKind::FreeText(FreeTextMode::Typewriter) => "typewriter",
            AnnotationKind::Ink => "ink",
            AnnotationKind::Line { arrow: false } => "line",
            AnnotationKind::Line { arrow: true } => "arrow",
            AnnotationKind::Rect => "rect",
            AnnotationKind::Oval => "oval",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "highlight" => Some(AnnotationKind::Highlight),
            "underline" => Some(AnnotationKind::Underline),
            "strikeout" => Some(AnnotationKind::Strikeout),
            "squiggly" => Some(AnnotationKind::Squiggly),
            "note" => Some(AnnotationKind::Note),
            "textbox" => Some(AnnotationKind::FreeText(FreeTextMode::Textbox)),
            "callout" => Some(AnnotationKind::FreeText(FreeTextMode::Callout)),
            "typewriter" => Some(AnnotationKind::FreeText(FreeTextMode::Typewriter)),
            "ink" => Some(AnnotationKind::Ink),
            "line" => Some(AnnotationKind::Line { arrow: false }),
            "arrow" => Some(AnnotationKind::Line { arrow: true }),
            "rect" => Some(AnnotationKind::Rect),
            "oval" => Some(AnnotationKind::Oval),
            // D-A4: `shape` is retired as a *name*, not as a row. Rows written by a build
            // before P3.1 say `shape`; they become `rect` here, on read, so no DB migration
            // is needed and a downgrade still finds rows it understands.
            "shape" => Some(AnnotationKind::Rect),
            _ => None,
        }
    }
}

/// A user annotation on a page (P2-D4).
///
/// `data` is a schema-versioned JSON blob rather than typed columns: each tool
/// has a different geometry (a highlight is rectangles, ink is a polyline, a note
/// is a point plus text), and a blob lets a new tool ship without a table
/// migration. `v` inside the blob handles shape changes within a tool.
///
/// `text` is denormalised out of `data` on purpose: it is what the FTS5 index
/// needs (P2-D5), and extracting it in SQL would mean parsing JSON in queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub id: String,
    pub document_id: String,
    pub page: u32,
    pub kind: AnnotationKind,
    pub data: String,
    pub text: Option<String>,
    pub created_at: i64,
    pub modified_at: i64,
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
    /// The document is password-protected and was opened read-only (P2.4 T7). Every
    /// editing command refuses with this while the document stays as it is; the only
    /// way to an editable file is the explicit "Decrypt a copy" action (T8).
    #[error(
        "this document is password-protected and was opened read-only; use \"Decrypt a copy\" to edit a copy"
    )]
    EncryptedReadOnly,
    /// The password supplied for a protected PDF was not accepted (P2.4 F7/F8).
    #[error("the password did not unlock this document")]
    WrongPassword,
    /// A Stage-1 admissibility gate refused the run (P2.4 T3). The string *names the
    /// violated gate* and, for the width gate, quotes both measured widths — a refusal
    /// that does not say why is indistinguishable from a bug.
    #[error("{0}")]
    TextRunNotEditable(String),
    /// Stage 2 (T11/T13): the replacement needs glyphs that neither the run's original
    /// font nor any standard-14 substitute can supply, and the shipped bundle genuinely
    /// lacks them. Permanent within this build: changing the substitution setting will not
    /// help, only a different font or a shorter range will.
    #[error(
        "the replacement needs glyphs that neither this run's font nor a standard font can \
         supply; choose a different font, select a narrower range, or use the annotation \
         tools to add a note"
    )]
    GlyphsNotCoverable,
    /// MECH-B (P2.7.6): the glyph **is** in a bundled face, but the subset could not be
    /// built. Deliberately a different code from `GlyphsNotCoverable`: this one is our bug,
    /// not a limitation of the shipped faces, and telling the user to "choose a different
    /// font" would send them after a problem they cannot fix.
    ///
    /// The parenthetical detail is the builder's own reason. It is included because this code
    /// means "our bug": a report that cannot say *what* failed costs a second round trip, and
    /// the user-facing sentence above it already states the remedy.
    #[error(
        "the glyphs for this replacement exist in the bundled font, but the font subset \
         could not be built; this is a defect in this build -- retry once, and report it if \
         it happens again ({0})"
    )]
    SubsetBuildFailed(String),
    /// P2.7.7: the run's own CIDs cannot be matched to glyphs this writer can place.
    ///
    /// A **permanent fact about the document**, unlike `CID_SUBSET_BUILD_FAILED`: a
    /// `/CIDToGIDMap` that is not `/Identity`, or a correspondence that puts a character
    /// somewhere the source program draws from a different glyph. Retrying, updating, or
    /// re-running the same edit on the same file changes nothing. The remedy is in the name.
    #[error(
        "this run's font maps characters to glyphs in a way this editor cannot extend ({0}); \
         choose a different font for this text, or use the annotation tools to add a note"
    )]
    CidMappingUnsupported(String),

    /// P2.7.7: the bundled face covers the characters, but the CID subset could not be built.
    ///
    /// **Temporary — our bug**, and separate from `SUBSET_BUILD_FAILED` because the two
    /// builders fail for different reasons. Remedy: report it.
    #[error(
        "this editor could not build the font this text needs ({0}); this is a defect in the \
         editor, not in your document, so please report it"
    )]
    CidSubsetBuildFailed(String),
    /// Stage 2 (O-ST2-4): the user turned substitution off, and the original font cannot
    /// set the replacement. Refused rather than substituting, per the toggle's contract.
    #[error(
        "this run's font does not contain the glyphs the replacement needs, and substitution \
         is turned off; select a narrower range, or use the annotation tools to add a note"
    )]
    SubstituteRefused,
    /// The lopdf run no longer matches the run the user picked (P2.4 T4): the file
    /// changed under the editor, so writing would land the text in the wrong place.
    #[error("this text changed since it was selected; reopen the page and try again")]
    TextRunMismatch,
    /// A page's decoded content exceeded the processing cap (P2.4 T3e / F3).
    #[error("this page's content is too large to edit safely")]
    ContentTooLarge,
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
            AppError::EncryptedReadOnly => "ENCRYPTED_READ_ONLY",
            AppError::WrongPassword => "WRONG_PASSWORD",
            AppError::TextRunNotEditable(_) => "TEXT_RUN_NOT_EDITABLE",
            AppError::GlyphsNotCoverable => "GLYPHS_NOT_COVERABLE",
            AppError::SubsetBuildFailed(_) => "SUBSET_BUILD_FAILED",
            AppError::CidMappingUnsupported(_) => "CID_MAPPING_UNSUPPORTED",
            AppError::CidSubsetBuildFailed(_) => "CID_SUBSET_BUILD_FAILED",
            AppError::SubstituteRefused => "SUBSTITUTE_REFUSED",
            AppError::TextRunMismatch => "TEXT_RUN_MISMATCH",
            AppError::ContentTooLarge => "CONTENT_TOO_LARGE",
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
                encrypted: false,
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

    /// D-A4: every kind survives a round trip through its stored name. The database stores
    /// `as_str()`, so a name that cannot be parsed back is a row that silently changes kind
    /// on the next read -- and the palette would offer a tool whose own rows are unrecognisable.
    #[test]
    fn every_annotation_kind_round_trips_through_its_stored_name() {
        let kinds = [
            AnnotationKind::Highlight,
            AnnotationKind::Underline,
            AnnotationKind::Strikeout,
            AnnotationKind::Squiggly,
            AnnotationKind::Note,
            AnnotationKind::FreeText(FreeTextMode::Textbox),
            AnnotationKind::FreeText(FreeTextMode::Callout),
            AnnotationKind::FreeText(FreeTextMode::Typewriter),
            AnnotationKind::Ink,
            AnnotationKind::Line { arrow: false },
            AnnotationKind::Line { arrow: true },
            AnnotationKind::Rect,
            AnnotationKind::Oval,
        ];
        for kind in kinds {
            assert_eq!(
                AnnotationKind::parse(kind.as_str()),
                Some(kind),
                "`{}` does not parse back to the kind that wrote it",
                kind.as_str()
            );
        }
    }

    /// D-A4: `shape` is retired as a *name*, not as a row. A build before P3.1 wrote `shape`,
    /// and those rows must read as `rect` without a database migration.
    #[test]
    fn the_retired_shape_name_reads_as_rect() {
        assert_eq!(AnnotationKind::parse("shape"), Some(AnnotationKind::Rect));
        assert_ne!(
            AnnotationKind::Rect.as_str(),
            "shape",
            "writing `shape` again would make the alias a two-way door and re-create the old name"
        );
    }

    /// The two line kinds differ only by their arrow, so the distinction has to survive
    /// serialisation or every arrow silently becomes a plain line.
    #[test]
    fn an_arrow_is_not_a_plain_line() {
        assert_ne!(
            AnnotationKind::Line { arrow: false }.as_str(),
            AnnotationKind::Line { arrow: true }.as_str()
        );
    }
}
