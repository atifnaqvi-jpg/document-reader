//! Turns stored annotations into real PDF annotation objects (P2-D3/D8).
//!
//! The database is the source of truth; the PDF file is a projection of it. So
//! "save my edits" is: load the original bytes, rebuild *every* annotation for the
//! document from the DB, and write the result. That is deliberately not an
//! incremental "apply the pending change" path -- rebuilding is idempotent, so a
//! failed or repeated save converges on the same file instead of stacking
//! duplicate annotations on top of each other.
//!
//! Annotations the user made in *another* PDF application are left alone: they are
//! not in our DB, and P2 has no business deleting someone else's work.

use lopdf::Document;

use crate::editor::geometry::PdfRect;
use crate::editor::lopdf_write;
use crate::model::{Annotation, AnnotationKind, AppError};

/// The `data` blob shape this module understands. `v` is explicit so a future
/// tool can change the geometry without guessing at older rows.
const SUPPORTED_BLOB_VERSION: u32 = 1;

/// A highlight with no stored opacity: matches the UI's default so a blob missing
/// the field renders the same as one that has it.
const DEFAULT_HIGHLIGHT_OPACITY: f32 = 0.4;

#[derive(serde::Deserialize)]
struct HighlightBlob {
    rects: Vec<[f32; 4]>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    opacity: Option<f32>,
}

#[derive(serde::Deserialize)]
struct InkBlob {
    strokes: Vec<Vec<[f32; 2]>>,
    #[serde(default)]
    color: Option<String>,
}

#[derive(serde::Deserialize)]
struct ShapeBlob {
    rect: [f32; 4],
    #[serde(default)]
    color: Option<String>,
}

/// Writes every annotation for one document into `bytes`, returning new bytes.
///
/// `bytes` must be the *original* document: applying this to an already-annotated
/// file would duplicate every annotation it wrote last time.
pub fn apply_annotations(bytes: &[u8], annotations: &[Annotation]) -> Result<Vec<u8>, AppError> {
    let mut document = Document::load_mem(bytes)
        .map_err(|error| AppError::Other(format!("could not read pdf: {error}")))?;

    let page_ids = document.get_pages();
    for annotation in annotations {
        // `get_pages()` is keyed by lopdf's **1-based** page number, while the DB and the
        // UI both use 0-based indices (the renderer's `page` is a 0-based index). Without
        // the +1 every annotation would land one page late -- and page 0's would silently
        // disappear, since lopdf has no page 0.
        let Some(page_id) = page_ids.get(&(annotation.page + 1)).copied() else {
            // An annotation pointing past the end of the file means the document
            // was replaced under a stable id. Skip it rather than writing it onto
            // an unrelated page.
            continue;
        };
        let Some(dictionary) = dictionary_for(annotation)? else {
            continue;
        };
        lopdf_write::attach_annotation(&mut document, page_id, dictionary)?;
    }

    lopdf_write::save_to_bytes(&mut document)
}

