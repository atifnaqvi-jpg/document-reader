//! XFDF import and export (P3.1 D-A9/D-A10).
//!
//! XFDF is the XML form of FDF, and it carries annotation *lists* rather than form values,
//! which is why it is the interchange format this phase ships (D-A9). FDF proper is the
//! AcroForm data container and belongs to P3.3.
//!
//! The mapping is deliberately not one-to-one with the PDF dictionary. XFDF's vocabulary is
//! the *old* Acrobat one (`<highlight>`, `<square>`, `<circle>`, `<freetext>`, `<line>`), and
//! its coordinates are PDF user-space with the origin at the bottom-left, the same space the
//! blob already stores (P2 §4). So no conversion pair is needed here, and none is added.
//!
//! What is *not* carried: `/AP` appearance streams (rebuilt from the blob by D-A8), the FTS
//! index (a DB concern), and anything this build does not model. An unknown element is
//! skipped and counted rather than failing the import, because a file from another viewer
//! will contain types we do not have, and refusing the whole file over one of them would
//! make import useless in practice.

use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use serde::{Deserialize, Serialize};

use crate::model::{Annotation, AnnotationKind, AnnotationStatus, AppError};

/// One annotation as XFDF carries it.
///
/// Separate from [`Annotation`] because XFDF has no notion of a database id, a document id or
/// a blob version: it is an interchange shape, and conflating it with the stored row would
/// leak storage details into the file format.
#[derive(Debug, Clone, PartialEq)]
pub struct XfdfAnnotation {
    /// The annotation name. XFDF has no `/NM`, so this is the field D-A10 matches on.
    pub name: String,
    pub page: u32,
    pub kind: AnnotationKind,
    /// PDF user space: `[x1, y1, x2, y2]` with the origin bottom-left.
    pub rect: [f32; 4],
    /// `#rrggbb`, or `None` when the file did not say and the palette default should apply.
    pub color: Option<String>,
    pub contents: Option<String>,
    pub author: Option<String>,
    /// Seconds since the epoch, as XFDF's `date` attribute spells them (`D:YYYYMMDDHHmmSS`).
    pub created: Option<i64>,
    pub modified: Option<i64>,
    pub status: Option<AnnotationStatus>,
    /// Quad points for the highlight family, flattened as XFDF writes them.
    pub quad_points: Vec<f32>,
    /// Ink polylines, one `Vec` per stroke.
    pub ink_paths: Vec<Vec<[f32; 2]>>,
    /// `[x1, y1, x2, y2]` for `line`/`arrow`.
    pub line: Option<[f32; 4]>,
}

/// What an import did.
///
/// Returned rather than logged (D-A10): the user has to be told how many rows were skipped,
/// and a count only the log knows is a count the user never sees.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: usize,
    /// Already present: the name matched an existing row (D-A10).
    pub skipped_existing: usize,
    /// A type or shape this build cannot represent.
    pub skipped_unsupported: usize,
    /// The annotation was malformed: no name, no page, or a rect that is not four numbers.
    pub skipped_malformed: usize,
}

impl ImportReport {
    pub fn total_skipped(&self) -> usize {
        self.skipped_existing + self.skipped_unsupported + self.skipped_malformed
    }
}

/// The namespace Acrobat writes. Kept identical so our files open in other viewers.
const XFDF_NAMESPACE: &str = "http://ns.adobe.com/xfdf/";

/// The XFDF element name for a kind, or `None` when the kind has no XFDF spelling.
///
/// `textbox`, `callout` and `typewriter` are all `<freetext>` with a different `/IT`: XFDF has
/// one element for all three, so the mode is recovered from the element plus, where the file
/// says so, its `intent` attribute.
fn element_for(kind: AnnotationKind) -> Option<&'static str> {
    match kind {
        AnnotationKind::Highlight => Some("highlight"),
        AnnotationKind::Underline => Some("underline"),
        AnnotationKind::Strikeout => Some("strikeout"),
        AnnotationKind::Squiggly => Some("squiggly"),
        AnnotationKind::Note => Some("text"),
        AnnotationKind::FreeText(_) => Some("freetext"),
        AnnotationKind::Ink => Some("ink"),
        AnnotationKind::Line { .. } => Some("line"),
        AnnotationKind::Rect => Some("square"),
        AnnotationKind::Oval => Some("circle"),
    }
}

/// The kind an XFDF element names, given the `intent` attribute where the element is
/// ambiguous.
fn kind_for(element: &str, intent: Option<&str>) -> Option<AnnotationKind> {
    match element {
        "highlight" => Some(AnnotationKind::Highlight),
        "underline" => Some(AnnotationKind::Underline),
        "strikeout" => Some(AnnotationKind::Strikeout),
        "squiggly" => Some(AnnotationKind::Squiggly),
        "text" => Some(AnnotationKind::Note),
        "freetext" => Some(match intent {
            Some("FreeTextCallout") => {
                AnnotationKind::FreeText(crate::model::FreeTextMode::Callout)
            }
            Some("FreeTextTypewriter") => {
                AnnotationKind::FreeText(crate::model::FreeTextMode::Typewriter)
            }
            // The unspecified case is the box form: it is the one that always has a `/Rect`
            // and needs no extra geometry, so it is the safe default for a file that says
            // nothing.
            _ => AnnotationKind::FreeText(crate::model::FreeTextMode::Textbox),
        }),
        "ink" => Some(AnnotationKind::Ink),
        "line" => Some(AnnotationKind::Line { arrow: false }),
        "square" => Some(AnnotationKind::Rect),
        "circle" => Some(AnnotationKind::Oval),
        _ => None,
    }
}

/// Escapes text for an XML attribute or body.
///
/// quick-xml escapes on write, but the date and status attributes are built by hand, so this
/// is the one place that has to be right for both.
fn hex_color(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let digits = trimmed.strip_prefix('#')?;
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", digits.to_ascii_lowercase()))
}

