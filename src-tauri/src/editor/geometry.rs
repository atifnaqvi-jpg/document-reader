//! The one place that converts between the two coordinate systems (P2 design §4).
//!
//! PDF native space is origin **bottom-left**, +y up, in points (1/72"). The UI
//! works in origin **top-left**, +y down, in CSS pixels at the current zoom.
//! Mixing these silently mirrors annotations about the middle of the page, which
//! looks almost right on a short page and is wrong on every other one -- so every
//! conversion goes through this module and nothing else is allowed to flip a y.

/// A rectangle in PDF points, origin bottom-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PdfRect {
    /// Normalises so `x0 <= x1` and `y0 <= y1`. Annotation `/Rect` values must be
    /// in this order (ISO 32000-1 12.5.2); a viewer given a reversed rect is free to
    /// treat the annotation as zero-area and drop it.
    pub fn normalised(self) -> Self {
        PdfRect {
            x0: self.x0.min(self.x1),
            y0: self.y0.min(self.y1),
            x1: self.x0.max(self.x1),
            y1: self.y0.max(self.y1),
        }
    }

    pub fn width(self) -> f32 {
        (self.x1 - self.x0).abs()
    }

    pub fn height(self) -> f32 {
        (self.y1 - self.y0).abs()
    }

    /// Rejects rects that cannot be drawn. A click without a drag produces a
    /// zero-area rect; saving one would put an invisible, unselectable annotation
    /// in the file that the user can neither see nor delete.
    pub fn is_drawable(self) -> bool {
        self.width() > MIN_DIMENSION_POINTS && self.height() > MIN_DIMENSION_POINTS
    }

    /// The four `/QuadPoints` corners in the order the spec requires:
    /// upper-left, upper-right, lower-left, lower-right.
    fn quad_points(self) -> [f32; 8] {
        let rect = self.normalised();
        // "Upper" is +y in PDF space, so upper == larger y.
        [
            rect.x0, rect.y1, // upper-left
            rect.x1, rect.y1, // upper-right
            rect.x0, rect.y0, // lower-left
            rect.x1, rect.y0, // lower-right
        ]
    }

    pub fn quad_points_array(self) -> Vec<f32> {
        self.quad_points().to_vec()
    }
}

/// Below this, a rect is treated as a dead click rather than a mark.
pub const MIN_DIMENSION_POINTS: f32 = 0.5;

/// Converts a rect captured in the UI (top-left origin, CSS px at `scale`) into
/// PDF points. `page_height_points` is the unrotated page height from PDFium.
///
/// The y flip is `page_height - y` rather than `-y`: annotations are positioned
/// within the page box, so a bare negation would place everything off-page.
pub fn page_to_pdf(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    scale: f32,
    page_height_points: f32,
) -> PdfRect {
    // A zero/negative scale would divide by zero (inf rect) -- fall back to 1:1
    // rather than writing an unrenderable annotation.
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let to_points = |value: f32| value / scale;
    PdfRect {
        x0: to_points(x0),
        y0: page_height_points - to_points(y1),
        x1: to_points(x1),
        y1: page_height_points - to_points(y0),
    }
    .normalised()
}

/// Converts a PDF-space rect back to UI space, for drawing an overlay on top of a
/// rendered page. Inverse of [`page_to_pdf`]; kept adjacent so the pair can be
/// checked against each other in tests.
pub fn pdf_to_page(rect: PdfRect, scale: f32, page_height_points: f32) -> (f32, f32, f32, f32) {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let rect = rect.normalised();
    (
        rect.x0 * scale,
        (page_height_points - rect.y1) * scale,
        rect.x1 * scale,
        (page_height_points - rect.y0) * scale,
    )
}

