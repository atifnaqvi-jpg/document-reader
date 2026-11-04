//! Writing real PDF annotation objects via `lopdf` (P2-D3).
//!
//! Why not PDFium: `pdfium-render`'s annotation *creation* path is `pub(crate)`
//! (verified in 0.8.37), so external code cannot make annotations through it.
//! PDFium *reads and renders* annotations, so it stays the renderer; `lopdf` is
//! the writer. The two meet here: this module produces bytes that PDFium can
//! then open and draw.
//!
//! What is deliberately **not** done here: page content stamping. A stamp has no
//! annotation object, cannot be selected, cannot be removed by another viewer,
//! and survives "delete annotation" -- so stamps are used only for live
//! rendering, never for persistence (P2-D3).

use lopdf::{dictionary, Dictionary, Document, Object, ObjectId};

use crate::editor::annotate::appearance;
use crate::editor::geometry::PdfRect;
use crate::model::{AnnotationKind, AppError};

/// Builds an annotation dictionary for one mark (ISO 32000-1 12.5.6).
///
/// All four subtypes are "markup" annotations and share `/Rect` + `/QuadPoints`;
/// only `/Subtype` and the appearance differ, so one builder covers them and a
/// new markup tool is a new arm rather than a new code path.
pub fn annotation_dictionary(
    document: &mut Document,
    kind: AnnotationKind,
    rect: PdfRect,
    color: (f32, f32, f32),
    opacity: f32,
    contents: Option<&str>,
) -> Result<Dictionary, AppError> {
    let rect = rect.normalised();
    if !rect.is_drawable() {
        return Err(AppError::Other(
            "annotation area is too small to draw".to_string(),
        ));
    }

    let rect_array = vec![
        Object::Real(rect.x0),
        Object::Real(rect.y0),
        Object::Real(rect.x1),
        Object::Real(rect.y1),
    ];

    let mut dictionary = match kind {
        AnnotationKind::Highlight
        | AnnotationKind::Underline
        | AnnotationKind::Strikeout
        | AnnotationKind::Squiggly => {
            let subtype = match kind {
                AnnotationKind::Highlight => "Highlight",
                AnnotationKind::Underline => "Underline",
                AnnotationKind::Strikeout => "Strikeout",
                AnnotationKind::Squiggly => "Squiggly",
                _ => unreachable!("arm is restricted to the markup kinds"),
            };
            dictionary! {
                "Type" => "Annot",
                "Subtype" => subtype,
                "Rect" => Object::Array(rect_array),
                "QuadPoints" => Object::Array(
                    rect.quad_points_array().into_iter().map(Object::Real).collect(),
                ),
                "C" => Object::Array(vec![
                    Object::Real(color.0),
                    Object::Real(color.1),
                    Object::Real(color.2),
                ]),
                "F" => Object::Integer(4), // Print
            }
        }
        AnnotationKind::Note => {
            // A sticky note is anchored at a point, but /Rect must still be a
            // real box for the icon to be clickable. 20x20 points is the
            // conventional icon size.
            let icon = PdfRect {
                x0: rect.x0,
                y0: rect.y0,
                x1: rect.x0 + 20.0,
                y1: rect.y0 + 20.0,
            }
            .normalised();
            dictionary! {
                "Type" => "Annot",
                "Subtype" => "Text",
                "Rect" => Object::Array(vec![
                    Object::Real(icon.x0),
                    Object::Real(icon.y0),
                    Object::Real(icon.x1),
                    Object::Real(icon.y1),
                ]),
                "Name" => Object::Name(b"Comment".to_vec()),
                "C" => Object::Array(vec![
                    Object::Real(color.0),
                    Object::Real(color.1),
                    Object::Real(color.2),
                ]),
                "F" => Object::Integer(4),
            }
        }
        AnnotationKind::Rect => dictionary! {
            "Type" => "Annot",
            "Subtype" => "Square",
            "Rect" => Object::Array(rect_array),
            "C" => Object::Array(vec![
                Object::Real(color.0),
                Object::Real(color.1),
                Object::Real(color.2),
            ]),
            "F" => Object::Integer(4),
        },
        // Ink needs a `/InkList` of stroke paths; it is stored in the JSON blob
        // and written by `annotation_dictionary_for_ink` instead.
        AnnotationKind::Ink => {
            return Err(AppError::Other(
                "ink annotations are written from their stroke list".to_string(),
            ))
        }
        // P3.1 rows land one type at a time. Refusing is the only honest answer for a kind
        // whose dictionary builder has not been written: a wrong subtype would put a shape on
        // the page that is not what the user drew, and the save would report success.
        // Free text needs an appearance stream (D-A8): no viewer synthesises a text box from
        // a rect, so the text has to be drawn into `/AP /N` or it is invisible outside Adobe.
        // The dictionary still records `/Rect`, `/C` and `/DA` -- `/DA` is what an editor
        // reopening this annotation uses when the user edits the text.
        AnnotationKind::FreeText(mode) => {
            use crate::editor::annotate::appearance;

            // `contents` is the text. A free-text annotation with no text is a frame with
            // nothing in it, so it is refused rather than written: the user would see an
            // empty box they cannot explain.
            let text = contents.unwrap_or_default();
            if text.trim().is_empty() {
                return Err(AppError::Other(
                    "a free-text annotation needs text".to_string(),
                ));
            }

            let width = rect.width();
            let height = rect.height();
            let appearance_stream =
                appearance::free_text_appearance(text, width, height, color, mode);
            let appearance_id = document.add_object(appearance_stream);

            // `/DA` is resolved against the annotation's own `/DR`, not against the page and
            // not against the appearance stream's `/Resources` (ISO 32000-1 12.7.3.3). Without
            // `/DR` the `/Helv` in `/DA` names a font the editor cannot look up, so it would
            // fall back to a default and re-lay-out edited text in the wrong font and width.
            let font_id = document.add_object(appearance::free_text_font());

            dictionary! {
                "Type" => "Annot",
                "Subtype" => "FreeText",
                "Rect" => rect_array,
                // The text is stored in both `/Contents` (what a comment pane lists) and the
                // appearance (what the page shows). They are written from the same string, so
                // they cannot drift.
                "Contents" => Object::string_literal(text),
                "C" => Object::Array(vec![
                    Object::Real(color.0),
                    Object::Real(color.1),
                    Object::Real(color.2),
                ]),
                "DA" => Object::string_literal(appearance::FREE_TEXT_DA),
                "DR" => dictionary! {
                    "Font" => dictionary! {
                        appearance::FREE_TEXT_FONT => Object::Reference(font_id),
                    },
                },
                "F" => Object::Integer(4),
                "AP" => dictionary! {
                    "N" => Object::Reference(appearance_id),
                },
            }
        }
        // Still refusing. Each is its own row (A-5, A-6): a wrong subtype would put a mark on
        // the page that is not what the user drew, and the save would report success.
        AnnotationKind::Line { .. } | AnnotationKind::Oval => {
            return Err(AppError::Other(format!(
                "`{}` annotations are not written yet (P3.1)",
                kind.as_str()
            )))
        }
    };

    // /CA is the "constant alpha" used for the annotation's non-stroking areas,
    // which is what a highlight's fill uses. Clamped because a stored 0 would make
    // the mark invisible and a negative value is invalid.
    dictionary.set("CA", Object::Real(opacity.clamp(0.05, 1.0)));

    if let Some(text) = contents {
        if !text.trim().is_empty() {
            dictionary.set("Contents", Object::string_literal(text));
        }
    }

    Ok(dictionary)
}

