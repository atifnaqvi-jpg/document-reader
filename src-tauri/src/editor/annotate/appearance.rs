//! `/AP` appearance-stream construction for the types that need one (D-A8).
//!
//! Markup (highlight, underline, strikeout, squiggly) deliberately gets no appearance here:
//! every viewer synthesises it from `/QuadPoints`, so writing one would duplicate what the
//! viewer already does and could disagree with it. Free text is the opposite case -- there is
//! no synthesis to rely on, because nothing tells a viewer what a free-text box should look
//! like. Without `/AP` a text box is visible in Adobe and invisible everywhere else.

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Object, Stream};

use crate::model::FreeTextMode;

/// The resource name the free-text appearance draws with.
///
/// `Helv` is the name Acrobat's own free-text annotations use for the standard-14 font, and
/// `/DA` below names the same font, so an editor that reopens the annotation can resolve the
/// `/Tf` in `/DA` against what the appearance declares.
pub const FREE_TEXT_FONT: &str = "Helv";

/// The `/DA` string a free-text annotation carries.
///
/// `/DA` is what an editor uses when the user changes the text, so it must name a font the
/// reader can resolve without the document having embedded one. Helvetica is one of the
/// standard 14 that every conforming reader supplies; naming a font from this app's own
/// bundle would name a font a different viewer does not have, and the text would render as
/// nothing there.
pub const FREE_TEXT_DA: &str = "/Helv 12 Tf 0 g";

/// Font size used for free text.
const FREE_TEXT_SIZE: f32 = 12.0;

/// Leading between lines, as a multiple of the font size.
const FREE_TEXT_LEADING: f32 = 1.2;

/// Left inset, so text is not flush against the box's border.
const FREE_TEXT_INSET: f32 = 2.0;

/// The `/IT` key a viewer reads to know which free-text flavour this is.
///
/// `/FreeText` is the default and is therefore *absent* for a text box rather than written
/// explicitly: a text box is what a viewer assumes, so an explicit value would be stating the
/// default as though it were a distinction.
pub fn it_value(mode: FreeTextMode) -> Option<&'static str> {
    match mode {
        FreeTextMode::Textbox | FreeTextMode::Callout => None,
        FreeTextMode::Typewriter => Some("FreeTextTypewriter"),
    }
}

/// Whether the mode draws a frame around its text.
///
/// A typewriter's defining property is that it has no frame; a box without one is just text
/// that happens to be wrapped.
pub fn is_bordered(mode: FreeTextMode) -> bool {
    !matches!(mode, FreeTextMode::Typewriter)
}

/// Builds the `/AP /N` stream for a free-text annotation.
///
/// The stream is built in the annotation's own box space: its `/BBox` is the rect, so the
/// content draws from `(0, 0)` to the box's width and height and knows nothing about where the
/// annotation sits on the page. That is the point of an appearance stream -- the viewer
/// positions it, so a box that moves does not need its text rewritten.
///
/// Text is wrapped and then truncated at the bottom of the box rather than allowed to
/// overflow: overflowing text is drawn *outside* the annotation's rect, which puts text on the
/// page that no hit-test or comment entry accounts for.
pub fn free_text_appearance(
    text: &str,
    width: f32,
    height: f32,
    color: (f32, f32, f32),
    mode: FreeTextMode,
) -> Stream {
    let mut operations = vec![Operation::new("q", vec![])];

    if is_bordered(mode) {
        // The frame is drawn first so the text sits on top of it.
        operations.push(Operation::new(
            "rg",
            vec![color.0.into(), color.1.into(), color.2.into()],
        ));
        operations.push(Operation::new("w", vec![Object::Real(1.0)]));
        operations.push(Operation::new(
            "re",
            vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(width),
                Object::Real(height),
            ],
        ));
        operations.push(Operation::new("S", vec![]));
    }

    operations.push(Operation::new("BT", vec![]));
    operations.push(Operation::new(
        "Tf",
        vec![
            Object::Name(FREE_TEXT_FONT.as_bytes().to_vec()),
            FREE_TEXT_SIZE.into(),
        ],
    ));
    operations.push(Operation::new(
        "rg",
        vec![color.0.into(), color.1.into(), color.2.into()],
    ));

    // An appearance stream's origin is the box's lower-left, so text starts near the top and
    // steps down. `Td` is relative to the current text position, hence the growing offset.
    let usable_width = (width - FREE_TEXT_INSET * 2.0).max(1.0);
    let mut baseline_from_top = FREE_TEXT_SIZE;
    let mut placed = 0.0_f32;

    for line in wrap_to_width(text, usable_width, FREE_TEXT_SIZE) {
        if baseline_from_top > height {
            break;
        }
        let y = height - baseline_from_top;
        operations.push(Operation::new(
            "Td",
            vec![
                Object::Real(if placed == 0.0 { FREE_TEXT_INSET } else { 0.0 }),
                Object::Real(y - placed),
            ],
        ));
        placed = y;
        operations.push(Operation::new(
            "Tj",
            vec![Object::String(
                escape_text(&line).into_bytes(),
                lopdf::StringFormat::Literal,
            )],
        ));
        baseline_from_top += FREE_TEXT_SIZE * FREE_TEXT_LEADING;
    }

    operations.push(Operation::new("ET", vec![]));
    operations.push(Operation::new("Q", vec![]));

    let content = Content { operations };
    let mut stream = Stream::new(Dictionary::new(), content.encode().unwrap_or_default());
    stream.dict.set("Type", Object::Name(b"XObject".to_vec()));
    stream.dict.set("Subtype", Object::Name(b"Form".to_vec()));
    stream.dict.set(
        "BBox",
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    stream.dict.set(
        "Resources",
        dictionary! {
            "Font" => dictionary! {
                FREE_TEXT_FONT => free_text_font(),
            },
        },
    );
    stream
}

