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

// ---------------------------------------------------------------------------------------------
// The signature badge (P3.2 SCOPE-C, D-SG7)
// ---------------------------------------------------------------------------------------------

fn frontend_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// D-SG7 requires the four states to be the *rendered* badge, not merely a computed enum. A
/// component that exists, is tested, and is never mounted satisfies nothing -- and that is not a
/// hypothetical failure mode here: `sign_save` was fully built, fully tested, and unreachable from
/// any UI for a whole unit because no test asked whether anything invoked it. This asks.
#[test]
fn the_signature_badge_is_mounted_in_the_app_not_only_imported() {
    let app = frontend_file("src/App.tsx");
    let without_imports: String = app
        .lines()
        .filter(|line| !line.trim_start().starts_with("import "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        without_imports.contains("<SignatureBadgeChip"),
        "App.tsx imports the badge but never renders it, so no user can see a signature state"
    );
}

/// `unknown` is the honest answer offline, so it must not be styled as a fault. The quiet style is
/// `.sig-badge--neutral`, and only `.sig-badge--valid` may be green.
#[test]
fn only_the_valid_badge_is_green() {
    let css = app_css();
    // `rule_containing` returns a default empty rule when nothing matches, so the selectors are
    // checked too: without that, a deleted `.sig-badge--valid` rule would satisfy every assertion
    // below by having nothing to fail on.
    let (valid_selectors, valid) = rule_containing(&css, ".sig-badge--valid");
    assert!(
        valid_selectors.contains(".sig-badge--valid"),
        "the green badge rule is missing from App.css; `valid` would render unstyled"
    );
    assert!(
        valid
            .iter()
            .any(|(prop, _)| prop == "background" || prop == "color"),
        "the valid badge must be visually distinct or the state is invisible"
    );
    let (neutral_selectors, neutral) = rule_containing(&css, ".sig-badge--neutral");
    assert!(
        neutral_selectors.contains(".sig-badge--neutral"),
        "the quiet badge rule is missing from App.css; `unknown` would render unstyled"
    );
    assert!(
        !neutral
            .iter()
            .any(|(prop, value)| prop == "background" && value.contains("#e8f5ec")),
        "the neutral badge must not share the green background: `unknown` is a fact, not a pass"
    );
}

/// The component must take the green class from the backend's boolean, never from the state string.
/// Interpolating the string was a real defect during development: a response saying `valid` with
/// `isGreen: false` still rendered the green class, which is the one thing D-SG7 forbids.
#[test]
fn the_badge_colour_comes_from_the_backend_boolean() {
    let source = frontend_file("src/components/SignatureBadge.tsx");
    let start = source
        .find("const modifier")
        .expect("the colour decision must be a single named binding");
    let end = source[start..]
        .find(';')
        .expect("the binding must end in a semicolon");
    let decision = &source[start..start + end];
    assert!(
        decision.contains("badge.isGreen"),
        "the colour must be decided by `isGreen`, not by the state string: {decision}"
    );
    assert!(
        !decision.contains("badge.badge"),
        "the state string may label the badge but must not choose its colour: {decision}"
    );
}

/// The same defect class that hid `sign_save`: a surface that exists, is tested, and is never
/// mounted. The badge guard next to this one was written after an unreachable command was found; this
/// is the same question asked about the surface that *creates* signatures.
///
/// The trigger and the dialog are checked separately because they are the two halves that can drift:
/// a button with no dialog, or a dialog nothing opens.
#[test]
fn the_sign_dialog_is_mounted_in_the_app_not_only_imported() {
    let app = frontend_file("src/App.tsx");
    let without_imports: String = app
        .lines()
        .filter(|line| !line.trim_start().starts_with("import "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        without_imports.contains("<SignAction"),
        "App.tsx imports the sign action but never renders it, so a user has no way to sign a \
         document -- the same unreachable-surface defect `sign_save` had"
    );
    assert!(
        !without_imports.contains("<SignDialog") || without_imports.contains("<SignAction"),
        "SignDialog is mounted directly, bypassing the trigger that owns it"
    );
}

/// `sign_document` is the command the dialog calls. It takes a PKCS#12 **path**; the certificate
/// listing reports **DER hex**. Passing the hex where the path belongs type-checks and fails only at
/// runtime, so the dialog must not send a certificate's `derHex` as `pfxPath`.
#[test]
fn the_sign_dialog_sends_a_certificate_file_path_not_a_certificate_blob() {
    let dialog = frontend_file("src/components/SignDialog.tsx");
    let without_comments: String = dialog
        .lines()
        .filter(|line| !line.trim_start().starts_with("//") && !line.trim_start().starts_with("*"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        without_comments.contains("pfxPath,"),
        "the dialog must pass a `pfxPath` to sign_document"
    );
    // Every `pfxPath` assignment is inspected, not one spelling of the defect. The first version of
    // this guard matched `pfxPath: chosen.derHex` exactly and missed `pfxPath: chosen?.derHex ?? ""`,
    // which is the same bug with optional chaining -- a guard that only knows one spelling is a guard
    // that a refactor walks straight past.
    for line in without_comments.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("pfxPath:") {
            assert!(
                !rest.contains("derHex"),
                "a certificate's DER blob is not a file path; sending it as `pfxPath` compiles and \
                 fails only at the point of use: {line}"
            );
        }
    }
    // And the store must be described as not yet usable, rather than offered as a working source.
    assert!(
        dialog.contains("not available yet"),
        "the store listing cannot sign yet; the dialog must say so instead of offering it"
    );
}

/// D-SG6: the level is an explicit choice, so the dialog must not default to a certification level.
/// `useState<number | null>(null)` is the approval default; a `0` or a `1` there would make every
/// signature a certification and silently restrict the user's own later edits.
#[test]
fn the_sign_dialog_defaults_to_an_approval_signature_not_to_a_level() {
    let dialog = frontend_file("src/components/SignDialog.tsx");
    assert!(
        dialog.contains("useState<number | null>(null)"),
        "the certification level must default to `null` (an approval signature); a numeric default \
         is an implicit DocMDP level, which D-SG6 forbids"
    );
}