/// `D:YYYYMMDDHHmmSS` as XFDF writes dates, from seconds since the epoch.
///
/// Hand-rolled because the crate has no date dependency and XFDF's format is fixed-width
/// UTC. A local-time interpretation of a field that is written as UTC is how an import
/// silently shifts every timestamp by the user's offset.
fn format_date(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let mut remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    remainder %= 3_600;
    let minute = remainder / 60;
    let second = remainder % 60;
    format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

/// Parses the XFDF date forms this build accepts, returning seconds since the epoch.
///
/// Accepts `D:YYYYMMDDHHmmSS` and the same with a trailing `Z` or offset, which other
/// viewers write. An unparseable date is `None` rather than an error: a wrong timestamp on
/// one row is not a reason to refuse a file.
fn parse_date(value: &str) -> Option<i64> {
    let body = value.trim().strip_prefix("D:")?;
    let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<i64> {
        digits.get(from..to).and_then(|part| part.parse().ok())
    };
    let year = number(0, 4)?;
    let month = number(4, 6)?;
    let day = number(6, 8)?;
    let hour = number(8, 10).unwrap_or(0);
    let minute = number(10, 12).unwrap_or(0);
    let second = number(12, 14).unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since the epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The civil date for days since the epoch.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Writes a document's annotations as an XFDF file.
///
/// Rows with no XFDF spelling are not silently dropped: they are counted, and the caller
/// reports the count alongside the file name. Exporting a document and finding one annotation
/// missing on re-import, with nothing to explain it, is worse than being told up front.
pub fn export(annotations: &[Annotation]) -> Result<(String, usize), AppError> {
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);

    write_event(
        &mut writer,
        Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)),
    )?;

    let mut root = BytesStart::new("xfdf");
    root.push_attribute(("xmlns", XFDF_NAMESPACE));
    write_event(&mut writer, Event::Start(root))?;
    write_event(&mut writer, Event::Start(BytesStart::new("annots")))?;

    let mut unsupported = 0;
    for annotation in annotations {
        let Some(element) = element_for(annotation.kind) else {
            unsupported += 1;
            continue;
        };
        let geometry = Geometry::from_annotation(annotation);

        // Owned strings, kept alive for the whole element: quick-xml's attributes borrow, so
        // building them inline from temporaries is what produced a type error, and cloning
        // them into `tag` one at a time would be the same bug with extra steps.
        let name = annotation.id.clone();
        let page = annotation.page.to_string();
        let rect = flatten(&geometry.rect);
        let created = format_date(annotation.created_at);
        let modified = format_date(annotation.modified_at);
        let subject = kind_subject(annotation.kind);

        let mut tag = BytesStart::new(element);
        tag.push_attribute(("name", name.as_str()));
        tag.push_attribute(("page", page.as_str()));
        tag.push_attribute(("rect", rect.as_str()));
        if let Some(color) = geometry.color.as_deref() {
            tag.push_attribute(("color", color));
        }
        if let Some(status) = geometry.status {
            tag.push_attribute(("state", status));
        }
        tag.push_attribute(("date", created.as_str()));
        tag.push_attribute(("creationdate", created.as_str()));
        tag.push_attribute(("moddate", modified.as_str()));
        tag.push_attribute(("subject", subject));
        if matches!(annotation.kind, AnnotationKind::Line { arrow: true }) {
            // XFDF has one element for lines and arrows; the head is an attribute. `OpenArrow`
            // matches the `/LE` this build writes, so an arrow's meaning survives a round trip.
            tag.push_attribute(("endHead", "OpenArrow"));
            tag.push_attribute(("startHead", "None"));
        }
        write_event(&mut writer, Event::Start(tag))?;

        if let Some(line) = geometry.line.as_ref() {
            // A line's two endpoints, in XFDF's own `coords` spelling. Without this the
            // `<line>` element carries only its bounding rect, and a re-import draws a
            // horizontal line along the box's bottom edge instead of the one the user drew.
            let coords = flatten(line);
            let mut element_start = BytesStart::new("linepoints");
            element_start.push_attribute(("coords", coords.as_str()));
            write_event(&mut writer, Event::Empty(element_start))?;
        }

        if !geometry.quad_points.is_empty() {
            let coords = flatten(&geometry.quad_points);
            let mut quads = BytesStart::new("quadPoints");
            quads.push_attribute(("coords", coords.as_str()));
            write_event(&mut writer, Event::Empty(quads))?;
        }

        if !geometry.ink_paths.is_empty() {
            write_event(&mut writer, Event::Start(BytesStart::new("inklist")))?;
            for path in &geometry.ink_paths {
                let flat: Vec<f32> = path.iter().flat_map(|point| [point[0], point[1]]).collect();
                let coords = flatten(&flat);
                let mut gesture = BytesStart::new("gesture");
                gesture.push_attribute(("coords", coords.as_str()));
                write_event(&mut writer, Event::Empty(gesture))?;
            }
            write_event(&mut writer, Event::End(BytesEnd::new("inklist")))?;
        }

        if let Some(contents) = annotation.text.as_deref() {
            write_event(&mut writer, Event::Start(BytesStart::new("contents")))?;
            write_event(&mut writer, Event::Text(BytesText::new(contents)))?;
            write_event(&mut writer, Event::End(BytesEnd::new("contents")))?;
        }

        if let Some(status) = geometry.status {
            write_event(&mut writer, Event::Start(BytesStart::new("state")))?;
            write_event(&mut writer, Event::Text(BytesText::new(status)))?;
            write_event(&mut writer, Event::End(BytesEnd::new("state")))?;
        }

        write_event(&mut writer, Event::End(BytesEnd::new(element)))?;
    }

    write_event(&mut writer, Event::End(BytesEnd::new("annots")))?;
    write_event(&mut writer, Event::End(BytesEnd::new("xfdf")))?;

    let body = String::from_utf8(writer.into_inner())
        .map_err(|error| AppError::Other(format!("XFDF export produced invalid UTF-8: {error}")))?;
    Ok((body, unsupported))
}

/// Writes one event, mapping the io error into the app's type.
fn write_event(writer: &mut Writer<Vec<u8>>, event: Event<'_>) -> Result<(), AppError> {
    writer
        .write_event(event)
        .map_err(|error| AppError::Other(format!("XFDF write failed: {error}")))
}

/// The blob's geometry, read once and reused by the element writer.
struct Geometry {
    rect: [f32; 4],
    color: Option<String>,
    quad_points: Vec<f32>,
    ink_paths: Vec<Vec<[f32; 2]>>,
    line: Option<[f32; 4]>,
    status: Option<&'static str>,
}

impl Geometry {
    /// Reads the geometry out of a row's blob.
    ///
    /// A blob that does not parse yields `None` from [`Geometry::to_element`], which counts as
    /// unsupported. That is the honest classification: the row exists but cannot be expressed,
    /// and calling it a successful export would be a lie in the one direction that matters.
    fn from_annotation(annotation: &Annotation) -> Self {
        let blob: serde_json::Value =
            serde_json::from_str(&annotation.data).unwrap_or(serde_json::Value::Null);

        let rect = blob
            .get("rect")
            .and_then(parse_rect_value)
            .or_else(|| {
                // The highlight family stores `rects`; XFDF wants one bounding rect plus the
                // quads. Union rather than first: a multi-line selection's first rect is a
                // fragment, and exporting only that would shrink the annotation on re-import.
                blob.get("rects")
                    .and_then(|value| value.as_array())
                    .and_then(|rects| union_of(rects))
            })
            .or_else(|| {
                blob.get("rects").and_then(|value| {
                    value
                        .as_array()
                        .and_then(|rects| rects.first())
                        .and_then(parse_rect_value)
                })
            })
            .unwrap_or([0.0, 0.0, 0.0, 0.0]);

        let color = blob
            .get("color")
            .and_then(|value| value.as_str())
            .and_then(hex_color);

        let quad_points = match blob.get("rects").and_then(|value| value.as_array()) {
            Some(rects) => rects.iter().filter_map(quads_of).flatten().collect(),
            None => Vec::new(),
        };

        let mut ink_paths = Vec::new();
        if let Some(strokes) = blob.get("strokes").and_then(|value| value.as_array()) {
            for stroke in strokes {
                if let Some(points) = stroke.as_array() {
                    let path: Vec<[f32; 2]> = points.iter().filter_map(point_of).collect();
                    if !path.is_empty() {
                        ink_paths.push(path);
                    }
                }
            }
        }

        let line = blob.get("line").and_then(parse_rect_value);

        let status = blob
            .get("status")
            .and_then(|value| value.as_str())
            .and_then(AnnotationStatus::parse)
            .map(AnnotationStatus::state_name);

        Self {
            rect,
            color,
            quad_points,
            ink_paths,
            line,
            status,
        }
    }
}

/// The `subject` attribute XFDF viewers show as the annotation's type.
fn kind_subject(kind: AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Highlight => "Highlight",
        AnnotationKind::Underline => "Underline",
        AnnotationKind::Strikeout => "Strike Out",
        AnnotationKind::Squiggly => "Squiggly",
        AnnotationKind::Note => "Note",
        AnnotationKind::FreeText(_) => "Free Text",
        AnnotationKind::Ink => "Ink",
        AnnotationKind::Line { arrow: false } => "Line",
        AnnotationKind::Line { arrow: true } => "Arrow",
        AnnotationKind::Rect => "Square",
        AnnotationKind::Oval => "Circle",
    }
}