/// Builds an annotation dictionary from a stored row, or `None` if the row's
/// geometry is not writeable as a PDF annotation (e.g. a zero-area mark).
fn dictionary_for(annotation: &Annotation) -> Result<Option<lopdf::Dictionary>, AppError> {
    let blob: serde_json::Value = serde_json::from_str(&annotation.data).map_err(|error| {
        AppError::Other(format!(
            "annotation {} has malformed data: {error}",
            annotation.id
        ))
    })?;

    // A blob from a newer build is not an error we can act on, but writing it
    // with the wrong interpretation would place a mark in the wrong place.
    let version = blob
        .get("v")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    if version > SUPPORTED_BLOB_VERSION as u64 {
        return Ok(None);
    }

    let contents = annotation.text.as_deref();

    match annotation.kind {
        AnnotationKind::Highlight | AnnotationKind::Underline | AnnotationKind::Strikeout => {
            let parsed: HighlightBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            let color = parse_color(parsed.color.as_deref());
            let opacity = parsed.opacity.unwrap_or(DEFAULT_HIGHLIGHT_OPACITY);
            let Some(first) = parsed.rects.first() else {
                return Ok(None);
            };
            let rect = rect_from(first);

            // Multi-rect highlights (a selection spanning lines) are written as one
            // annotation per rect with an /IRT parent link in a full implementation;
            // P2 writes the union, which every viewer renders as a single highlight.
            let union = parsed
                .rects
                .iter()
                .skip(1)
                .fold(rect, |acc, next| union(acc, rect_from(next)));

            Ok(Some(lopdf_write::annotation_dictionary(
                annotation.kind,
                union,
                color,
                opacity,
                contents,
            )?))
        }
        AnnotationKind::Note => {
            // A note's anchor is its /Rect; the blob for a note stores a point.
            let parsed: serde_json::Value = blob;
            let point = parsed
                .get("point")
                .and_then(|point| {
                    let array = point.as_array()?;
                    Some((
                        array.first()?.as_f64()? as f32,
                        array.get(1)?.as_f64()? as f32,
                    ))
                })
                .or_else(|| {
                    parsed.get("rect").and_then(|rect| {
                        let array = rect.as_array()?;
                        Some((
                            array.first()?.as_f64()? as f32,
                            array.get(1)?.as_f64()? as f32,
                        ))
                    })
                });
            let Some((x, y)) = point else {
                return Ok(None);
            };
            let anchor = PdfRect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            };
            let color = parse_color(parsed.get("color").and_then(serde_json::Value::as_str));
            // `is_drawable` is false for a bare point, so the note path must not go
            // through the rect guard: the dictionary builder gives it an icon box.
            Ok(Some(lopdf_write::annotation_dictionary(
                AnnotationKind::Note,
                PdfRect {
                    x0: anchor.x0,
                    y0: anchor.y0,
                    // 1x1 is enough for the builder to accept it and then expand
                    // to the conventional 20x20 icon.
                    x1: anchor.x0 + 1.0,
                    y1: anchor.y0 + 1.0,
                },
                color,
                1.0,
                contents,
            )?))
        }
        AnnotationKind::Shape => {
            let parsed: ShapeBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            Ok(Some(lopdf_write::annotation_dictionary(
                AnnotationKind::Shape,
                rect_from(&parsed.rect),
                parse_color(parsed.color.as_deref()),
                1.0,
                contents,
            )?))
        }
        AnnotationKind::Ink => {
            let parsed: InkBlob = serde_json::from_value(blob).map_err(|error| {
                AppError::Other(format!("annotation {}: {error}", annotation.id))
            })?;
            let strokes: Vec<Vec<(f32, f32)>> = parsed
                .strokes
                .iter()
                .map(|stroke| stroke.iter().map(|point| (point[0], point[1])).collect())
                .collect();
            // A stroke with fewer than two points cannot be drawn; the builder
            // filters those and errors if nothing is left.
            // Stroke count is checked before the rect: a single tapped point
            // produces a zero-area bounds, and reporting that as "too small" would
            // misdescribe an ink row that simply has nothing to draw.
            if !strokes.iter().any(|stroke| stroke.len() >= 2) {
                return Ok(None);
            }
            let rect = strokes_bounds(&strokes);
            match lopdf_write::ink_annotation_dictionary(
                rect,
                &strokes,
                parse_color(parsed.color.as_deref()),
                1.0,
                contents,
            ) {
                Ok(dictionary) => Ok(Some(dictionary)),
                Err(error) if error.to_string().contains("no drawable strokes") => Ok(None),
                Err(error) => Err(error),
            }
        }
    }
}

fn rect_from(rect: &[f32; 4]) -> PdfRect {
    PdfRect {
        x0: rect[0],
        y0: rect[1],
        x1: rect[2],
        y1: rect[3],
    }
}

fn union(a: PdfRect, b: PdfRect) -> PdfRect {
    PdfRect {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    }
}

