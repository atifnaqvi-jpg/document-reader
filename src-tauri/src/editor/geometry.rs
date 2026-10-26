//! The one place that converts between the two coordinate systems (P2 design §4).
//!
//! PDF native space is origin **bottom-left**, +y up, in points (1/72"). The UI
//! works in origin **top-left**, +y down, in CSS pixels at the current zoom.
//! Mixing these silently mirrors annotations about the middle of the page, which
//! looks almost right on a short page and is wrong on every other one -- so every
//! conversion goes through this module and nothing else is allowed to flip a y.

/// A rectangle in PDF points, origin bottom-left.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// A page-space point, in PDF points with the origin bottom-left.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

impl Point2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// The frame a run's text is laid out in, expressed in page space (P2.7.4 §4).
///
/// Every quantity the picker measures — where the run starts, how wide it is, which line it
/// sits on, how far the next fragment is — is *relative to the direction the text advances*.
/// For horizontal text that direction is `(1, 0)`, so `along` is `x` and `across` is `y` and
/// every existing formula is unchanged bit for bit. For rotated text the same formulas keep
/// working once they are written in terms of the axes rather than the page's own.
///
/// `advance` is where one unit of text advance points; `up` is the glyphs' up direction.
/// They are unit vectors but need not be perpendicular: a skew leaves them at an angle, and
/// nothing measured here requires orthogonality — only that `advance` is the measurement
/// direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunFrame {
    pub advance: Point2,
    pub up: Point2,
}

impl RunFrame {
    /// The page's own axes: the frame every unrotated run is measured in.
    pub const PAGE: Self = Self {
        advance: Point2::new(1.0, 0.0),
        up: Point2::new(0.0, 1.0),
    };

    /// Builds a frame from two vectors, normalising each. A degenerate vector falls back to
    /// the page axis for that direction, so callers never divide by zero; the caller is
    /// responsible for refusing a run whose transform has no usable scale.
    pub fn from_vectors(advance: (f32, f32), up: (f32, f32)) -> Self {
        fn unit(vector: (f32, f32), fallback: Point2) -> Point2 {
            let length = (vector.0 * vector.0 + vector.1 * vector.1).sqrt();
            if length > f32::EPSILON {
                Point2::new(vector.0 / length, vector.1 / length)
            } else {
                fallback
            }
        }
        Self {
            advance: unit(advance, Point2::new(1.0, 0.0)),
            up: unit(up, Point2::new(0.0, 1.0)),
        }
    }

    /// How far along the advance axis a page-space point lies.
    pub fn along(&self, point: Point2) -> f32 {
        point.x * self.advance.x + point.y * self.advance.y
    }

    /// How far along the up axis a page-space point lies: the run's "baseline coordinate".
    pub fn across(&self, point: Point2) -> f32 {
        point.x * self.up.x + point.y * self.up.y
    }

    /// True when the advance and up axes are perpendicular, i.e. the text is not sheared.
    ///
    /// Load-bearing for measurement: PDFium's character boxes are **axis-aligned in page
    /// space**, so for a sheared glyph the box is the bounding box of the sheared ink and is
    /// wider than the advance by `|shear| × glyph height`. Projecting that box onto the
    /// baseline therefore overstates the run — measured 124.88pt for a 114.67pt line at 20°
    /// of x-shear. Under a shear the grid cannot supply an advance width; only the font's own
    /// metric, carried through the advance axis, can.
    pub fn is_orthogonal(&self) -> bool {
        const EPSILON: f32 = 1e-3;
        (self.advance.x * self.up.x + self.advance.y * self.up.y).abs() <= EPSILON
    }

    /// The page-space point `along` units down the advance axis and `across` units up.
    ///
    /// Exact inverse of [`Self::along`]/[`Self::across`] only when the axes are orthogonal.
    /// Projections are what this phase measures with, so the inverse is used for placing
    /// anchors and for tests, not for reconstructing ink boxes under a skew.
    pub fn point(&self, along: f32, across: f32) -> Point2 {
        Point2::new(
            along * self.advance.x + across * self.up.x,
            along * self.advance.y + across * self.up.y,
        )
    }