/// Ink annotation from one or more stroke paths (ISO 32000-1 12.5.6.13).
/// `/InkList` is an array of arrays of x,y pairs in PDF points.
pub fn ink_annotation_dictionary(
    document: &mut Document,
    rect: PdfRect,
    strokes: &[Vec<(f32, f32)>],
    color: (f32, f32, f32),
    opacity: f32,
    contents: Option<&str>,
) -> Result<Dictionary, AppError> {
    let rect = rect.normalised();
    if !rect.is_drawable() {
        return Err(AppError::Other(
            "annotation area is too small to draw".to_string(),
        ));
    }

    let ink_list: Vec<Object> = strokes
        .iter()
        .filter(|stroke| stroke.len() >= 2)
        .map(|stroke| {
            Object::Array(
                stroke
                    .iter()
                    .flat_map(|(x, y)| [Object::Real(*x), Object::Real(*y)])
                    .collect(),
            )
        })
        .collect();

    if ink_list.is_empty() {
        return Err(AppError::Other(
            "ink annotation has no drawable strokes".to_string(),
        ));
    }

    // D-A8: ink needs an appearance stream. `/InkList` alone is drawn only by Adobe, so a
    // stroke without `/AP` saves, reopens, and is invisible in every other viewer -- the same
    // save-and-vanish failure that A-2 found in flattening, arrived at from the other side.
    let drawable: Vec<Vec<(f32, f32)>> = strokes
        .iter()
        .filter(|stroke| stroke.len() >= 2)
        .cloned()
        .collect();
    let appearance_stream =
        appearance::ink_appearance(&drawable, rect.width(), rect.height(), color);
    let appearance_id = document.add_object(appearance_stream);

    let mut dictionary = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Ink",
        "AP" => dictionary! {
            "N" => Object::Reference(appearance_id),
        },
        "Rect" => Object::Array(vec![
            Object::Real(rect.x0),
            Object::Real(rect.y0),
            Object::Real(rect.x1),
            Object::Real(rect.y1),
        ]),
        "InkList" => Object::Array(ink_list),
        "C" => Object::Array(vec![
            Object::Real(color.0),
            Object::Real(color.1),
            Object::Real(color.2),
        ]),
        "F" => Object::Integer(4),
    };

    dictionary.set("CA", Object::Real(opacity.clamp(0.05, 1.0)));

    if let Some(text) = contents {
        if !text.trim().is_empty() {
            dictionary.set("Contents", Object::string_literal(text));
        }
    }

    Ok(dictionary)
}

