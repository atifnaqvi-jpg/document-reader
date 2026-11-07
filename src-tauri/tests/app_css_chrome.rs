//! A-11b: the note-prompt and note-editor textareas must be legible.
//!
//! Reported against the fix build: typing in the note prompt put the text flush against the
//! top border with the first line half-clipped. The writer was already correct — the three
//! pixel tests in `free_text_appearance.rs` prove the saved file renders its text inside the
//! box — so this is editor chrome, a stylesheet defect, and the stylesheet is what is guarded
//! here.
//!
//! The cause, measured in Chromium rather than guessed: `.page-stage` sets `line-height: 0`
//! so it can shrink-wrap the page raster, and the textareas said `font: inherit`. That is a
//! *shorthand*, so it re-applies the inherited line-height along with the family and size. The
//! textarea's line boxes came out zero pixels tall, every line drew at the same y overprinting
//! itself, and the first line hung above the content box where the border clipped it. A named
//! `line-height` is what overrides the shorthand; `padding` supplies the inset the control never
//! had. `docs/evidence/note-prompt-before-after.png` is the rendered before/after.
//!
//! These guards read the declarations out of `App.css` rather than a computed style, because
//! the measurement that matters is a browser's and the gate does not run one. They are the
//! declarations that regressed, which is the part a future edit would take away.

use std::path::PathBuf;

fn app_css() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .join("src/App.css");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The selector list and body of the first rule whose selector text contains `needle`.
///
/// Deliberately not a regex: the bodies here are flat, and a substring scan cannot be fooled
/// by catastrophic backtracking. Returns `(selectors, declarations)` with the body split into
/// `prop: value` pairs on `;`.
fn rule_containing(css: &str, needle: &str) -> (String, Vec<(String, String)>) {
    for block in css.split('}') {
        let Some((selectors, body)) = block.split_once('{') else {
            continue;
        };
        if !selectors.contains(needle) {
            continue;
        }
        // Trailing comment lines precede the selector; take the last non-empty, non-comment
        // line as the selector text.
        let selector_text = selectors
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("/*") && !line.starts_with('*'))
            .next_back()
            .unwrap_or_default()
            .to_string();
        let declarations = body
            .split(';')
            .filter_map(|decl| {
                let (prop, value) = decl.split_once(':')?;
                Some((prop.trim().to_string(), value.trim().to_string()))
            })
            .collect();
        return (selector_text, declarations);
    }
    panic!("no rule in App.css mentions `{needle}`");
}

fn declaration<'a>(decls: &'a [(String, String)], prop: &str) -> Option<&'a str> {
    decls
        .iter()
        .find(|(name, _)| name == prop)
        .map(|(_, value)| value.as_str())
}

const TEXTAREA: &str = ".annotation-note-prompt textarea";

#[test]
fn the_textarea_rule_covers_the_note_editor_too() {
    // Both prompts are the same element in two places. A rule narrowed to one of them leaves
    // the other clipped, which is exactly how this defect would return.
    let (selectors, _) = rule_containing(&app_css(), TEXTAREA);
    assert!(
        selectors.contains(".annotation-note-editor textarea"),
        "the textarea rule must cover both prompts; selector was `{selectors}`"
    );
}

#[test]
fn the_textarea_sets_a_line_height_that_overrides_the_inherited_zero() {
    let css = app_css();
    let (_, decls) = rule_containing(&css, TEXTAREA);

    // Absent is the defect: `font: inherit` then re-applies `.page-stage`'s `line-height: 0`,
    // and the lines overprint. `normal` is equally unsound here — it would defer to the same
    // inherited value rather than override it.
    let Some(line_height) = declaration(&decls, "line-height") else {
        panic!(
            "the textarea rule must name a line-height; without one `font: inherit` re-applies \
             .page-stage's `line-height: 0` and every line draws at the same y. Declarations: \
             {decls:?}"
        );
    };
    assert_ne!(
        line_height, "normal",
        "`line-height: normal` does not override the inherited zero; name a length or a ratio"
    );

    // The value must be at least the font size or the same overlap returns in a milder form.
    if let Some(px) = line_height.strip_suffix("px") {
        let px: f64 = px.parse().expect("line-height px must be numeric");
        // This rule sets no font-size, so it inherits the app's ~14px default.
        assert!(
            px >= 14.0,
            "line-height {line_height} is shorter than a 14px line; the text would still overlap"
        );
    } else {
        let ratio: f64 = line_height
            .parse()
            .unwrap_or_else(|_| panic!("line-height `{line_height}` is neither px nor unitless"));
        assert!(
            ratio >= 1.0,
            "line-height ratio {ratio} is shorter than the font; the text would still overlap"
        );
    }
}

#[test]
fn the_textarea_carries_a_top_inset() {
    let css = app_css();
    let (_, decls) = rule_containing(&css, TEXTAREA);
    let Some(padding) = declaration(&decls, "padding") else {
        panic!("the textarea rule must carry a padding inset; declarations: {decls:?}");
    };
    let top: f64 = padding
        .split_whitespace()
        .next()
        .and_then(|value| value.strip_suffix("px"))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("expected a px top inset in `padding: {padding}`"));

    // Editor chrome, not PDF space: a fixed few pixels. Above zero so the first line cannot
    // touch the border, and small enough that it cannot be mistaken for the writer's own inset
    // scaled by zoom.
    assert!(
        (2.0..=8.0).contains(&top),
        "top inset {top}px is outside the 2-8px range editor chrome should use"
    );
}

#[test]
fn the_free_text_preview_keeps_its_metrics_out_of_the_stylesheet() {
    // A-11 moved font size, leading and inset onto the mark inline, from the writer's
    // constants. Re-adding any of them to the stylesheet would let the preview disagree with
    // the file at every zoom but 1 — the bug A-11 was raised for.
    let css = app_css();
    let (_, decls) = rule_containing(&css, ".kind-free-text");
    for prop in ["font-size", "line-height", "padding"] {
        assert!(
            declaration(&decls, prop).is_none(),
            "`.kind-free-text` must not set `{prop}`: those come from the writer's constants \
             inline, or the preview stops matching the saved file at zoom != 1"
        );
    }
}