/// Line width for an ink stroke, in points.
///
/// A stroke with no width operator takes the viewer's default, which varies; a fixed width is
/// what makes the saved mark look like the one that was drawn.
pub const INK_STROKE_WIDTH: f32 = 2.0;

/// Builds the `/AP /N` stream for an ink annotation (D-A8).
///
/// `/InkList` records where the pen went but not what the stroke looks like: no width, no
/// caps, no joins. Adobe reads `/InkList` directly, so ink survives there without an
/// appearance; every other viewer draws only `/AP`, so without this the stroke is invisible.
///
/// The stream is built in the annotation's own box space, with the box's lower-left at the
/// origin. That is why the points are translated by the box's corner rather than written in
/// page coordinates: a `/BBox` starting at the origin and content in page space would put the
/// stroke off the appearance, and the viewer -- which positions the appearance at `/Rect` --
/// would draw it twice-displaced.
pub fn ink_appearance(
    strokes: &[Vec<(f32, f32)>],
    width: f32,
    height: f32,
    color: (f32, f32, f32),
) -> Stream {
    let mut operations = vec![
        Operation::new("q", vec![]),
        Operation::new("RG", vec![color.0.into(), color.1.into(), color.2.into()]),
        Operation::new("w", vec![Object::Real(INK_STROKE_WIDTH)]),
        // Round caps and joins: a freehand stroke is a pen, and a butt cap makes a turning
        // point look notched. The graphics state needs the caps set after `w` to apply.
        Operation::new("J", vec![Object::Integer(1)]),
        Operation::new("j", vec![Object::Integer(1)]),
    ];

    // The appearance box's lower-left, so page-space points become box-space points.
    let origin_x = strokes
        .iter()
        .flat_map(|stroke| stroke.iter())
        .map(|(x, _)| *x)
        .fold(f32::INFINITY, f32::min);
    let origin_y = strokes
        .iter()
        .flat_map(|stroke| stroke.iter())
        .map(|(_, y)| *y)
        .fold(f32::INFINITY, f32::min);

    for stroke in strokes.iter().filter(|stroke| stroke.len() >= 2) {
        let mut points = stroke.iter();
        if let Some((x, y)) = points.next() {
            operations.push(Operation::new(
                "m",
                vec![Object::Real(x - origin_x), Object::Real(y - origin_y)],
            ));
        }
        for (x, y) in points {
            operations.push(Operation::new(
                "l",
                vec![Object::Real(x - origin_x), Object::Real(y - origin_y)],
            ));
        }
        operations.push(Operation::new("S", vec![]));
    }

    operations.push(Operation::new("Q", vec![]));

    let content = Content { operations };
    let mut stream = Stream::new(Dictionary::new(), content.encode().unwrap_or_default());
    stream.dict.set("Type", Object::Name(b"XObject".to_vec()));
    stream.dict.set("Subtype", Object::Name(b"Form".to_vec()));
    stream.dict.set(
        "BBox",
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    // No `/Resources` entry: ink draws no text and names no font. An empty resources dict is
    // correct here, and it is also why ink does not have free text's `/DR` obligation.
    stream.dict.set("Resources", Dictionary::new());
    stream
}

/// Line width for a line/arrow stroke, in points.
pub const LINE_STROKE_WIDTH: f32 = 2.0;

/// Length of each barb of an arrowhead, and the half-angle it opens to.
///
/// Fixed lengths rather than a fraction of the stroke: an arrowhead scales with the line it
/// terminates in Acrobat, but a head that grows with the line on a long leader ends up larger
/// than the text it points at.
const ARROW_HEAD_LENGTH: f32 = 10.0;
const ARROW_HEAD_HALF_WIDTH: f32 = 4.0;

/// Builds the `/AP /N` stream for a line or arrow (D-A8).
///
/// The stream is in the annotation's box space: the box's lower-left goes to the origin, so
/// the endpoints are written relative to it. Like ink, a line has no `/AP`-exempt synthesis
/// path -- `/L` gives a viewer the endpoints but not the stroke's appearance -- so without
/// this the mark is Adobe-only.
///
/// The arrowhead is drawn here *as well as* declared in `/LE`. `/LE` is the semantic record,
/// which a viewer that renders lines itself uses to draw its own head; the appearance is what
/// a viewer that only blits `/AP` shows. Writing only `/LE` leaves those viewers a plain line.
pub fn line_appearance(
    from: (f32, f32),
    to: (f32, f32),
    width: f32,
    height: f32,
    color: (f32, f32, f32),
    arrow: bool,
) -> Stream {
    let origin_x = from.0.min(to.0);
    let origin_y = from.1.min(to.1);
    let start = (from.0 - origin_x, from.1 - origin_y);
    let end = (to.0 - origin_x, to.1 - origin_y);

    let mut operations = vec![
        Operation::new("q", vec![]),
        Operation::new("RG", vec![color.0.into(), color.1.into(), color.2.into()]),
        Operation::new("w", vec![Object::Real(LINE_STROKE_WIDTH)]),
        Operation::new("J", vec![Object::Integer(1)]),
        Operation::new("m", vec![Object::Real(start.0), Object::Real(start.1)]),
        Operation::new("l", vec![Object::Real(end.0), Object::Real(end.1)]),
        Operation::new("S", vec![]),
    ];

    if arrow {
        // The head points along the line, so the barbs are the two ends of a segment centred
        // on the tip and perpendicular to the line's direction.
        let dx = end.0 - start.0;
        let dy = end.1 - start.1;
        let length = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
        let (ux, uy) = (dx / length, dy / length);

        let base_x = end.0 - ux * ARROW_HEAD_LENGTH;
        let base_y = end.1 - uy * ARROW_HEAD_LENGTH;
        // Perpendicular to the direction, scaled to the head's half-width.
        let (px, py) = (-uy * ARROW_HEAD_HALF_WIDTH, ux * ARROW_HEAD_HALF_WIDTH);

        operations.push(Operation::new(
            "rg",
            vec![color.0.into(), color.1.into(), color.2.into()],
        ));
        operations.push(Operation::new(
            "m",
            vec![Object::Real(end.0), Object::Real(end.1)],
        ));
        operations.push(Operation::new(
            "l",
            vec![Object::Real(base_x + px), Object::Real(base_y + py)],
        ));
        operations.push(Operation::new(
            "l",
            vec![Object::Real(base_x - px), Object::Real(base_y - py)],
        ));
        operations.push(Operation::new("f", vec![]));
    }

    operations.push(Operation::new("Q", vec![]));

    let content = Content { operations };
    let mut stream = Stream::new(Dictionary::new(), content.encode().unwrap_or_default());
    stream.dict.set("Type", Object::Name(b"XObject".to_vec()));
    stream.dict.set("Subtype", Object::Name(b"Form".to_vec()));
    stream.dict.set(
        "BBox",
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    stream.dict.set("Resources", Dictionary::new());
    stream
}

/// The `/Font` object a free-text appearance's `/Resources` names.
///
/// Helvetica rather than a bundled font: an appearance stream naming a font the reader does
/// not have renders as nothing, and Helvetica is one of the standard 14 every conforming
/// reader supplies.
pub fn free_text_font() -> Dictionary {
    dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    }
}

/// Greedy wrap at spaces. A word longer than the box is kept whole rather than cut:
/// truncating mid-word would put a partial word on the page that the stored text does not
/// contain.
fn wrap_to_width(text: &str, width: f32, size: f32) -> Vec<String> {
    // Width is estimated, not measured: an appearance stream has no metrics engine available,
    // and Helvetica averages roughly half its point size per character. A generous estimate
    // wraps early, which errs toward staying inside the box.
    let per_char = size * 0.5;
    let max_chars = if width > 0.0 {
        (width / per_char).floor().max(1.0) as usize
    } else {
        1
    };

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= max_chars {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Escapes a PDF literal string. Duplicated from `export`'s helper deliberately: that one is
/// private to the export path, and sharing it would couple the appearance module to the
/// flattening code's internals.
fn escape_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '(' => escaped.push_str("\\("),
            ')' => escaped.push_str("\\)"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_textbox_writes_no_it_key() {
        // A text box is the viewer's default, so writing `/IT /FreeText` explicitly would be
        // asserting the default. The absence is the assertion.
        assert_eq!(it_value(FreeTextMode::Textbox), None);
    }

    #[test]
    fn a_typewriter_writes_the_typewriter_it_key() {
        assert_eq!(
            it_value(FreeTextMode::Typewriter),
            Some("FreeTextTypewriter")
        );
    }

    #[test]
    fn a_callout_is_free_text_not_its_own_it_value() {
        // A callout is distinguished by its leader line (`/CL`), not by `/IT`.
        assert_eq!(it_value(FreeTextMode::Callout), None);
    }

    #[test]
    fn only_the_typewriter_has_no_frame() {
        assert!(is_bordered(FreeTextMode::Textbox));
        assert!(is_bordered(FreeTextMode::Callout));
        assert!(!is_bordered(FreeTextMode::Typewriter));
    }

    #[test]
    fn wrapping_never_exceeds_the_box_width() {
        let lines = wrap_to_width("the quick brown fox jumps over the lazy dog", 120.0, 12.0);
        assert!(lines.len() > 1, "a long sentence must wrap");
        for line in &lines {
            // 120pt at 12pt Helvetica is ~20 characters at the 0.5 estimate.
            assert!(
                line.chars().count() <= 20,
                "line {line:?} exceeds the box width"
            );
        }
    }

    #[test]
    fn wrapping_keeps_a_long_word_whole() {
        let lines = wrap_to_width("supercalifragilisticexpialidocious", 40.0, 12.0);
        assert_eq!(lines, vec!["supercalifragilisticexpialidocious"]);
    }

    #[test]
    fn an_empty_string_still_yields_one_line() {
        // A stream with no `Tj` at all is valid but pointless; one empty line keeps the
        // operation sequence uniform, which the truncation rule below relies on.
        assert_eq!(wrap_to_width("", 100.0, 12.0), vec![String::new()]);
    }

    #[test]
    fn the_appearance_bbox_is_the_box_and_the_resources_name_a_font() {
        let stream =
            free_text_appearance("hello", 200.0, 60.0, (0.0, 0.0, 0.0), FreeTextMode::Textbox);
        assert_eq!(
            stream
                .dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .unwrap(),
            b"Form"
        );
        let bbox = stream.dict.get(b"BBox").and_then(Object::as_array).unwrap();
        assert_eq!(bbox.len(), 4);
        // The box origin is 0,0 by definition: the viewer positions the form.
        assert_eq!(bbox[0].as_float().unwrap(), 0.0);
        assert_eq!(bbox[1].as_float().unwrap(), 0.0);
        assert_eq!(bbox[2].as_float().unwrap(), 200.0);
        assert_eq!(bbox[3].as_float().unwrap(), 60.0);

        let resources = stream.dict.get(b"Resources").unwrap().as_dict().unwrap();
        let fonts = resources.get(b"Font").unwrap().as_dict().unwrap();
        assert!(
            fonts.get(FREE_TEXT_FONT.as_bytes()).is_ok(),
            "the appearance must declare the font it draws with, or `Tf` names nothing"
        );
    }

    #[test]
    fn text_is_truncated_rather_than_overflowing_the_box() {
        // Overflowing text is drawn outside the annotation's rect, which puts text on the
        // page that no hit-test or comment entry accounts for.
        let long = "word ".repeat(200);
        let stream =
            free_text_appearance(&long, 100.0, 40.0, (0.0, 0.0, 0.0), FreeTextMode::Textbox);
        let content = String::from_utf8_lossy(stream.content.as_slice());
        let drawn = content.matches("Tj").count();
        // 40pt of box at 12pt and 1.2 leading fits three baselines; the fourth is past the
        // bottom edge and must not be emitted.
        assert!(
            drawn <= 3,
            "drew {drawn} lines into a box that fits at most 3"
        );
    }

    #[test]
    fn parentheses_in_the_text_are_escaped() {
        // An unescaped `)` closes the literal string early and the rest of the text becomes
        // content-stream operators. The appearance-stream version of the bug `export` guards.
        assert_eq!(escape_text("a(b)c"), "a\\(b\\)c");
    }
}