    /// True when the advance axis is the page's `x` axis, which is what lets a box's width be
    /// trusted as a width (P2.7.4 Q2).
    pub fn is_horizontal(&self) -> bool {
        const EPSILON: f32 = 1e-4;
        self.advance.x > 0.0 && self.advance.y.abs() <= EPSILON
    }

    /// True when the frame reverses handedness — the glyphs are reflected, so the text does
    /// not read left-to-right along its own advance axis (Q5).
    pub fn is_mirrored(&self) -> bool {
        self.advance.x * self.up.y - self.advance.y * self.up.x < 0.0
    }

    /// Whether two fragments share one text direction, within a small angular tolerance.
    ///
    /// Per Q7, fragments at different angles are not one line even when they are adjacent:
    /// a merged write puts the whole replacement in the head fragment, drawn through the
    /// head's matrix, so the tail's text would move.
    pub fn same_direction(&self, other: &Self) -> bool {
        const COSINE: f32 = 0.9998; // ≈1.15°
        let dot = self.advance.x * other.advance.x + self.advance.y * other.advance.y;
        dot >= COSINE
    }
}

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

    /// The property the whole of P2.7.4 depends on: the page frame must reduce every
    /// projection to the coordinate the pre-phase code read directly. If this ever fails,
    /// unrotated behaviour has changed.
    #[test]
    fn the_page_frame_projects_onto_x_and_y() {
        let frame = RunFrame::PAGE;
        for (x, y) in [(0.0, 0.0), (72.5, 700.25), (-400.0, 172.0)] {
            assert_eq!(frame.along(Point2::new(x, y)), x);
            assert_eq!(frame.across(Point2::new(x, y)), y);
            assert_eq!(frame.point(x, y), Point2::new(x, y));
        }
        assert!(frame.is_horizontal());
        assert!(!frame.is_mirrored());
    }

    /// A 90° run: advance points up the page, glyphs point left. Measured values, from the
    /// `rotated_text` fixture's own axes.
    #[test]
    fn a_quarter_turn_projects_along_the_rotated_axis() {
        let frame = RunFrame::from_vectors((0.0, 1.0), (-1.0, 0.0));
        // A character one em further down the page is one em further along the line.
        assert_eq!(frame.along(Point2::new(300.0, 117.33)), 117.33);
        assert_eq!(frame.across(Point2::new(300.0, 117.33)), -300.0);
        // The horizontal extent of that ink (18pt of glyph height) is *not* advance.
        assert_eq!(frame.along(Point2::new(282.31, 117.33)), 117.33);
        assert!(!frame.is_horizontal());
        assert!(!frame.is_mirrored());
    }

    #[test]
    fn a_mirrored_frame_is_detected_by_handedness() {
        // An x-flip: advance still points right, but up points down, so the determinant is
        // negative and the glyphs are reflected.
        let frame = RunFrame::from_vectors((-1.0, 0.0), (0.0, 1.0));
        assert!(frame.is_mirrored());
        assert!(
            !frame.is_horizontal(),
            "a flipped advance is not left-to-right"
        );
    }

    #[test]
    fn frames_agree_only_within_the_angular_tolerance() {
        let horizontal = RunFrame::PAGE;
        let near = RunFrame::from_vectors((1.0, 0.01), (0.0, 1.0));
        let turned = RunFrame::from_vectors((0.0, 1.0), (-1.0, 0.0));
        assert!(horizontal.same_direction(&horizontal));
        assert!(
            horizontal.same_direction(&near),
            "a fraction of a degree is a rounding difference, not a new line"
        );
        assert!(!horizontal.same_direction(&turned));
    }

    #[test]
    fn a_degenerate_axis_falls_back_instead_of_dividing_by_zero() {
        let frame = RunFrame::from_vectors((0.0, 0.0), (0.0, 0.0));
        assert!(frame.advance.x.is_finite() && frame.advance.y.is_finite());
        assert!(frame.up.x.is_finite() && frame.up.y.is_finite());
    }
}