/// Attaches an annotation to a page, creating `/Annots` when absent.
///
/// The page's `/Annots` may legally be either an inline array or a reference to
/// one; both forms are handled, because rewriting a reference into an inline
/// array would silently orphan the object it pointed at.
pub fn attach_annotation(
    document: &mut Document,
    page_id: ObjectId,
    annotation: Dictionary,
) -> Result<ObjectId, AppError> {
    let annotation_id = document.add_object(annotation);
    let reference = Object::Reference(annotation_id);

    // Read /Annots out as a cheap clone so the page borrow ends here. Branching on
    // a live borrow and then mutating `document` inside one arm is what the
    // borrow checker (correctly) refuses.
    let existing = match document
        .get_object(page_id)
        .map_err(|error| AppError::Other(format!("page object unreadable: {error}")))?
        .as_dict()
        .map_err(|error| AppError::Other(format!("page object is not a dictionary: {error}")))?
        .get(b"Annots")
    {
        Ok(annots) => Some(annots.clone()),
        Err(_) => None,
    };

    match existing {
        Some(Object::Array(mut entries)) => {
            entries.push(reference);
            set_annots(document, page_id, Object::Array(entries))?;
        }
        Some(Object::Reference(annotations_id)) => {
            // Preserve the indirection: replacing the reference with an inline
            // array would orphan the object it pointed at, silently discarding
            // any annotations already stored there.
            document
                .get_object_mut(annotations_id)
                .map_err(|error| {
                    AppError::Other(format!("/Annots reference is dangling: {error}"))
                })?
                .as_array_mut()
                .map_err(|error| AppError::Other(format!("/Annots is not an array: {error}")))?
                .push(reference);
        }
        _ => {
            set_annots(document, page_id, Object::Array(vec![reference]))?;
        }
    }

    Ok(annotation_id)
}

