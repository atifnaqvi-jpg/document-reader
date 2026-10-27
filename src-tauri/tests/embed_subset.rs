//! P2.7.6 §3: the subset itself, measured rather than asserted.
//!
//! Every claim §2.3 makes about the written program is checked here against the bytes that
//! would go into a PDF, with the same parser a reader would use:
//!
//! * it re-parses as a TrueType face at all (a subset that only our viewer can read is not a
//!   subset; it is a private format);
//! * it maps exactly the requested characters and no others;
//! * it is smaller than the source face — the bound that stops "subsetting" from quietly
//!   becoming a full copy;
//! * the `name` table carries the licence records and does **not** carry the reserved name;
//! * the `/BaseFont` and every name token avoid `Liberation`;
//! * the same input twice yields the same bytes, because a non-deterministic prefix would
//!   make two identical edits two different faces.

use document_reader_lib::editor::embed;

fn sans() -> &'static embed::Face {
    embed::face_for_style("Helvetica").expect("Helvetica maps to a bundled face")
}

#[test]
fn a_subset_reparses_as_a_true_type_face() {
    let face = sans();
    let subset = embed::subset(face, "H\u{0100}").expect("U+0100 is covered");
    let parsed = ttf_parser::Face::parse(&subset.program, 0)
        .expect("the written program must parse with the same parser a reader uses");
    assert!(
        parsed.number_of_glyphs() > 0,
        "a subset with no glyphs is not a font"
    );
    assert_eq!(
        parsed.units_per_em(),
        ttf_parser::Face::parse(face.bytes, 0).unwrap().units_per_em(),
        "the subset must keep the source's em, or every advance changes scale"
    );
}

/// The subset maps the characters it was built for, and nothing else.
///
/// This is the guard that would have caught a `cmap` written with the wrong segment
/// arithmetic: `ttf-parser` finds no glyph for the requested character, and the face renders
/// blanks while every other assertion in this file still passes.
#[test]
fn a_subset_maps_exactly_the_requested_characters() {
    let face = sans();
    let text = "H\u{0100}\u{2192}";
    let subset = embed::subset(face, text).expect("all three characters are covered");
    let parsed = ttf_parser::Face::parse(&subset.program, 0).unwrap();

    for ch in text.chars() {
        let id = parsed.glyph_index(ch);
        assert!(
            id.is_some_and(|id| id != ttf_parser::GlyphId(0)),
            "the subset does not map U+{:04X}, so the written run would render .notdef",
            ch as u32
        );
    }

    // And it does not map a character it was not built from. A `cmap` that answers for `Z` is
    // either a full copy or a segment covering more than it should.
    for ch in ['Z', 'Q', '\u{6f22}'] {
        assert!(
            parsed.glyph_index(ch).is_none(),
            "the subset maps U+{:04X}, which it was not built from",
            ch as u32
        );
    }
}

/// The size bound §1.1 requires: a subset is smaller than the face it came from.
///
/// Asserted as a real bound rather than "smaller than 400 KB", because the interesting failure
/// is a subsetter that copies every glyph and reports success.
#[test]
fn a_subset_is_much_smaller_than_the_source_face() {
    let face = sans();
    let subset = embed::subset(face, "H\u{0100}").expect("covered");
    let source = face.bytes.len();
    assert!(
        subset.program.len() < source / 4,
        "a two-glyph subset is {} B against a {source} B source face; subsetting is not \
         removing enough to justify the mechanism",
        subset.program.len()
    );

    // The other direction: a subset has to contain the glyph data it claims. A program under
    // a few hundred bytes cannot carry `H` and `Ā`.
    assert!(
        subset.program.len() > 200,
        "the subset is only {} B, which cannot carry the outlines it claims",
        subset.program.len()
    );
}

/// OFL condition 2 travels; condition 3's reserved name does not.
#[test]
fn a_subset_keeps_the_notice_and_drops_the_reserved_name() {
    let subset = embed::subset(sans(), "H\u{0100}").expect("covered");
    let parsed = ttf_parser::Face::parse(&subset.program, 0).unwrap();

    let record = |id: u16| {
        parsed
            .names()
            .into_iter()
            .find(|name| name.name_id == id && name.is_unicode())
            .and_then(|name| name.to_string())
            .unwrap_or_default()
    };
    assert!(
        record(0).contains("Red Hat"),
        "the copyright notice must travel with the subset (OFL condition 2): {:?}",
        record(0)
    );
    assert!(
        record(13).contains("SIL Open Font License"),
        "the licence identifier must travel with the subset: {:?}",
        record(13)
    );
    assert!(
        record(14).contains("scripts.sil.org/OFL"),
        "the licence URL must travel with the subset: {:?}",
        record(14)
    );

    // The written primary name. The family names (IDs 1/4/6) are deliberately absent.
    for id in [1, 4, 6] {
        assert_eq!(
            record(id),
            "",
            "name ID {id} names the face; a subset is a Modified Version and may not present \
             the Reserved Font Name"
        );
    }
    assert!(
        !subset.base_font.to_ascii_lowercase().contains("liberation"),
        "the written /BaseFont {:?} uses the Reserved Font Name",
        subset.base_font
    );
}