/// Formats a flat coordinate list the way XFDF writes `coords`.
fn flatten(values: &[f32]) -> String {
    values
        .iter()
        .map(|value| {
            // Trimming the trailing zeros keeps files readable and diffable; `{:.3}` would
            // round a coordinate a user placed deliberately.
            let mut text = format!("{value:.4}");
            while text.contains('.') && (text.ends_with('0') || text.ends_with('.')) {
                text.pop();
            }
            text
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Parses a `coords` attribute into numbers.
fn parse_coords(value: &str) -> Option<Vec<f32>> {
    let numbers: Vec<f32> = value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f32>().ok())
        .collect();
    if numbers.is_empty() {
        None
    } else {
        Some(numbers)
    }
}

/// Four numbers from a JSON array, or `None`.
fn parse_rect_value(value: &serde_json::Value) -> Option<[f32; 4]> {
    let array = value.as_array()?;
    if array.len() != 4 {
        return None;
    }
    let mut rect = [0.0f32; 4];
    for (index, entry) in array.iter().enumerate() {
        rect[index] = entry.as_f64()? as f32;
    }
    Some(rect)
}

/// A point from a JSON `[x, y]`, or `None`.
fn point_of(value: &serde_json::Value) -> Option<[f32; 2]> {
    let array = value.as_array()?;
    if array.len() < 2 {
        return None;
    }
    Some([array[0].as_f64()? as f32, array[1].as_f64()? as f32])
}

/// The bounding box of a list of rects.
fn union_of(rects: &[serde_json::Value]) -> Option<[f32; 4]> {
    let mut union: Option<[f32; 4]> = None;
    for rect in rects.iter().filter_map(parse_rect_value) {
        union = Some(match union {
            None => rect,
            Some(current) => [
                current[0].min(rect[0]),
                current[1].min(rect[1]),
                current[2].max(rect[2]),
                current[3].max(rect[3]),
            ],
        });
    }
    union
}

/// The eight numbers XFDF wants per quad, from one stored rect.
///
/// The stored `[x1, y1, x2, y2]` is a rect; XFDF's quad is the four corners in the order
/// lower-left, lower-right, upper-left, upper-right. Writing the rect twice would be a quad
/// with two coincident corners, which viewers draw as a degenerate wedge.
fn quads_of(value: &serde_json::Value) -> Option<Vec<f32>> {
    let [x1, y1, x2, y2] = parse_rect_value(value)?;
    let (left, right) = (x1.min(x2), x1.max(x2));
    let (bottom, top) = (y1.min(y2), y1.max(y2));
    Some(vec![left, bottom, right, bottom, left, top, right, top])
}

/// Parses an XFDF document into importable annotations.
///
/// Every failure mode is counted into [`ImportReport`] rather than aborting: an XFDF file from
/// a viewer we do not control is the expected input, and one unsupported element in a
/// fifty-annotation file is not a reason to import nothing.
pub fn parse(xml: &str) -> Result<(Vec<XfdfAnnotation>, ImportReport), AppError> {
    let mut reader = Reader::from_str(xml);
    // Deliberately not `trim_text(true)`: quick-xml delivers text and entity references as
    // separate events when they are adjacent, so trimming each node eats the spaces around an
    // `&amp;` and "a < b" comes back as "a<b". The two buffers that need trimming -- contents
    // and state -- are trimmed once, at the end, in `finish`.
    reader.config_mut().trim_text(false);

    let mut parsed = Vec::new();
    let mut report = ImportReport::default();
    let mut current: Option<Partial> = None;
    let mut text_target: Option<TextTarget> = None;

    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(event)) => {
                handle_start(&event, &mut current, &mut text_target, &mut report);
                continue;
            }
            Ok(Event::Empty(event)) => {
                // An empty element is complete on its own: `<state/>` carries no text, and
                // `<gesture/>` would have no points, so neither opens a buffer.
                handle_start(&event, &mut current, &mut text_target, &mut report);
                if is_annotation_element(&event) {
                    finish(current.take(), &mut parsed, &mut report);
                }
                continue;
            }
            Ok(Event::End(event)) => event,
            Ok(Event::Text(event)) => {
                // Text nodes arrive escaped; `&amp;` and `&lt;` have to become their
                // characters or a note's contents come back mangled.
                let value = quick_xml::escape::unescape(event.as_ref())
                    .unwrap_or_default()
                    .into_owned();
                match text_target {
                    Some(TextTarget::Contents) => {
                        if let Some(annotation) = current.as_mut() {
                            annotation.contents.push_str(&value);
                        }
                    }
                    Some(TextTarget::State) => {
                        if let Some(annotation) = current.as_mut() {
                            annotation.state.push_str(&value);
                        }
                    }
                    None => {}
                }
                continue;
            }
            Ok(Event::GeneralRef(reference)) => {
                // quick-xml delivers `&amp;` as its own event, separate from the text around
                // it. Ignoring these deletes every entity character from a note -- the round
                // trip test for "a < b && c > d" came back as "abcd".
                if let Some(character) = reference_char(reference.into_inner().as_ref()) {
                    match text_target {
                        Some(TextTarget::Contents) => {
                            if let Some(annotation) = current.as_mut() {
                                annotation.contents.push(character);
                            }
                        }
                        Some(TextTarget::State) => {
                            if let Some(annotation) = current.as_mut() {
                                annotation.state.push(character);
                            }
                        }
                        None => {}
                    }
                }
                continue;
            }
            Ok(_) => continue,
            Err(error) => {
                return Err(AppError::Other(format!(
                    "XFDF is not well-formed XML: {error}"
                )));
            }
        };

        let name = event.name().as_ref().to_string();
        match name.as_str() {
            "contents" | "state" => text_target = None,
            _ if is_annotation_name(&name) => finish(current.take(), &mut parsed, &mut report),
            _ => {}
        }
    }

    // A file that ended mid-element is truncated, not empty. `None` would make a cut-off
    // download look like a file with no annotations, and the user would be told the import
    // succeeded with zero rows.
    if current.is_some() {
        return Err(AppError::Other(
            "XFDF ended inside an annotation: the file is truncated".to_string(),
        ));
    }

    Ok((parsed, report))
}

/// Whether an element name is one of the annotation elements this build reads.
///
/// One list, used by both the opener and the closer: two lists that disagreed would leave a
/// `current` open for an element nobody closes, and the truncation check at the end would
/// then reject a well-formed file.
fn is_annotation_name(name: &str) -> bool {
    matches!(
        name,
        "highlight"
            | "underline"
            | "strikeout"
            | "squiggly"
            | "text"
            | "freetext"
            | "ink"
            | "line"
            | "square"
            | "circle"
    )
}

fn is_annotation_element(event: &BytesStart<'_>) -> bool {
    is_annotation_name(event.name().as_ref())
}

/// The character an entity reference stands for, for the five XML entities and numeric forms.
///
/// Anything else is `None`: a DTD entity we do not have the definition for cannot be resolved,
/// and inventing a character for it would corrupt the text.
fn reference_char(reference: &str) -> Option<char> {
    match reference {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = reference
                .strip_prefix("#x")
                .or_else(|| reference.strip_prefix("#X"))?;
            u32::from_str_radix(digits, 16)
                .ok()
                .and_then(char::from_u32)
        }
    }
}

/// Whether an element is an annotation *type* this build cannot represent.
///
/// The distinction matters for the report: an unknown annotation type is a row that was not
/// imported, while an unknown *child* element (`<popup>`, `<appearance>`) is a detail of a row
/// that was. Listing the known-unrepresentable types rather than "anything unrecognised" is
/// what keeps child elements out of the count -- an `annots` member is not a fixed vocabulary,
/// so the complement cannot be enumerated.
fn is_unknown_annotation(event: &BytesStart<'_>) -> bool {
    let name = event.name();
    matches!(
        name.as_ref(),
        "caret"
            | "fileattachment"
            | "sound"
            | "movie"
            | "screen"
            | "widget"
            | "link"
            | "redact"
            | "stamp"
            | "polygon"
            | "polyline"
    )
}