/// Converts a single point (note anchors, ink vertices) from UI to PDF space.
pub fn point_to_pdf(x: f32, y: f32, scale: f32, page_height_points: f32) -> (f32, f32) {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    (x / scale, page_height_points - (y / scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A4-ish page in points.
    const PAGE_H: f32 = 842.0;

    #[test]
    fn top_of_the_page_maps_to_a_high_pdf_y() {
        // A strip 10px from the top, at 100% zoom, must land near the page top
        // (large y), not near y=0.
        let rect = page_to_pdf(0.0, 10.0, 100.0, 30.0, 1.0, PAGE_H);
        assert!((rect.y1 - (PAGE_H - 10.0)).abs() < 0.001);
        assert!((rect.y0 - (PAGE_H - 30.0)).abs() < 0.001);
        assert!(rect.y0 < rect.y1, "rect must come out normalised");
    }

    #[test]
    fn bottom_of_the_page_maps_to_a_low_pdf_y() {
        let rect = page_to_pdf(0.0, PAGE_H - 30.0, 100.0, PAGE_H - 10.0, 1.0, PAGE_H);
        assert!(rect.y0 < 30.0);
        assert!(rect.y1 < 30.0 + 0.001);
    }

    #[test]
    fn zoom_is_divided_out() {
        // The same visual mark at 2x zoom has double the pixel coordinates but
        // must produce the same page-space rect.
        let at_1x = page_to_pdf(20.0, 40.0, 120.0, 60.0, 1.0, PAGE_H);
        let at_2x = page_to_pdf(40.0, 80.0, 240.0, 120.0, 2.0, PAGE_H);
        assert_eq!(at_1x, at_2x);
    }

    #[test]
    fn round_trips_through_both_spaces() {
        let original = PdfRect {
            x0: 40.0,
            y0: 100.0,
            x1: 300.0,
            y1: 140.0,
        };
        for scale in [0.5_f32, 1.0, 1.5, 3.0] {
            let (x0, y0, x1, y1) = pdf_to_page(original, scale, PAGE_H);
            let back = page_to_pdf(x0, y0, x1, y1, scale, PAGE_H);
            assert!((back.x0 - original.x0).abs() < 0.001, "x0 at scale {scale}");
            assert!((back.y0 - original.y0).abs() < 0.001, "y0 at scale {scale}");
            assert!((back.x1 - original.x1).abs() < 0.001, "x1 at scale {scale}");
            assert!((back.y1 - original.y1).abs() < 0.001, "y1 at scale {scale}");
        }
    }

    #[test]
    fn quad_points_follow_spec_order() {
        let rect = PdfRect {
            x0: 10.0,
            y0: 20.0,
            x1: 30.0,
            y1: 40.0,
        };
        // upper-left, upper-right, lower-left, lower-right
        assert_eq!(
            rect.quad_points_array(),
            vec![10.0, 40.0, 30.0, 40.0, 10.0, 20.0, 30.0, 20.0]
        );
    }

    #[test]
    fn reversed_input_rect_is_normalised() {
        let rect = page_to_pdf(200.0, 100.0, 50.0, 40.0, 1.0, PAGE_H);
        assert!(rect.x0 <= rect.x1);
        assert!(rect.y0 <= rect.y1);
    }

    #[test]
    fn dead_click_is_not_drawable() {
        let click = PdfRect {
            x0: 10.0,
            y0: 10.0,
            x1: 10.0,
            y1: 10.0,
        };
        assert!(!click.is_drawable());

        let real = PdfRect {
            x0: 10.0,
            y0: 10.0,
            x1: 80.0,
            y1: 24.0,
        };
        assert!(real.is_drawable());
    }

    #[test]
    fn zero_scale_does_not_produce_infinite_coordinates() {
        let rect = page_to_pdf(10.0, 10.0, 60.0, 30.0, 0.0, PAGE_H);
        assert!(rect.x0.is_finite() && rect.y0.is_finite());
        assert!(rect.x1.is_finite() && rect.y1.is_finite());
    }

    #[test]
    fn point_conversion_matches_rect_conversion() {
        let rect = page_to_pdf(10.0, 20.0, 30.0, 40.0, 1.5, PAGE_H);
        let (px0, py1) = point_to_pdf(10.0, 20.0, 1.5, PAGE_H);
        let (px1, py0) = point_to_pdf(30.0, 40.0, 1.5, PAGE_H);
        assert!((px0 - rect.x0).abs() < 0.001);
        assert!((py1 - rect.y1).abs() < 0.001);
        assert!((px1 - rect.x1).abs() < 0.001);
        assert!((py0 - rect.y0).abs() < 0.001);
    }
}