/// The prefix is six uppercase letters, and it is a function of the content.
#[test]
fn the_written_name_is_deterministic_and_prefixed() {
    let first = embed::subset(sans(), "H\u{0100}").unwrap();
    let again = embed::subset(sans(), "H\u{0100}").unwrap();
    assert_eq!(
        first.program, again.program,
        "the same input must produce the same bytes, or two identical edits are two faces"
    );
    assert_eq!(first.base_font, again.base_font);

    let (prefix, family) = first.base_font.split_once('+').expect("prefix+family");
    assert_eq!(prefix.len(), 6, "a subset prefix is six letters: {prefix:?}");
    assert!(
        prefix.chars().all(|c| c.is_ascii_uppercase()),
        "the prefix must be uppercase ASCII: {prefix:?}"
    );
    assert_eq!(family, "Sans-Regular");

    // A different replacement is a different subset, and must not collide.
    let other = embed::subset(sans(), "H\u{2192}").unwrap();
    assert_ne!(
        first.base_font, other.base_font,
        "two different subsets in one document must not share a name"
    );
}

/// Every bundled style subclasses successfully, not just the one the fixtures use.
#[test]
fn every_bundled_face_can_be_subset() {
    for (class, face) in embed::faces() {
        let subset = embed::subset(face, "H\u{0100}").unwrap_or_else(|error| {
            panic!("{class} ({}) could not be subset: {error}", face.file)
        });
        let parsed = ttf_parser::Face::parse(&subset.program, 0)
            .unwrap_or_else(|error| panic!("{class} subset did not parse: {error}"));
        assert!(
            parsed
                .glyph_index('\u{0100}')
                .is_some_and(|id| id != ttf_parser::GlyphId(0)),
            "{class} subset lost U+0100"
        );
    }
}

/// `/Widths` is indexed by code, and `advance` is indexed by character: both have to answer
/// for the text the caller will actually write.
#[test]
fn widths_are_indexed_by_the_codes_the_run_will_carry() {
    let subset = embed::subset(sans(), "H\u{0100}").expect("covered");
    assert_eq!(subset.codes.len(), 2);
    assert_eq!(
        subset.widths.len(),
        subset.codes.len(),
        "/Widths must have one entry per code"
    );
    assert_eq!(
        subset.encode("H\u{0100}"),
        Some(vec![1, 2]),
        "codes are assigned in first-use order starting at 1"
    );

    // H and Ā are not the same width in a proportional face, so a `/Widths` built in glyph
    // order rather than code order would still have the right length. The advance is what
    // distinguishes them.
    let h = subset.advance("H", 1000.0).unwrap();
    let a_macron = subset.advance("\u{0100}", 1000.0).unwrap();
    assert!(
        h > a_macron,
        "H ({h}) should be wider than Ā ({a_macron}) in Sans; if not, the code-to-width \
         mapping is wrong"
    );
}

/// The `/ToUnicode` CMap names every code the run carries.
#[test]
fn the_to_unicode_cmap_covers_every_code() {
    let subset = embed::subset(sans(), "H\u{0100}").expect("covered");
    let cmap = String::from_utf8(embed::to_unicode_cmap(&subset.codes)).unwrap();
    for (code, ch) in &subset.codes {
        let entry = format!("<{code:02X}> <{:04X}>", *ch as u32);
        assert!(
            cmap.contains(&entry),
            "the /ToUnicode CMap is missing {entry}; copy-paste would read the raw code"
        );
    }
    assert!(cmap.contains("begincodespacerange"));
    assert!(cmap.contains("endcmap"));
}

/// `/Differences` names a glyph for every code, using the `uniXXXX` convention.
#[test]
fn the_differences_array_names_every_code() {
    let subset = embed::subset(sans(), "H\u{0100}").expect("covered");
    let array = embed::differences_array(&subset.codes);
    assert_eq!(array.len(), subset.codes.len() * 2);
    for pair in array.chunks(2) {
        let code = pair[0].as_i64().unwrap();
        let name = String::from_utf8(pair[1].as_name().unwrap().to_vec()).unwrap();
        let ch = subset
            .codes
            .iter()
            .find(|(c, _)| i64::from(*c) == code)
            .map(|(_, ch)| *ch)
            .expect("every code has a character");
        assert_eq!(name, format!("uni{:04X}", ch as u32));
    }
}