/// Handles a Start or Empty element: opens the annotation, or records a child's attributes.
fn handle_start(
    event: &BytesStart<'_>,
    current: &mut Option<Partial>,
    text_target: &mut Option<TextTarget>,
    report: &mut ImportReport,
) {
    let name = event.name().as_ref().to_string();
    if is_annotation_name(&name) {
        *current = Some(Partial::from_element(event, &name, report));
        return;
    }
    match name.as_str() {
        "contents" => {
            if current.is_some() {
                *text_target = Some(TextTarget::Contents);
            }
        }
        "state" => {
            if current.is_some() {
                *text_target = Some(TextTarget::State);
            }
        }
        "quadPoints" => {
            if let Some(annotation) = current.as_mut() {
                if let Some(coords) =
                    attribute(event, "coords").and_then(|value| parse_coords(&value))
                {
                    // A viewer that wrote a count that is not a multiple of eight produced a
                    // shape this build cannot represent. The row is kept and the quads are
                    // dropped: refusing the whole file over one bad attribute is worse.
                    if coords.len() % 8 == 0 {
                        annotation.quad_points = coords;
                    }
                }
            }
        }
        _ if is_unknown_annotation(event) => {
            // An element inside `<annots>` that this build does not model. Counted here
            // rather than at the end, because there is no `Partial` to finish for it.
            report.skipped_unsupported += 1;
        }
        "linepoints" => {
            if let Some(annotation) = current.as_mut() {
                if let Some(coords) =
                    attribute(event, "coords").and_then(|value| parse_coords(&value))
                {
                    if coords.len() >= 4 {
                        annotation.line_points = Some([coords[0], coords[1], coords[2], coords[3]]);
                    }
                }
            }
        }
        "gesture" => {
            if let Some(annotation) = current.as_mut() {
                if let Some(coords) =
                    attribute(event, "coords").and_then(|value| parse_coords(&value))
                {
                    let points: Vec<[f32; 2]> = coords
                        .chunks_exact(2)
                        .map(|pair| [pair[0], pair[1]])
                        .collect();
                    if !points.is_empty() {
                        annotation.ink_paths.push(points);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Which element the text buffer is currently filling.
enum TextTarget {
    Contents,
    State,
}

/// The annotation being built while its element is open.
#[derive(Default)]
struct Partial {
    name: Option<String>,
    page: Option<u32>,
    rect: Option<[f32; 4]>,
    color: Option<String>,
    author: Option<String>,
    date: Option<i64>,
    modified: Option<i64>,
    contents: String,
    state: String,
    quad_points: Vec<f32>,
    ink_paths: Vec<Vec<[f32; 2]>>,
    line_points: Option<[f32; 4]>,
    kind: Option<AnnotationKind>,
    unsupported: bool,
}

impl Partial {
    fn from_element(event: &BytesStart<'_>, name: &str, report: &mut ImportReport) -> Self {
        let kind = kind_for(name, attribute(event, "intent").as_deref());
        let mut partial = Partial {
            kind,
            unsupported: kind.is_none(),
            ..Partial::default()
        };
        if partial.unsupported {
            report.skipped_unsupported += 1;
        }
        partial.rect = attribute(event, "rect")
            .and_then(|value| parse_coords(&value))
            .and_then(|coords| {
                if coords.len() >= 4 {
                    Some([coords[0], coords[1], coords[2], coords[3]])
                } else {
                    None
                }
            });
        partial.name = attribute(event, "name");
        partial.page = attribute(event, "page").and_then(|value| value.parse().ok());
        partial.color = attribute(event, "color").and_then(|value| hex_color(&value));
        partial.author = attribute(event, "title");
        let date = attribute(event, "creationdate").or_else(|| attribute(event, "date"));
        partial.date = date.as_deref().and_then(parse_date);
        partial.modified = attribute(event, "moddate")
            .as_deref()
            .and_then(parse_date)
            .or(partial.date);
        if let Some(coords) = attribute(event, "coords") {
            partial.line_points = parse_coords(&coords).and_then(|values| {
                if values.len() >= 4 {
                    Some([values[0], values[1], values[2], values[3]])
                } else {
                    None
                }
            });
        }
        // An arrow is a `<line>` with an end head set.
        if let Some(head) = attribute(event, "endHead") {
            if head != "None" && !head.is_empty() {
                partial.kind = Some(AnnotationKind::Line { arrow: true });
            }
        }
        partial
    }
}

/// Reads one attribute as a `String`, decoded.
fn attribute(event: &BytesStart<'_>, name: &str) -> Option<String> {
    for entry in event.attributes().flatten() {
        if entry.key.as_ref() == name {
            return Some(entry.value.to_string());
        }
    }
    None
}

/// Moves a finished element into the result, or counts why it could not be.
fn finish(partial: Option<Partial>, parsed: &mut Vec<XfdfAnnotation>, report: &mut ImportReport) {
    let Some(partial) = partial else { return };
    if partial.unsupported {
        // Already counted when the element opened; counting again would double-report.
        return;
    }
    let (Some(kind), Some(name), Some(page)) = (partial.kind, partial.name, partial.page) else {
        report.skipped_malformed += 1;
        return;
    };
    // An XFDF page is zero-based in the file Acrobat writes; a negative or absurd index is
    // malformed rather than clamped, because clamping would put the annotation on a page the
    // user did not choose.
    // XFDF carries the ISO `/State` names (`Rejected`), while `parse` takes the stored
    // lowercase spellings. Comparing them directly meant a status from any real file was
    // silently dropped.
    let status = AnnotationStatus::parse(&partial.state.trim().to_ascii_lowercase());
    parsed.push(XfdfAnnotation {
        name,
        page,
        kind,
        rect: partial.rect.unwrap_or([0.0, 0.0, 0.0, 0.0]),
        color: partial.color,
        contents: if partial.contents.trim().is_empty() {
            None
        } else {
            Some(partial.contents.trim().to_string())
        },
        author: partial.author,
        created: partial.date,
        modified: partial.modified,
        status,
        quad_points: partial.quad_points,
        ink_paths: partial.ink_paths,
        line: partial.line_points,
    });
}

/// Imports parsed annotations into a document, skipping names that already exist (D-A10).
///
/// The skip is keyed on the name, which for our own exports is the row's id, so exporting and
/// re-importing is a no-op. For a file from another viewer the name is whatever that viewer
/// wrote, which is what "the same annotation" means across documents.
///
/// `existing_names` is passed in rather than queried here so this stays testable without a
/// database; the caller supplies `SELECT id FROM Annotation WHERE document_id = ?`.
pub fn merge(
    parsed: Vec<XfdfAnnotation>,
    existing_names: &std::collections::HashSet<String>,
    mut report: ImportReport,
    mut insert: impl FnMut(&XfdfAnnotation) -> Result<(), AppError>,
) -> Result<ImportReport, AppError> {
    for annotation in parsed {
        if existing_names.contains(&annotation.name) {
            // Never overwrite (D-A10): the DB is the source of truth, and an imported row is
            // the older copy in every case that matters.
            report.skipped_existing += 1;
            continue;
        }
        insert(&annotation)?;
        report.imported += 1;
    }
    Ok(report)
}

/// The blob a parsed XFDF annotation becomes.
///
/// The XFDF vocabulary does not map one-to-one onto the blob, so this is where the impedance
/// is absorbed: quads become `rects`, ink gestures become `strokes`, and the status becomes the
/// blob's `status` key. Doing this in one place keeps the import path and the writer using the
/// same shapes.
pub fn blob_for(annotation: &XfdfAnnotation) -> serde_json::Value {
    let mut blob = serde_json::json!({
        "v": 2,
        "rect": annotation.rect,
    });
    if let Some(color) = annotation.color.as_deref() {
        blob["color"] = serde_json::Value::String(color.to_string());
    }
    if let Some(status) = annotation.status {
        blob["status"] = serde_json::Value::String(status.as_str().to_string());
    }
    if let Some(author) = annotation.author.as_deref() {
        blob["author"] = serde_json::Value::String(author.to_string());
    }
    if !annotation.quad_points.is_empty() {
        // Rebuild one rect per quad from its corners, so a highlight comes back with the same
        // per-line rectangles it went out with rather than one bounding box.
        let rects: Vec<[f32; 4]> = annotation
            .quad_points
            .chunks_exact(8)
            .map(|quad| {
                let left = quad[0].min(quad[4]);
                let right = quad[2].max(quad[6]);
                let bottom = quad[1].min(quad[5]);
                let top = quad[3].max(quad[7]);
                [left, bottom, right, top]
            })
            .collect();
        if !rects.is_empty() {
            blob["rects"] = serde_json::json!(rects);
        }
    }
    if !annotation.ink_paths.is_empty() {
        blob["strokes"] = serde_json::json!(annotation.ink_paths);
    }
    if let Some(line) = annotation.line {
        blob["line"] = serde_json::json!(line);
    }
    if let AnnotationKind::FreeText(mode) = annotation.kind {
        blob["mode"] = serde_json::Value::String(
            match mode {
                crate::model::FreeTextMode::Textbox => "textbox",
                crate::model::FreeTextMode::Callout => "callout",
                crate::model::FreeTextMode::Typewriter => "typewriter",
            }
            .to_string(),
        );
    }
    blob
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FreeTextMode;

    mod form_data_tests {
        use super::super::form_data;

        fn xml(body: &str) -> String {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/"><fields>{body}</fields></xfdf>"#
            )
        }

        #[test]
        fn a_field_value_round_trips_through_export_and_parse() {
            let mut values = std::collections::BTreeMap::new();
            values.insert("agree".to_string(), "Yes".to_string());
            values.insert("name".to_string(), "Ada Lovelace".to_string());

            let exported = form_data::export(&values).expect("export");
            let (parsed, report) = form_data::parse(&exported).expect("the export must re-parse");
            let mut report = report;
            let back = form_data::values(&parsed, &mut report);

            assert_eq!(back, values, "export then parse must be the identity");
            assert_eq!(report.imported, 2);
            assert_eq!(report.collapsed_duplicates, 0);
        }

        /// The refusal R7 requires, and the reason it is by content and not by extension: a user who
        /// renames a `.fdf` to `.xfdf` has not changed the bytes, and letting it through would produce
        /// an XML syntax error that names neither the format nor the fix.
        #[test]
        fn an_fdf_file_is_refused_by_name_and_told_what_to_use_instead() {
            let fdf = b"%FDF-1.2\n1 0 obj<</FDF<</Fields[]>>>>endobj\ntrailer<</Root 1 0 R>>";

            let error = form_data::refuse_non_xfdf(fdf)
                .expect_err("an FDF container must be refused before any parsing");

            assert_eq!(error.code(), "FDF_UNSUPPORTED");
            let message = error.to_string();
            assert!(
                message.contains("XFDF"),
                "a refusal must name the format that is supported, or it is a dead end: {message}"
            );
        }

        /// `<annots>` is P3.1's section and a real viewer exports both together. Reading only
        /// `<fields>` must not be confused with failing to read the file.
        #[test]
        fn the_annotation_section_is_ignored_rather_than_mistaken_for_form_data() {
            let document = r#"<?xml version="1.0"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/">
  <annots><square name="a1" page="0" rect="0,0,10,10"/></annots>
  <fields><field name="agree"><value>Yes</value></field></fields>
</xfdf>"#;

            let (parsed, mut report) = form_data::parse(document).expect("parse");
            let values = form_data::values(&parsed, &mut report);

            assert_eq!(values.len(), 1);
            assert_eq!(values.get("agree").map(String::as_str), Some("Yes"));
        }

        /// A radio group legitimately produces several `<field>` entries naming one field. Last wins,
        /// and the collapse is *counted*: a silent merge would make a corrupt file look identical to
        /// a healthy radio group.
        #[test]
        fn a_repeated_name_collapses_to_the_last_value_and_says_how_many_it_collapsed() {
            let document = xml(r#"<field name="choice"><value>Off</value></field>
                   <field name="choice"><value>On</value></field>"#);

            let (parsed, mut report) = form_data::parse(&document).expect("parse");
            let values = form_data::values(&parsed, &mut report);

            assert_eq!(
                values.get("choice").map(String::as_str),
                Some("On"),
                "the last entry is the current state Acrobat writes last"
            );
            assert_eq!(report.collapsed_duplicates, 1);
            assert_eq!(report.imported, 1);
        }

        #[test]
        fn a_field_with_no_name_is_counted_rather_than_dropped_silently() {
            let document = xml(r#"<field><value>orphan</value></field>"#);

            let (parsed, report) = form_data::parse(&document).expect("parse");

            assert!(parsed.is_empty());
            assert_eq!(report.skipped_nameless, 1);
        }

        /// Acrobat's spelling for a cleared field. Without this the value would be the *previous*
        /// field's text, because an empty element emits no text event to clear the buffer.
        #[test]
        fn a_self_closing_field_reads_as_an_empty_value_not_as_a_leftover() {
            let document = xml(r#"<field name="first"><value>Yes</value></field>
                   <field name="second"/>"#);

            let (parsed, mut report) = form_data::parse(&document).expect("parse");
            let values = form_data::values(&parsed, &mut report);

            assert_eq!(values.get("first").map(String::as_str), Some("Yes"));
            assert_eq!(
                values.get("second").map(String::as_str),
                Some(""),
                "a self-closing field must be empty, not carry the previous field's value"
            );
        }

        #[test]
        fn entities_in_a_value_are_decoded() {
            let document = xml(r#"<field name="note"><value>a &amp; b &lt; c</value></field>"#);

            let (parsed, mut report) = form_data::parse(&document).expect("parse");
            let values = form_data::values(&parsed, &mut report);

            assert_eq!(
                values.get("note").map(String::as_str),
                Some("a & b < c"),
                "an escaped value must come back as characters, not as entities"
            );
        }

        /// The claim W7 exists to make: form data leaves the document and comes back into a
        /// *different* copy of it. A unit test on the parser would pass while export and the fill
        /// path disagreed about field names, so this drives the real writer twice.
        #[test]
        fn a_filled_value_survives_export_to_xfdf_and_a_fill_of_a_fresh_copy() {
            use crate::editor::{forms, testpdf};

            let dir = tempfile::tempdir().expect("temp dir");
            let original = dir.path().join("blank.pdf");
            std::fs::write(&original, testpdf::bytes(&testpdf::form_text_fields())).expect("write");

            let first = dir.path().join("first.pdf");
            let outcome = forms::fill_to(
                &original,
                &first,
                &[forms::FillValue {
                    name: "given".to_string(),
                    value: "Ada".to_string(),
                }],
                |path| path.exists(),
            )
            .expect("the blank form fills");

            // Read the field back out of the *saved* file, so the value handed to XFDF is the one
            // that actually persisted rather than the one that was requested.
            let saved = forms::open(&std::path::PathBuf::from(&outcome.path)).expect("reopen");
            let mut values = std::collections::BTreeMap::new();
            for field in saved.fields() {
                if let Some(value) = &field.value {
                    values.insert(field.name.clone(), value.clone());
                }
            }
            assert_eq!(values.get("given").map(String::as_str), Some("Ada"));

            let xfdf = form_data::export(&values).expect("export");

            // A fresh copy of the *original*, so the value can only arrive through the XFDF.
            let (parsed, mut report) = form_data::parse(&xfdf).expect("parse");
            let reimported = form_data::values(&parsed, &mut report);
            let fill: Vec<forms::FillValue> = reimported
                .iter()
                .map(|(name, value)| forms::FillValue {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect();

            let second = dir.path().join("second.pdf");
            forms::fill_to(&original, &second, &fill, |path| path.exists())
                .expect("the re-imported values fill");

            let refilled = forms::open(&second).expect("reopen the second copy");
            let given = refilled.field("given").expect("the field is still there");
            assert_eq!(
                given.value.as_deref(),
                Some("Ada"),
                "the value must survive leave, export, import and refill -- not merely parse"
            );
        }

        #[test]
        fn a_parse_failure_is_reported_rather_than_returning_no_fields() {
            let error = form_data::parse("<xfdf><fields><field name=")
                .expect_err("truncated XML must be an error, not an empty result");

            assert_eq!(error.code(), "OTHER");
            assert!(error.to_string().contains("XFDF"), "{error}");
        }
    }

    fn row(
        id: &str,
        page: u32,
        kind: AnnotationKind,
        blob: serde_json::Value,
        text: Option<&str>,
    ) -> Annotation {
        Annotation {
            id: id.to_string(),
            document_id: "doc-1".to_string(),
            page,
            kind,
            data: blob.to_string(),
            text: text.map(str::to_string),
            created_at: 1_700_000_000,
            modified_at: 1_700_000_000,
        }
    }

    fn highlight_blob(rects: &[[f32; 4]]) -> serde_json::Value {
        serde_json::json!({ "v": 2, "rects": rects, "color": "#fff3b0", "opacity": 0.4 })
    }

    #[test]
    fn export_writes_the_xfdf_namespace_and_every_name() {
        let (xml, unsupported) = export(&[
            row(
                "a",
                0,
                AnnotationKind::Highlight,
                highlight_blob(&[[1.0, 2.0, 3.0, 4.0]]),
                Some("first"),
            ),
            row(
                "b",
                1,
                AnnotationKind::Note,
                serde_json::json!({"v":2,"point":[5.0,6.0]}),
                Some("second"),
            ),
        ])
        .unwrap();

        assert_eq!(unsupported, 0);
        assert!(
            xml.contains(r#"xmlns="http://ns.adobe.com/xfdf/""#),
            "no namespace: {xml}"
        );
        assert!(xml.contains("<highlight"));
        assert!(xml.contains("<text"));
        // The name is what D-A10 matches on, so it has to be in the file.
        assert!(xml.contains(r#"name="a""#));
        assert!(xml.contains(r#"name="b""#));
    }

    #[test]
    fn squiggly_exports_as_its_own_element_not_as_underline() {
        // A round trip through a viewer that rewrote squiggly as underline would silently
        // change what the user marked.
        let (xml, _) = export(&[row(
            "a",
            0,
            AnnotationKind::Squiggly,
            highlight_blob(&[[1.0, 1.0, 2.0, 2.0]]),
            None,
        )])
        .unwrap();
        assert!(
            xml.contains("<squiggly"),
            "squiggly lost its element: {xml}"
        );
        assert!(
            !xml.contains("<underline"),
            "squiggly written as underline: {xml}"
        );
    }

    #[test]
    fn a_multi_rect_highlight_exports_its_union_and_every_quad() {
        // Exporting only the first rect would shrink a multi-line selection on re-import.
        let (xml, _) = export(&[row(
            "a",
            0,
            AnnotationKind::Highlight,
            highlight_blob(&[[10.0, 10.0, 20.0, 12.0], [10.0, 14.0, 40.0, 16.0]]),
            None,
        )])
        .unwrap();

        assert!(
            xml.contains(r#"rect="10,10,40,16""#),
            "union rect wrong: {xml}"
        );
        let quads = xml.split("<quadPoints").nth(1).expect("no quadPoints");
        let coords = quads
            .split("coords=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert_eq!(coords.split(',').count(), 16, "quads: {coords}");
    }

    #[test]
    fn an_arrow_is_a_line_with_an_end_head() {
        let (xml, _) = export(&[row(
            "a",
            0,
            AnnotationKind::Line { arrow: true },
            serde_json::json!({"v":2,"line":[1.0,2.0,3.0,4.0]}),
            None,
        )])
        .unwrap();
        assert!(xml.contains("<line"), "{xml}");
        assert!(xml.contains(r#"endHead="OpenArrow""#), "{xml}");
    }

    #[test]
    fn a_line_keeps_its_endpoints_not_just_its_box() {
        // The rect alone cannot say which diagonal a line runs along: re-importing from the
        // box would draw the other one.
        let (xml, _) = export(&[row(
            "a",
            0,
            AnnotationKind::Line { arrow: false },
            serde_json::json!({"v":2,"line":[1.0,2.0,30.0,40.0]}),
            None,
        )])
        .unwrap();
        assert!(xml.contains(r#"coords="1,2,30,40""#), "{xml}");

        let (parsed, _) = parse(&xml).unwrap();
        assert_eq!(parsed[0].line, Some([1.0, 2.0, 30.0, 40.0]));
    }

    #[test]
    fn a_plain_line_writes_no_head_attribute() {
        // The XFDF analogue of `[/None /None]` on a plain line: absent, not "None".
        let (xml, _) = export(&[row(
            "a",
            0,
            AnnotationKind::Line { arrow: false },
            serde_json::json!({"v":2,"line":[1.0,2.0,3.0,4.0]}),
            None,
        )])
        .unwrap();
        assert!(
            !xml.contains("endHead"),
            "a plain line carried a head: {xml}"
        );
    }

    #[test]
    fn ink_keeps_one_gesture_per_stroke() {
        // Merging two strokes into one gesture would join them into a scribble nobody drew.
        let (xml, _) = export(&[row(
            "a",
            0,
            AnnotationKind::Ink,
            serde_json::json!({"v":2,"strokes":[[[1.0,2.0],[3.0,4.0]],[[5.0,6.0],[7.0,8.0]]]}),
            None,
        )])
        .unwrap();
        assert_eq!(xml.matches("<gesture").count(), 2, "{xml}");
    }

    #[test]
    fn a_status_travels_as_its_state_name() {
        let (xml, _) = export(&[row(
            "a",
            0,
            AnnotationKind::Note,
            serde_json::json!({"v":2,"point":[1.0,1.0],"status":"completed"}),
            None,
        )])
        .unwrap();
        assert!(xml.contains("<state>Completed</state>"), "{xml}");
    }

    #[test]
    fn an_unrepresentable_blob_exports_a_zero_rect_rather_than_vanishing() {
        // The row exists and the user asked for their annotations, so it is written with no
        // geometry instead of being dropped.
        let mut broken = row(
            "a",
            0,
            AnnotationKind::Highlight,
            serde_json::json!({}),
            None,
        );
        broken.data = "not json".to_string();
        let (xml, unsupported) = export(&[broken]).unwrap();
        assert_eq!(unsupported, 0);
        assert!(xml.contains(r#"name="a""#), "{xml}");
    }

    #[test]
    fn export_then_parse_round_trips_the_fields_that_matter() {
        // DoD 4 of P3.1: export, re-import, assert equality.
        let original = row(
            "round-trip-1",
            3,
            AnnotationKind::Highlight,
            highlight_blob(&[[10.0, 20.0, 30.0, 40.0]]),
            Some("a claim to check"),
        );

        let (xml, _) = export(std::slice::from_ref(&original)).unwrap();
        let (parsed, report) = parse(&xml).unwrap();

        assert_eq!(
            report.total_skipped(),
            0,
            "a clean round trip skipped rows: {report:?}"
        );
        assert_eq!(parsed.len(), 1);
        let back = &parsed[0];
        assert_eq!(back.name, original.id);
        assert_eq!(back.page, 3);
        assert_eq!(back.kind, AnnotationKind::Highlight);
        assert_eq!(back.rect, [10.0, 20.0, 30.0, 40.0]);
        assert_eq!(back.contents.as_deref(), Some("a claim to check"));
        assert_eq!(back.color.as_deref(), Some("#fff3b0"));
        assert_eq!(back.created, Some(original.created_at));
    }

    #[test]
    fn re_importing_the_same_file_yields_the_same_names() {
        // DoD 4's second half: identity is the name, so the names must survive the trip.
        let rows = vec![
            row(
                "x",
                0,
                AnnotationKind::Highlight,
                highlight_blob(&[[1.0, 1.0, 2.0, 2.0]]),
                None,
            ),
            row(
                "y",
                1,
                AnnotationKind::Note,
                serde_json::json!({"v":2,"point":[1.0,1.0]}),
                Some("hi"),
            ),
        ];

        let (xml, _) = export(&rows).unwrap();
        let (first, _) = parse(&xml).unwrap();
        let (second, _) = parse(&xml).unwrap();

        let names = |rows: &[XfdfAnnotation]| -> Vec<String> {
            rows.iter().map(|row| row.name.clone()).collect()
        };
        assert_eq!(
            names(&first),
            names(&second),
            "a repeat import changed the names"
        );
        assert_eq!(names(&first), vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn a_freetext_callout_is_recovered_from_its_intent() {
        let xml = r#"<xfdf xmlns="http://ns.adobe.com/xfdf/"><annots>
            <freetext name="c" page="0" rect="1,2,3,4" intent="FreeTextCallout"/>
        </annots></xfdf>"#;
        let (parsed, _) = parse(xml).unwrap();
        assert_eq!(
            parsed[0].kind,
            AnnotationKind::FreeText(FreeTextMode::Callout)
        );
    }

    #[test]
    fn a_freetext_without_an_intent_is_a_textbox() {
        // The box form always has a rect and needs no extra geometry, so it is the safe
        // reading of a file that says nothing.
        let xml = r#"<xfdf><annots><freetext name="t" page="0" rect="1,2,3,4"/></annots></xfdf>"#;
        let (parsed, _) = parse(xml).unwrap();
        assert_eq!(
            parsed[0].kind,
            AnnotationKind::FreeText(FreeTextMode::Textbox)
        );
    }

    #[test]
    fn an_unknown_element_is_counted_and_does_not_fail_the_file() {
        let xml = r#"<xfdf><annots>
            <circle name="ok" page="0" rect="1,2,3,4"/>
            <sound name="noise" page="0" rect="1,2,3,4"/>
        </annots></xfdf>"#;
        let (parsed, report) = parse(xml).unwrap();
        assert_eq!(parsed.len(), 1, "the good row was lost with the bad one");
        assert_eq!(report.skipped_unsupported, 1);
    }

    #[test]
    fn an_annotation_without_a_name_or_page_is_counted_as_malformed() {
        let xml = r#"<xfdf><annots>
            <circle page="0" rect="1,2,3,4"/>
            <circle name="n" rect="1,2,3,4"/>
        </annots></xfdf>"#;
        let (parsed, report) = parse(xml).unwrap();
        assert!(parsed.is_empty());
        assert_eq!(report.skipped_malformed, 2);
    }

    #[test]
    fn a_truncated_file_is_an_error_not_an_empty_import() {
        // A cut-off download would otherwise report "imported 0" as a success.
        let xml = r#"<xfdf><annots><circle name="a" page="0" rect="1,2,3,4">"#;
        assert!(
            parse(xml).is_err(),
            "a truncated file was accepted as empty"
        );
    }

    #[test]
    fn a_state_element_sets_the_status() {
        let xml = r#"<xfdf><annots><circle name="a" page="0" rect="1,2,3,4">
            <state>Rejected</state>
        </circle></annots></xfdf>"#;
        let (parsed, _) = parse(xml).unwrap();
        assert_eq!(parsed[0].status, Some(AnnotationStatus::Rejected));
    }

    #[test]
    fn contents_text_round_trips_through_entities() {
        // A note about "a < b && c > d" is not exotic.
        let original = row(
            "esc",
            0,
            AnnotationKind::Note,
            serde_json::json!({"v":2,"point":[1.0,1.0]}),
            Some("a < b && c > d"),
        );
        let (xml, _) = export(&[original]).unwrap();
        let (parsed, _) = parse(&xml).unwrap();
        assert_eq!(parsed[0].contents.as_deref(), Some("a < b && c > d"));
    }

    #[test]
    fn a_bad_hex_colour_is_ignored_rather_than_stored() {
        // A colour the palette cannot express must not reach the blob.
        let xml = r##"<xfdf><annots><circle name="a" page="0" rect="1,2,3,4" color="red"/></annots></xfdf>"##;
        let (parsed, _) = parse(xml).unwrap();
        assert_eq!(parsed[0].color, None);
    }

    #[test]
    fn dates_are_written_and_read_in_utc() {
        // A local-time reading of a UTC field shifts every timestamp by the user's offset.
        assert_eq!(format_date(0), "D:19700101000000");
        assert_eq!(parse_date("D:19700101000000"), Some(0));
        let stamp = 1_700_000_000;
        assert_eq!(parse_date(&format_date(stamp)), Some(stamp));
    }

    #[test]
    fn merge_skips_an_existing_name_and_counts_it() {
        // D-A10: the match is on the name, the skip is counted, and the existing row is not
        // overwritten.
        let parsed = vec![
            XfdfAnnotation {
                name: "already-here".to_string(),
                page: 0,
                kind: AnnotationKind::Highlight,
                rect: [0.0, 0.0, 1.0, 1.0],
                color: None,
                contents: Some("imported".to_string()),
                author: None,
                created: None,
                modified: None,
                status: None,
                quad_points: Vec::new(),
                ink_paths: Vec::new(),
                line: None,
            },
            XfdfAnnotation {
                name: "new-one".to_string(),
                page: 1,
                kind: AnnotationKind::Note,
                rect: [2.0, 2.0, 3.0, 3.0],
                color: None,
                contents: None,
                author: None,
                created: None,
                modified: None,
                status: None,
                quad_points: Vec::new(),
                ink_paths: Vec::new(),
                line: None,
            },
        ];

        let existing: std::collections::HashSet<String> =
            ["already-here".to_string()].into_iter().collect();
        let mut inserted = Vec::new();
        let report = merge(parsed, &existing, ImportReport::default(), |annotation| {
            inserted.push(annotation.name.clone());
            Ok(())
        })
        .unwrap();

        assert_eq!(report.imported, 1);
        assert_eq!(report.skipped_existing, 1);
        assert_eq!(report.total_skipped(), 1);
        // The existing one must not have been written, not merely not counted.
        assert_eq!(inserted, vec!["new-one".to_string()]);
    }

    #[test]
    fn merge_of_an_exported_file_imports_nothing_the_second_time() {
        // The risk table's promise: a repeat import is a no-op that reports its count.
        let rows = vec![
            row(
                "p",
                0,
                AnnotationKind::Highlight,
                highlight_blob(&[[1.0, 1.0, 2.0, 2.0]]),
                None,
            ),
            row(
                "q",
                1,
                AnnotationKind::Note,
                serde_json::json!({"v":2,"point":[1.0,1.0]}),
                Some("x"),
            ),
        ];
        let (xml, _) = export(&rows).unwrap();
        let (parsed, report) = parse(&xml).unwrap();

        // The document already holds both rows.
        let existing: std::collections::HashSet<String> =
            ["p".to_string(), "q".to_string()].into_iter().collect();
        let mut inserted = 0;
        let report = merge(parsed, &existing, report, |_| {
            inserted += 1;
            Ok(())
        })
        .unwrap();

        assert_eq!(inserted, 0, "a repeat import wrote rows");
        assert_eq!(report.imported, 0);
        assert_eq!(report.skipped_existing, 2);
    }

    #[test]
    fn merge_into_an_empty_document_imports_everything() {
        let rows = vec![row(
            "only",
            0,
            AnnotationKind::Highlight,
            highlight_blob(&[[1.0, 1.0, 2.0, 2.0]]),
            None,
        )];
        let (xml, _) = export(&rows).unwrap();
        let (parsed, report) = parse(&xml).unwrap();
        let empty = std::collections::HashSet::new();
        let mut inserted = 0;
        let report = merge(parsed, &empty, report, |_| {
            inserted += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(inserted, 1);
        assert_eq!(report.imported, 1);
        assert_eq!(report.total_skipped(), 0);
    }

    #[test]
    fn a_quad_round_trips_back_into_per_line_rects() {
        // The quads exist so a multi-line highlight keeps its per-line rectangles. If the
        // import only rebuilt the bounding box, a two-line selection would become one block
        // covering the gap between the lines.
        let (xml, _) = export(&[row(
            "h",
            0,
            AnnotationKind::Highlight,
            highlight_blob(&[[10.0, 10.0, 20.0, 12.0], [10.0, 14.0, 40.0, 16.0]]),
            None,
        )])
        .unwrap();
        let (parsed, _) = parse(&xml).unwrap();

        let blob = blob_for(&parsed[0]);
        let rects = blob["rects"].as_array().expect("no rects rebuilt");
        assert_eq!(rects.len(), 2, "per-line rects collapsed: {blob}");
        assert_eq!(rects[0], serde_json::json!([10.0, 10.0, 20.0, 12.0]));
        assert_eq!(rects[1], serde_json::json!([10.0, 14.0, 40.0, 16.0]));
    }

    #[test]
    fn a_date_without_a_time_is_accepted() {
        let seconds = parse_date("D:20231114").unwrap();
        assert_eq!(format_date(seconds), "D:20231114000000");
    }
}

/// Form *data* interchange, as distinct from the annotation interchange above.
///
/// R7 (2026-11-14) ruled XFDF in and FDF out. The two live in one module because they share a
/// namespace, a root element and a parser's worth of entity handling; they do not share a shape,
/// which is why the types are separate rather than one type with an `is_form` flag.
pub mod form_data {
    use std::collections::BTreeMap;

    use quick_xml::events::{BytesDecl, BytesStart, BytesText, Event};
    use quick_xml::{Reader, Writer};

    use crate::model::AppError;

    /// One field's value from an XFDF file.
    ///
    /// Fields arrive in document order and duplicates are possible: Acrobat writes one `<field>`
    /// per widget in a radio group, all naming the same field with different states. Collapsing
    /// them here would hide a document whose two widgets disagree, so the reader keeps every one
    /// and [`values`] resolves them with a rule the caller can see.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FieldValue {
        /// The fully qualified name, as the XFDF `<field name="...">` attribute spells it.
        pub name: String,
        /// The `<value>` text. Empty when the element carried none, which for a checkbox means
        /// "not ticked" rather than "unknown".
        pub value: String,
    }

    /// What an import produced, and what it had to leave behind.
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct FormDataReport {
        /// Names whose value was taken.
        pub imported: usize,
        /// Names that appeared more than once and were collapsed. Counted, not silently merged:
        /// a radio group legitimately produces these, a corrupt file also does.
        pub collapsed_duplicates: usize,
        /// `<field>` elements with no `name` attribute. Unaddressable, so unusable.
        pub skipped_nameless: usize,
    }

    /// Refuses any input that is not XFDF, by name, before parsing it.
    ///
    /// FDF is a *binary* container whose first bytes are `%FDF-1.x`. Detecting it by content rather
    /// than by file extension is deliberate: a user who renames a `.fdf` to `.xfdf` has not changed
    /// what the bytes are, and refusing on extension would let that file through to a parser that
    /// then reports a confusing XML syntax error.
    pub fn refuse_non_xfdf(bytes: &[u8]) -> Result<(), AppError> {
        let head = &bytes[..bytes.len().min(8)];
        if head.starts_with(b"%FDF-") {
            return Err(AppError::FdfUnsupported);
        }
        Ok(())
    }

    /// Parses XFDF form data into the pairs it carries, in document order.
    ///
    /// `<fields>` is the only section read. `<annots>` is P3.1's, and a file carrying both is
    /// exactly what a viewer exports from a document with annotations and a form, so ignoring it
    /// here is correct rather than lenient.
    pub fn parse(xml: &str) -> Result<(Vec<FieldValue>, FormDataReport), AppError> {
        refuse_non_xfdf(xml.as_bytes())?;

        let mut reader = Reader::from_str(xml);
        // Deliberately *not* `trim_text(true)`, for the reason the annotation parser above records:
        // quick-xml delivers text and entity references as separate events when adjacent, so
        // trimming each node eats the spaces around an `&amp;` and "a & b" comes back as "a&b".
        // The buffer is trimmed once, when the field closes.
        reader.config_mut().trim_text(false);

        let mut values = Vec::new();
        let mut report = FormDataReport::default();
        let mut in_fields = false;
        let mut current_name: Option<String> = None;
        // Separate from `current_name` so a nameless `<field>` is still recognised as a field when
        // it closes; keying the end branch on the name loses exactly the case that needs counting.
        let mut in_field = false;
        let mut current_value = String::new();
        let mut in_value = false;

        loop {
            match reader.read_event() {
                Err(error) => {
                    return Err(AppError::Other(format!(
                        "this file is not readable as XFDF: {error}"
                    )))
                }
                Ok(Event::Eof) => break,
                Ok(Event::Start(event)) => match event.name().as_ref() {
                    "fields" => in_fields = true,
                    "field" if in_fields => {
                        in_field = true;
                        current_name = attribute(&event, "name");
                        current_value.clear();
                    }
                    "value" if in_field => in_value = true,
                    _ => {}
                },
                Ok(Event::Empty(event)) if in_fields && event.name().as_ref() == "field" => {
                    // A self-closing <field name="x"/> is Acrobat's spelling for an empty value.
                    finish_field(
                        attribute(&event, "name"),
                        String::new(),
                        &mut values,
                        &mut report,
                    );
                }
                Ok(Event::Text(event)) => {
                    if in_value {
                        current_value.push_str(
                            &quick_xml::escape::unescape(event.as_ref()).unwrap_or_default(),
                        );
                    }
                }
                Ok(Event::GeneralRef(reference)) => {
                    // Entities arrive as their own events, not as text: without this arm "a & b"
                    // reads back as "a  b", with the character simply missing. Same trap the
                    // annotation parser above records, and the same helper resolves it.
                    if in_value {
                        if let Some(character) =
                            super::reference_char(reference.into_inner().as_ref())
                        {
                            current_value.push(character);
                        }
                    }
                }
                Ok(Event::End(event)) => match event.name().as_ref() {
                    "value" => in_value = false,
                    "field" if in_field => {
                        in_field = false;
                        in_value = false;
                        finish_field(
                            current_name.take(),
                            std::mem::take(&mut current_value).trim().to_string(),
                            &mut values,
                            &mut report,
                        );
                    }
                    "fields" => in_fields = false,
                    _ => {}
                },
                Ok(_) => {}
            }
        }

        Ok((values, report))
    }

    fn finish_field(
        name: Option<String>,
        value: String,
        values: &mut Vec<FieldValue>,
        report: &mut FormDataReport,
    ) {
        match name {
            Some(name) if !name.trim().is_empty() => values.push(FieldValue {
                name: name.trim().to_string(),
                value,
            }),
            _ => report.skipped_nameless += 1,
        }
    }

    fn attribute(event: &BytesStart<'_>, wanted: &str) -> Option<String> {
        event
            .attributes()
            .flatten()
            .find(|attribute| attribute.key.as_ref() == wanted)
            .map(|attribute| attribute.value.into_owned())
    }

    /// One value per name: the last one wins, and the number of collapses is reported.
    ///
    /// Last-wins rather than first-wins because Acrobat writes the *current* state last in a radio
    /// group whose earlier widgets were also on. The report is what keeps that choice visible —
    /// without it, "one name produced two values" is a fact only this function knows.
    pub fn values(parsed: &[FieldValue], report: &mut FormDataReport) -> BTreeMap<String, String> {
        let mut resolved = BTreeMap::new();
        for field in parsed {
            if resolved
                .insert(field.name.clone(), field.value.clone())
                .is_some()
            {
                report.collapsed_duplicates += 1;
            }
        }
        report.imported = resolved.len();
        resolved
    }

    /// Writes form values as XFDF, so a fill can be handed to another document or another tool.
    pub fn export(values: &BTreeMap<String, String>) -> Result<String, AppError> {
        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
        write(
            &mut writer,
            Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)),
        )?;

        let mut root = BytesStart::new("xfdf");
        root.push_attribute(("xmlns", super::XFDF_NAMESPACE));
        write(&mut writer, Event::Start(root))?;
        write(&mut writer, Event::Start(BytesStart::new("fields")))?;

        for (name, value) in values {
            let mut field = BytesStart::new("field");
            field.push_attribute(("name", name.as_str()));
            write(&mut writer, Event::Start(field))?;
            write(&mut writer, Event::Start(BytesStart::new("value")))?;
            write(&mut writer, Event::Text(BytesText::new(value)))?;
            write(
                &mut writer,
                Event::End(quick_xml::events::BytesEnd::new("value")),
            )?;
            write(
                &mut writer,
                Event::End(quick_xml::events::BytesEnd::new("field")),
            )?;
        }

        write(
            &mut writer,
            Event::End(quick_xml::events::BytesEnd::new("fields")),
        )?;
        write(
            &mut writer,
            Event::End(quick_xml::events::BytesEnd::new("xfdf")),
        )?;

        String::from_utf8(writer.into_inner()).map_err(|error| {
            AppError::Other(format!("XFDF export produced invalid UTF-8: {error}"))
        })
    }

    fn write(writer: &mut Writer<Vec<u8>>, event: Event<'_>) -> Result<(), AppError> {
        writer
            .write_event(event)
            .map_err(|error| AppError::Other(format!("could not write XFDF: {error}")))
    }
}