fn strokes_bounds(strokes: &[Vec<(f32, f32)>]) -> PdfRect {
    let mut rect = PdfRect {
        x0: f32::MAX,
        y0: f32::MAX,
        x1: f32::MIN,
        y1: f32::MIN,
    };
    for (x, y) in strokes.iter().flatten() {
        rect.x0 = rect.x0.min(*x);
        rect.y0 = rect.y0.min(*y);
        rect.x1 = rect.x1.max(*x);
        rect.y1 = rect.y1.max(*y);
    }
    if rect.x0 > rect.x1 {
        // No points at all; a degenerate rect that the builder will reject.
        return PdfRect {
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        };
    }
    rect
}

/// Parses `#rrggbb` into PDF's 0..1 component triple. An unparseable colour falls
/// back to yellow rather than failing the save: losing a highlight's colour is a
/// smaller harm than losing the user's whole edit session.
fn parse_color(color: Option<&str>) -> (f32, f32, f32) {
    const DEFAULT: (f32, f32, f32) = (1.0, 0.95, 0.69);
    let Some(color) = color else {
        return DEFAULT;
    };
    let hex = color.trim_start_matches('#');
    if hex.len() != 6 {
        return DEFAULT;
    }
    let parse = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16)
            .ok()
            .map(|value| value as f32 / 255.0)
    };
    match (parse(0..2), parse(2..4), parse(4..6)) {
        (Some(r), Some(g), Some(b)) => (r, g, b),
        _ => DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::dictionary;
    use lopdf::Object;

    fn sample_pdf() -> Vec<u8> {
        two_page_pdf(1)
    }

    /// A minimal valid PDF with `pages` pages, so page-number mapping can be tested.
    fn two_page_pdf(pages: usize) -> Vec<u8> {
        let mut document = Document::with_version("1.4");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("body")]),
                Operation::new("ET", vec![]),
            ],
        };
        let mut kids = Vec::new();
        for _ in 0..pages {
            // A fresh content stream per page: sharing one is legal but makes the
            // fixture harder to reason about when a test asserts on page contents.
            let content_id = document.add_object(lopdf::Stream::new(
                dictionary! {},
                content.encode().unwrap(),
            ));
            let page_id = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
                "Contents" => content_id,
                "Resources" => resources_id,
            });
            kids.push(page_id.into());
        }
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => pages as i64,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document.renumber_objects();

        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn annotation(page: u32, kind: AnnotationKind, data: &str, text: Option<&str>) -> Annotation {
        Annotation {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: "d1".to_string(),
            page,
            kind,
            data: data.to_string(),
            text: text.map(str::to_string),
            created_at: 0,
            modified_at: 0,
        }
    }

    fn annotation_count(bytes: &[u8]) -> usize {
        let document = Document::load_mem(bytes).unwrap();
        document
            .get_pages()
            .values()
            .map(|page_id| lopdf_write::page_annotation_count(&document, *page_id))
            .sum()
    }

    #[test]
    fn a_highlight_survives_the_round_trip() {
        // P2 definition of done #4, in miniature: annotate -> save -> re-open.
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]],"color":"#fff3b0"}"##,
            None,
        )];

        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 1);
    }

    #[test]
    fn every_tool_writes_an_annotation() {
        let rows = vec![
            annotation(
                0,
                AnnotationKind::Highlight,
                r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]],"color":"#ffff00"}"##,
                None,
            ),
            annotation(
                0,
                AnnotationKind::Underline,
                r##"{"v":1,"rects":[[72.0,680.0,300.0,700.0]],"color":"#0000ff"}"##,
                None,
            ),
            annotation(
                0,
                AnnotationKind::Strikeout,
                r##"{"v":1,"rects":[[72.0,660.0,300.0,680.0]],"color":"#ff0000"}"##,
                None,
            ),
            annotation(
                0,
                AnnotationKind::Note,
                r##"{"v":1,"point":[80.0,640.0],"color":"#ffcc00"}"##,
                Some("a remark"),
            ),
            annotation(
                0,
                AnnotationKind::Shape,
                r##"{"v":1,"rect":[72.0,600.0,300.0,640.0],"color":"#00ff00"}"##,
                None,
            ),
            annotation(
                0,
                AnnotationKind::Ink,
                r##"{"v":1,"strokes":[[[72.0,580.0],[120.0,600.0]]],"color":"#000000"}"##,
                None,
            ),
        ];

        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 6);
    }

    #[test]
    fn rebuilding_is_idempotent_when_starting_from_the_original() {
        // Saving twice from the same source and DB state must not double up.
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];
        let original = sample_pdf();

        let first = apply_annotations(&original, &rows).unwrap();
        let second = apply_annotations(&original, &rows).unwrap();
        assert_eq!(annotation_count(&first), 1);
        assert_eq!(annotation_count(&second), 1);
        assert_eq!(first, second, "same inputs must produce identical bytes");
    }

    #[test]
    fn applying_to_already_annotated_bytes_duplicates() {
        // Documents the trap the doc comment warns about: `bytes` must be the
        // original. This is the behaviour, asserted so it is not a surprise.
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];
        let once = apply_annotations(&sample_pdf(), &rows).unwrap();
        let twice = apply_annotations(&once, &rows).unwrap();
        assert_eq!(annotation_count(&twice), 2);
    }

    #[test]
    fn an_annotation_past_the_last_page_is_skipped() {
        // Guards against a stale row landing on an unrelated page.
        let rows = vec![annotation(
            5,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn a_newer_blob_version_is_ignored_rather_than_misread() {
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":99,"rects":[[1.0,2.0,3.0,4.0]]}"##,
            None,
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn malformed_json_is_reported_with_the_annotation_id() {
        let row = annotation(0, AnnotationKind::Highlight, "{not json", None);
        let id = row.id.clone();
        let error = apply_annotations(&sample_pdf(), &[row]).unwrap_err();
        assert!(error.to_string().contains(&id));
    }

    #[test]
    fn multi_line_highlight_is_written_as_one_union_rect() {
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0],[72.0,660.0,300.0,680.0]]}"##,
            None,
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();

        let document = Document::load_mem(&bytes).unwrap();
        let page_id = *document.get_pages().values().next().unwrap();
        let annotations = document.get_page_annotations(page_id).unwrap();
        assert_eq!(annotations.len(), 1);
        let rect = annotations[0].get(b"Rect").unwrap().as_array().unwrap();
        let values: Vec<f32> = rect.iter().map(|o| o.as_float().unwrap()).collect();
        assert_eq!(
            values,
            vec![72.0, 660.0, 300.0, 720.0],
            "union of both rects"
        );
    }

    #[test]
    fn contents_are_carried_into_the_pdf() {
        let rows = vec![annotation(
            0,
            AnnotationKind::Note,
            r##"{"v":1,"point":[80.0,640.0]}"##,
            Some("remember this"),
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();

        let document = Document::load_mem(&bytes).unwrap();
        let page_id = *document.get_pages().values().next().unwrap();
        let annotations = document.get_page_annotations(page_id).unwrap();
        assert_eq!(
            annotations[0]
                .get(b"Contents")
                .and_then(Object::as_str)
                .unwrap(),
            b"remember this"
        );
    }

    #[test]
    fn an_empty_annotation_list_returns_a_valid_document() {
        let bytes = apply_annotations(&sample_pdf(), &[]).unwrap();
        assert!(Document::load_mem(&bytes).is_ok());
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn pre_existing_foreign_annotations_are_preserved() {
        // An annotation made in another app is not in our DB and must survive.
        let original = sample_pdf();
        let mut document = Document::load_mem(&original).unwrap();
        let page_id = *document.get_pages().values().next().unwrap();
        let foreign = lopdf_write::annotation_dictionary(
            AnnotationKind::Highlight,
            PdfRect {
                x0: 10.0,
                y0: 10.0,
                x1: 100.0,
                y1: 30.0,
            },
            (0.0, 1.0, 0.0),
            0.4,
            None,
        )
        .unwrap();
        lopdf_write::attach_annotation(&mut document, page_id, foreign).unwrap();
        let with_foreign = lopdf_write::save_to_bytes(&mut document).unwrap();

        let rows = vec![annotation(
            0,
            AnnotationKind::Underline,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];
        let bytes = apply_annotations(&with_foreign, &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 2);
    }

    #[test]
    fn colour_parsing_handles_hex_and_falls_back_safely() {
        assert_eq!(parse_color(Some("#ff0000")), (1.0, 0.0, 0.0));
        assert_eq!(parse_color(Some("00ff00")), (0.0, 1.0, 0.0));
        assert_eq!(parse_color(None), (1.0, 0.95, 0.69));
        assert_eq!(parse_color(Some("nonsense")), (1.0, 0.95, 0.69));
        assert_eq!(parse_color(Some("#fff")), (1.0, 0.95, 0.69));
    }

    #[test]
    fn an_ink_row_with_no_drawable_strokes_is_skipped_not_fatal() {
        let rows = vec![annotation(
            0,
            AnnotationKind::Ink,
            r##"{"v":1,"strokes":[[[10.0,10.0]]]}"##,
            None,
        )];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn a_note_without_a_point_is_skipped() {
        let rows = vec![annotation(1, AnnotationKind::Note, r##"{"v":1}"##, None)];
        let bytes = apply_annotations(&sample_pdf(), &rows).unwrap();
        assert_eq!(annotation_count(&bytes), 0);
    }

    #[test]
    fn corrupted_input_bytes_are_reported() {
        let error = apply_annotations(b"not a pdf at all", &[]).unwrap_err();
        assert!(error.to_string().contains("could not read pdf"));
    }

    #[test]
    fn page_zero_maps_to_the_first_pdf_page() {
        // Regression: the DB/UI use 0-based page indices, lopdf's get_pages() is 1-based.
        // Getting this wrong puts every annotation one page late and silently loses the
        // first page's entirely, so this asserts the *location*, not just a count.
        let rows = vec![annotation(
            0,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];

        let bytes = apply_annotations(&two_page_pdf(2), &rows).unwrap();

        let document = Document::load_mem(&bytes).unwrap();
        let pages = document.get_pages();
        let first = pages.get(&1).copied().unwrap();
        let second = pages.get(&2).copied().unwrap();
        assert_eq!(lopdf_write::page_annotation_count(&document, first), 1);
        assert_eq!(
            lopdf_write::page_annotation_count(&document, second),
            0,
            "annotation belongs to page 0, not page 1"
        );
    }

    #[test]
    fn the_last_page_is_addressable() {
        let rows = vec![annotation(
            1,
            AnnotationKind::Highlight,
            r##"{"v":1,"rects":[[72.0,700.0,300.0,720.0]]}"##,
            None,
        )];

        let bytes = apply_annotations(&two_page_pdf(2), &rows).unwrap();
        let document = Document::load_mem(&bytes).unwrap();
        let pages = document.get_pages();
        let second = pages.get(&2).copied().unwrap();
        assert_eq!(lopdf_write::page_annotation_count(&document, second), 1);
    }

    #[test]
    fn one_annotation_per_page_lands_on_its_own_page() {
        let rows = vec![
            annotation(
                0,
                AnnotationKind::Highlight,
                r##"{"v":1,"rects":[[10.0,700.0,60.0,720.0]]}"##,
                None,
            ),
            annotation(
                1,
                AnnotationKind::Highlight,
                r##"{"v":1,"rects":[[20.0,600.0,70.0,620.0]]}"##,
                None,
            ),
        ];

        let bytes = apply_annotations(&two_page_pdf(2), &rows).unwrap();
        let document = Document::load_mem(&bytes).unwrap();
        let pages = document.get_pages();
        let first = pages.get(&1).copied().unwrap();
        let second = pages.get(&2).copied().unwrap();

        let rect_of = |page_id| {
            document.get_page_annotations(page_id).unwrap()[0]
                .get(b"Rect")
                .unwrap()
                .as_array()
                .unwrap()[0]
                .as_float()
                .unwrap()
        };
        assert_eq!(rect_of(first), 10.0);
        assert_eq!(rect_of(second), 20.0);
    }
}