fn set_annots(document: &mut Document, page_id: ObjectId, annots: Object) -> Result<(), AppError> {
    document
        .get_object_mut(page_id)
        .map_err(|error| AppError::Other(format!("page object unreadable: {error}")))?
        .as_dict_mut()
        .map_err(|error| AppError::Other(format!("page object is not a dictionary: {error}")))?
        .set(b"Annots", annots);
    Ok(())
}

/// Counts annotations on a page, used to verify a write round-trip.
pub fn page_annotation_count(document: &Document, page_id: ObjectId) -> usize {
    document
        .get_page_annotations(page_id)
        .map(|annotations| annotations.len())
        .unwrap_or(0)
}

/// Serialises a document to bytes.
pub fn save_to_bytes(document: &mut Document) -> Result<Vec<u8>, AppError> {
    let mut buffer = Vec::new();
    document
        .save_to(&mut buffer)
        .map_err(|error| AppError::Other(format!("could not write pdf: {error}")))?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::Stream;

    /// A real, openable one-page PDF (not a byte-string mock): the write path is
    /// only meaningful if PDFium can reopen what we produce.
    fn one_page_pdf() -> Document {
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
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal("hello")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id =
            document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Contents" => content_id,
            "Resources" => resources_id,
        });
        // `new_object_id` only reserves an id; the node must be inserted or the
        // catalog's /Pages reference would dangle and the file would have no pages.
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document.renumber_objects();
        document
    }

    fn first_page(document: &Document) -> ObjectId {
        document.get_pages().values().next().copied().unwrap()
    }

    /// Reads an array of numbers as `f32`, regardless of how the parser stored
    /// each entry. lopdf writes `72.0` as `Real` but re-reads a whole-number
    /// `Real` as `Integer`, so comparing `Object` variants directly would fail on
    /// a round-trip that is in fact correct.
    fn numbers(object: &Object) -> Vec<f32> {
        object
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_float().unwrap())
            .collect()
    }

    fn sample_rect() -> PdfRect {
        PdfRect {
            x0: 72.0,
            y0: 700.0,
            x1: 300.0,
            y1: 720.0,
        }
    }

    #[test]
    fn adds_annots_array_to_a_page_that_has_none() {
        let mut document = one_page_pdf();
        let page_id = first_page(&document);
        assert_eq!(page_annotation_count(&document, page_id), 0);

        let annotation = annotation_dictionary(
            &mut document,
            AnnotationKind::Highlight,
            sample_rect(),
            (1.0, 0.9, 0.2),
            0.4,
            None,
        )
        .unwrap();
        attach_annotation(&mut document, page_id, annotation).unwrap();

        assert_eq!(page_annotation_count(&document, page_id), 1);
    }

    #[test]
    fn appends_to_an_existing_annots_array() {
        let mut document = one_page_pdf();
        let page_id = first_page(&document);
        for _ in 0..3 {
            let annotation = annotation_dictionary(
                &mut document,
                AnnotationKind::Highlight,
                sample_rect(),
                (1.0, 0.9, 0.2),
                0.4,
                None,
            )
            .unwrap();
            attach_annotation(&mut document, page_id, annotation).unwrap();
        }
        assert_eq!(page_annotation_count(&document, page_id), 3);
    }

    #[test]
    fn round_trips_through_save_and_reload() {
        let mut document = one_page_pdf();
        let page_id = first_page(&document);
        let annotation = annotation_dictionary(
            &mut document,
            AnnotationKind::Highlight,
            sample_rect(),
            (1.0, 0.9, 0.2),
            0.4,
            Some("important"),
        )
        .unwrap();
        attach_annotation(&mut document, page_id, annotation).unwrap();

        let bytes = save_to_bytes(&mut document).unwrap();
        let reloaded = Document::load_mem(&bytes).unwrap();
        let reloaded_page = first_page(&reloaded);
        let annotations = reloaded.get_page_annotations(reloaded_page).unwrap();

        assert_eq!(annotations.len(), 1);
        let annotation = annotations[0];
        assert_eq!(
            annotation
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Highlight"
        );
        assert_eq!(
            numbers(annotation.get(b"Rect").unwrap()),
            vec![72.0, 700.0, 300.0, 720.0]
        );
        assert_eq!(
            annotation
                .get(b"Contents")
                .and_then(Object::as_str)
                .unwrap(),
            b"important"
        );
    }

    #[test]
    fn quad_points_are_written_in_spec_order() {
        let mut document = one_page_pdf();
        let page_id = first_page(&document);
        let annotation = annotation_dictionary(
            &mut document,
            AnnotationKind::Highlight,
            sample_rect(),
            (1.0, 0.9, 0.2),
            0.4,
            None,
        )
        .unwrap();
        attach_annotation(&mut document, page_id, annotation).unwrap();

        let reloaded = Document::load_mem(&save_to_bytes(&mut document).unwrap()).unwrap();
        let annotations = reloaded
            .get_page_annotations(first_page(&reloaded))
            .unwrap();
        assert_eq!(
            numbers(annotations[0].get(b"QuadPoints").unwrap()),
            vec![
                72.0, 720.0, // upper-left
                300.0, 720.0, // upper-right
                72.0, 700.0, // lower-left
                300.0, 700.0, // lower-right
            ]
        );
    }

    #[test]
    fn writes_each_markup_subtype() {
        for (kind, expected) in [
            (AnnotationKind::Highlight, b"Highlight".as_slice()),
            (AnnotationKind::Underline, b"Underline".as_slice()),
            (AnnotationKind::Strikeout, b"Strikeout".as_slice()),
            // D-A4: the fourth markup type. It shares underline's geometry but keeps its own
            // subtype, because a viewer that synthesises the wave needs to be told which mark
            // it is -- /QuadPoints alone cannot distinguish a squiggle from a rule.
            (AnnotationKind::Squiggly, b"Squiggly".as_slice()),
            (AnnotationKind::Note, b"Text".as_slice()),
            (AnnotationKind::Rect, b"Square".as_slice()),
        ] {
            let mut document = one_page_pdf();
            let page_id = first_page(&document);
            let annotation = annotation_dictionary(
                &mut document,
                kind,
                sample_rect(),
                (1.0, 0.0, 0.0),
                0.4,
                None,
            )
            .unwrap();
            attach_annotation(&mut document, page_id, annotation).unwrap();

            let reloaded = Document::load_mem(&save_to_bytes(&mut document).unwrap()).unwrap();
            let annotations = reloaded
                .get_page_annotations(first_page(&reloaded))
                .unwrap();
            assert_eq!(
                annotations[0]
                    .get(b"Subtype")
                    .and_then(Object::as_name)
                    .unwrap(),
                expected,
                "{kind:?} wrote the wrong subtype"
            );
            assert_eq!(
                annotations[0]
                    .get(b"Type")
                    .and_then(Object::as_name)
                    .unwrap(),
                b"Annot"
            );
        }
    }

    #[test]
    fn ink_annotation_round_trips_with_ink_list() {
        let mut document = one_page_pdf();
        let page_id = first_page(&document);
        let strokes = vec![
            vec![(72.0, 700.0), (100.0, 720.0), (140.0, 705.0)],
            vec![(200.0, 700.0), (240.0, 730.0)],
        ];
        let annotation = ink_annotation_dictionary(
            &mut document,
            sample_rect(),
            &strokes,
            (0.0, 0.0, 1.0),
            0.6,
            Some("sketch"),
        )
        .unwrap();
        attach_annotation(&mut document, page_id, annotation).unwrap();

        let reloaded = Document::load_mem(&save_to_bytes(&mut document).unwrap()).unwrap();
        let annotations = reloaded
            .get_page_annotations(first_page(&reloaded))
            .unwrap();
        let ink_list = annotations[0]
            .get(b"InkList")
            .and_then(Object::as_array)
            .unwrap();
        assert_eq!(ink_list.len(), 2, "both strokes should survive");
        assert_eq!(
            ink_list[0].as_array().unwrap().len(),
            6,
            "3 points -> 6 numbers"
        );
        assert_eq!(
            annotations[0]
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Ink"
        );
    }

    #[test]
    fn rejects_an_ink_annotation_with_no_drawable_strokes() {
        // A single-point stroke cannot be drawn; silently writing it would create
        // an invisible annotation the user cannot find or delete.
        let mut document = one_page_pdf();
        let error = ink_annotation_dictionary(
            &mut document,
            sample_rect(),
            &[vec![(10.0, 10.0)], vec![]],
            (0.0, 0.0, 1.0),
            0.6,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("no drawable strokes"));
    }

    #[test]
    fn rejects_a_dead_click_rect() {
        let mut document = one_page_pdf();
        let click = PdfRect {
            x0: 10.0,
            y0: 10.0,
            x1: 10.0,
            y1: 10.0,
        };
        let error = annotation_dictionary(
            &mut document,
            AnnotationKind::Highlight,
            click,
            (1.0, 0.9, 0.2),
            0.4,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("too small"));
    }

    #[test]
    fn blank_contents_are_not_written() {
        // An empty /Contents would show a tooltip-less icon and adds nothing.
        let mut document = one_page_pdf();
        let annotation = annotation_dictionary(
            &mut document,
            AnnotationKind::Highlight,
            sample_rect(),
            (1.0, 0.9, 0.2),
            0.4,
            Some("   "),
        )
        .unwrap();
        assert!(annotation.get(b"Contents").is_err());
    }

    #[test]
    fn handles_annots_written_as_an_indirect_reference() {
        // ISO 32000-1 permits /Annots to be a reference to an array. A naive
        // implementation would overwrite the reference and orphan the original
        // annotations, silently deleting a user's earlier work.
        let mut document = one_page_pdf();
        let page_id = first_page(&document);
        let existing = annotation_dictionary(
            &mut document,
            AnnotationKind::Highlight,
            sample_rect(),
            (1.0, 0.0, 0.0),
            0.4,
            None,
        )
        .unwrap();
        let existing_id = document.add_object(existing);

        let array_id = document.add_object(Object::Array(vec![Object::Reference(existing_id)]));
        document
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(b"Annots", Object::Reference(array_id));

        let annotation = annotation_dictionary(
            &mut document,
            AnnotationKind::Underline,
            sample_rect(),
            (0.0, 0.0, 1.0),
            0.4,
            None,
        )
        .unwrap();
        attach_annotation(&mut document, page_id, annotation).unwrap();

        let reloaded = Document::load_mem(&save_to_bytes(&mut document).unwrap()).unwrap();
        let annotations = reloaded
            .get_page_annotations(first_page(&reloaded))
            .unwrap();
        assert_eq!(
            annotations.len(),
            2,
            "the pre-existing annotation must not be orphaned"
        );
    }

    #[test]
    fn written_pdf_is_still_openable_by_pdfium() {
        // The whole point of the write path is that the *renderer* accepts it.
        // This asserts on the raw bytes' readability by lopdf; PDFium's own
        // re-render check lives in the integration test (needs the dll).
        let mut document = one_page_pdf();
        let page_id = first_page(&document);
        let annotation = annotation_dictionary(
            &mut document,
            AnnotationKind::Highlight,
            sample_rect(),
            (1.0, 0.9, 0.2),
            0.4,
            None,
        )
        .unwrap();
        attach_annotation(&mut document, page_id, annotation).unwrap();

        let bytes = save_to_bytes(&mut document).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(Document::load_mem(&bytes).is_ok());
    }
}
