//! Programmatic PDFs for P2.4 tests, built with lopdf's public API only.
//!
//! The committed fixtures under `tests/fixtures/` are the gate (P2.4 §6): they are the
//! files a reviewer can open, and the integration tests read them from disk. These
//! builders exist for the *unit* tests, which need a document in memory and should not
//! depend on a binary blob's exact bytes. They generate the same shapes.
//!
//! Every gate has a shape here, including the two that cannot honestly be produced as a
//! committed fixture (see `tests/fixtures/README.md`): a subset-flagged font dictionary
//! and an unroundtrippable encoding are both constructible in memory, so unit tests cover
//! those gates even where the committed-fixture path is limited.

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Document, Object, Stream, StringFormat};

/// A `Dictionary` from string pairs, for tests that need a font dictionary without a
/// document around it.
#[cfg(test)]
pub fn dictionary_for_test(pairs: &[(&str, &str)]) -> Dictionary {
    let mut dictionary = Dictionary::new();
    for (key, value) in pairs {
        dictionary.set(key.to_string(), Object::Name(value.as_bytes().to_vec()));
    }
    dictionary
}

/// A content stream holding the given operations.
fn page_body(operations: Vec<Operation>) -> Stream {
    let content = Content { operations };
    Stream::new(dictionary! {}, content.encode().unwrap())
}

/// The operations for a page with one 24pt `Tj` run of `text`.
fn single_run(text: &str) -> Vec<Operation> {
    vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new("Tj", vec![Object::string_literal(text)]),
        Operation::new("ET", vec![]),
    ]
}

/// A page whose only font is a **non-embedded** simple font that publishes `/Widths` — the
/// shape the Maastricht brochure uses for its Times/Arial resources.
pub fn simple_font_widths_page() -> Document {
    let widths: Vec<Object> = (32..=126).map(|_| 500.into()).collect();
    document_with_pages(
        dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "DejaVuSans",
            "FirstChar" => 32,
            "LastChar" => 126,
            "Widths" => Object::Array(widths),
            "Encoding" => "WinAnsiEncoding",
        },
        vec![single_run("Test")],
    )
}

/// A page whose font publishes no `/Widths` and embeds no program: genuinely unmeasurable.
pub fn no_widths_page() -> Document {
    document_with_pages(
        dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "DejaVuSans",
            "Encoding" => "WinAnsiEncoding",
        },
        vec![single_run("x")],
    )
}

/// Wraps content streams into a one-page document using `font` as `/F1`.
fn document_with_pages(font: Dictionary, pages: Vec<Vec<Operation>>) -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(font);
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut kids = Vec::new();
    for operations in pages {
        let content_id = document.add_object(page_body(operations));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => resources_id,
        });
        kids.push(Object::Reference(page_id));
    }

    let count = kids.len() as i64;
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(kids),
            "Count" => count,
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

/// The Type1 Helvetica + `/WinAnsiEncoding` font (the best case, matching `sample.pdf`).
fn winansi_helvetica() -> Dictionary {
    dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    }
}

/// The 3-page best-case fixture, matching `tests/fixtures/sample.pdf`.
pub fn simple_winansi() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![
            vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
                Operation::new("ET", vec![]),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 657.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal(
                        "Document Reader P1 fixture, page one.",
                    )],
                ),
                Operation::new("ET", vec![]),
            ],
            vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("BRAVO")]),
                Operation::new("ET", vec![]),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 657.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal(
                        "Second page mentions zephyrium so search can find it.",
                    )],
                ),
                Operation::new("ET", vec![]),
            ],
            vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("CHARLIE")]),
                Operation::new("ET", vec![]),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 657.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal(
                        "Third and final page of the fixture.",
                    )],
                ),
                Operation::new("ET", vec![]),
            ],
        ],
    )
}

/// A one-page document whose single page has an empty content stream, ready for a test
/// to install its own operations with `change_page_content`.
pub fn blank_page() -> Document {
    let mut document = document_with_pages(winansi_helvetica(), vec![vec![]]);
    // `document_with_pages` gives the page an empty stream already; nothing more to do.
    document.renumber_objects();
    document
}

/// One page carrying a single `Tj` run, with a caller-chosen font dictionary.
pub fn page_with_run(text: &str, font: Dictionary) -> Document {
    document_with_pages(font, vec![single_run(text)])
}

/// Gate (c): a subset-flagged TrueType font (`ABCDEF+...`).
pub fn subset_font_page() -> Document {
    page_with_run("SUBSET", subset_font())
}

/// Gate (a): a CID-keyed Type0 font with an Identity-H encoding.
pub fn cid_identity_page() -> Document {
    page_with_run(
        "CID",
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "Identity-H",
            "Encoding" => "Identity-H",
        },
    )
}

/// P2.7.5: a Type0 page that is **editable** — the positive counterpart to
/// [`cid_identity_page`].
///
/// Codes `<0001>`–`<0003>` draw `A`, `B`, `C`, and the run shows `<0001><0002>` ("AB").
/// The `/ToUnicode` names the character behind each code, so an encoder has something to
/// reverse; `/W` gives each cid an exact advance, so the width gate is measurable. No program
/// is embedded, which keeps the fixture to the CMap path — the route that decides *meaning*
/// and therefore the one every CID edit must be able to take on its own.
pub fn cid_tounicode_page() -> Document {
    cid_page(
        b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\nbegincmap\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
3 beginbfchar\n<0001> <0041>\n<0002> <0042>\n<0003> <0043>\nendbfchar\n\
endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n",
    )
}

/// A Type0 page whose CMap also maps the code `0x0000`, which no real producer writes.
///
/// Splitting a two-byte code into bytes therefore yields a *plausible* string: code `0002`
/// (`B`) reads as `AA`. A test that only checks "the Type0 path returns the right text" passes
/// against a broken byte-split reader; this fixture makes the two paths disagree visibly.
pub fn cid_trap_cmap_page() -> Document {
    cid_page(
        b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\nbegincmap\n\
/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
3 beginbfchar\n<0000> <0041>\n<0002> <0042>\n<0003> <0043>\nendbfchar\n\
endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n",
    )
}

fn cid_page(cmap: &[u8]) -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();

    let mut cmap_stream = Stream::new(dictionary! {}, cmap.to_vec());
    cmap_stream.compress().unwrap();
    let cmap_id = document.add_object(cmap_stream);

    let descendant_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "CidFixture",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "CIDToGIDMap" => "Identity",
        "DW" => 500,
        "W" => vec![1.into(), vec![600.into(), 600.into(), 600.into()].into()],
    });

    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "CidFixture",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(descendant_id)],
        "ToUnicode" => cmap_id,
    });

    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let content_id = document.add_object(page_body(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 10.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new(
            "Tj",
            vec![Object::String(
                vec![0x00, 0x01, 0x00, 0x02],
                StringFormat::Hexadecimal,
            )],
        ),
        Operation::new("ET", vec![]),
    ]));

    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
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

/// P2.7.5 C7: a Type0 page whose `/FontFile2` is **compressed**, and whose `/ToUnicode` names
/// only the characters of `run_text`.
///
/// This is the shape every real document has, and the one that exposed the `program_of` bug:
/// the stream content is deflate-compressed, so a reader that skips decompression hands
/// `ttf-parser` bytes it cannot parse and silently concludes the font covers nothing. The
/// program is built from `program_glyphs`, so a character the CMap omits but the program has
/// is reachable *only* if route 2 works.
pub fn compressed_program_cid_page(program_glyphs: &[char]) -> Document {
    let mut document = cid_tounicode_page();
    let page_id = document.get_pages().values().next().copied().unwrap();
    // `font_dictionary_at` is `pub(crate)`, so this `#[path]`-included module cannot call it.
    // The page-level lookup it performs is spelled out here; `font_dictionary_at` is what the
    // product uses and what the tests exercise, so this is a fixture-side duplicate of a
    // one-level lookup, not a second implementation of resolution.
    let font = {
        let (inline, referenced) = document.get_page_resources(page_id).unwrap();
        let mut dictionaries: Vec<&lopdf::Dictionary> = inline.into_iter().collect();
        dictionaries.extend(
            referenced
                .iter()
                .filter_map(|id| document.get_dictionary(*id).ok()),
        );
        dictionaries
            .into_iter()
            .find_map(|resources| {
                let fonts = resources.get(b"Font").ok()?;
                let fonts = document.dereference(fonts).ok()?.1.as_dict().ok()?;
                document
                    .dereference(fonts.get(b"F1").ok()?)
                    .ok()?
                    .1
                    .as_dict()
                    .ok()
                    .cloned()
            })
            .unwrap()
    };
    let descendant_id = font.get(b"DescendantFonts").unwrap().as_array().unwrap()[0]
        .as_reference()
        .unwrap();

    let mut program = Stream::new(
        dictionary! {},
        true_type_program_with_advance(program_glyphs, 1000),
    );
    program.compress().unwrap();
    let program_id = document.add_object(program);
    let descriptor_id = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "CidFixture",
        "Flags" => 32,
        "FontFile2" => program_id,
    });

    let descendant = document
        .get_object_mut(descendant_id)
        .unwrap()
        .as_dict_mut()
        .unwrap();
    descendant.set("FontDescriptor", descriptor_id);

    document
}

/// Gate (b): a font whose `/Differences` map cannot round-trip a byte.
///
/// `space` and `nbspace` both denote U+0020, so lopdf's inverse map keeps only one of
/// them. A byte that decoded to U+0020 re-encodes to whichever byte won, which is not
/// necessarily the byte that was read — the exact failure gate (b) exists to catch.
pub fn roundtrip_fail_page() -> Document {
    page_with_run(
        "\u{20}",
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Unroundtrippable",
            "Encoding" => dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "WinAnsiEncoding",
                "Differences" => vec![
                    Object::Integer(65),
                    Object::Name(b"space".to_vec()),
                    Object::Name(b"nbspace".to_vec()),
                ],
            },
        },
    )
}

/// Gate (e) / F3: a page whose content stream is a decompression bomb.
pub fn bomb() -> Document {
    let mut document = blank_page();
    let page_id = document.get_pages().values().next().copied().unwrap();

    // 16 MiB of repetitive content compresses to a few kilobytes; the cap is 8 MiB.
    let inflated = vec![b'A'; 16 * 1024 * 1024];
    let mut stream = Stream::new(dictionary! {}, inflated);
    stream.compress().unwrap();
    let content_id = document.add_object(stream);
    document
        .get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Contents", content_id);
    document
}

/// Gate (d): a page with one short run, so a plausible replacement overflows its advance
/// width. The gate is evaluated at edit time from measured widths (PDFium, read-only), so
/// the fixture's job is to be a real document with a genuinely narrow run to widen.
pub fn too_wide_page() -> Document {
    page_with_run("HI", winansi_helvetica())
}

/// F2: a page whose `/Contents` is an array of two streams, each with its own run.
pub fn multi_stream() -> Document {
    let mut document = blank_page();
    let page_id = document.get_pages().values().next().copied().unwrap();

    let first = document.add_object(page_body(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
        Operation::new("Tj", vec![Object::string_literal("FIRST")]),
        Operation::new("ET", vec![]),
    ]));
    let second = document.add_object(page_body(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
        Operation::new("Tj", vec![Object::string_literal("SECOND")]),
        Operation::new("ET", vec![]),
    ]));
    document
        .get_object_mut(page_id)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set(
            "Contents",
            Object::Array(vec![Object::Reference(first), Object::Reference(second)]),
        );
    document
}

/// An AES-128 encrypted copy of [`simple_winansi`], with user password `secret`.
///
/// The only fixture whose builder needs `Document::encrypt`, which is **forbidden in the
/// product** (T9) but required to produce the fixture. It therefore lives in a
/// `#[cfg(any(test, feature = "testfixtures"))]` block and the generator is built with
/// that feature, so the shipped library cannot reach it.
#[cfg(any(test, feature = "testfixtures"))]
pub fn encrypted_aes() -> Document {
    use lopdf::encryption::crypt_filters::{Aes128CryptFilter, CryptFilter};
    use lopdf::{EncryptionState, EncryptionVersion, Permissions};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let mut document = simple_winansi();
    // Encryption needs a file `/ID`; without one lopdf fails with `MissingFileID`.
    document.trailer.set(
        "ID",
        Object::Array(vec![
            Object::string_literal(b"0123456789abcdef".to_vec()),
            Object::string_literal(b"0123456789abcdef".to_vec()),
        ]),
    );

    let mut filters: BTreeMap<Vec<u8>, Arc<dyn CryptFilter>> = BTreeMap::new();
    filters.insert(b"StdCF".to_vec(), Arc::new(Aes128CryptFilter));
    let state: EncryptionState = EncryptionVersion::V4 {
        document: &document,
        encrypt_metadata: false,
        crypt_filters: filters,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: "owner",
        user_password: "secret",
        permissions: Permissions::all(),
    }
    .try_into()
    .unwrap();

    document.encrypt(&state).unwrap();
    document
}

// --- Stage 2: committed fixture builders (§6) ---------------------------------

/// §6 `style-match-cases.pdf`: one page per original font name, so a test can drive the
/// name heuristic and assert each maps to the documented standard-14 family. The names
/// are subset-prefixed on purpose: the prefix must be stripped before matching.
pub fn style_match_cases() -> Document {
    let names = [
        "ABCDEF+TimesNewRoman",
        "ABCDEF+TimesNewRoman-BoldItalic",
        "ABCDEF+CourierNew",
        "ABCDEF+DejaVuSerif",
        "ABCDEF+Arial",
        "ABCDEF+Arial-Bold",
        "ABCDEF+Helvetica-Oblique",
    ];
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();

    let mut kids = Vec::new();
    for name in names {
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => name,
            "Encoding" => "WinAnsiEncoding",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content_id = document.add_object(page_body(single_run("SAMPLE")));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => resources_id,
        });
        kids.push(Object::Reference(page_id));
    }

    let count = kids.len() as i64;
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(kids),
            "Count" => count,
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

// --- P2.7.3: Form XObject fixtures (design §8) ---------------------------------

/// How a fixture's form declares `/Resources`.
enum FormResources {
    /// No `/Resources`: every name resolves through the enclosing level (ISO 32000-1 §8.10).
    Inherit,
    /// A private dictionary naming `/F1` plus the child XObjects listed (a nested form, a
    /// cycle, or no children at all).
    Own(Vec<String>),
    /// One **indirect** `/Resources` object shared by every `Shared` form, naming `/F1` plus
    /// the children they list. The shape Q3's private-copy rule exists for.
    Shared(Vec<String>),
}

/// The operations that draw one run through the resource name `F1`.
fn form_run(text: &str) -> Vec<Operation> {
    form_run_in(text, "F1", 700)
}

/// The operations that draw one run through a named resource, at a given baseline.
fn form_run_in(text: &str, font: &str, y: i64) -> Vec<Operation> {
    vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec![font.into(), 24.into()]),
        Operation::new("Td", vec![72.into(), y.into()]),
        Operation::new("Tj", vec![Object::string_literal(text)]),
        Operation::new("ET", vec![]),
    ]
}

/// One 24pt run of `text` at `y`, drawn **on the page** rather than inside a form.
fn text_at(text: &str, y: f64) -> Vec<Operation> {
    vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), y.into()]),
        Operation::new("Tj", vec![Object::string_literal(text)]),
        Operation::new("ET", vec![]),
    ]
}

/// One `Do` of a named form, as a page (or an enclosing form) writes it.
fn draw_form(name: &str) -> Vec<Operation> {
    vec![Operation::new("Do", vec![name.into()])]
}

/// Builds a document from named forms and one or more pages.
///
/// Form ids are allocated **before** any stream is built, so a form can name a child or
/// itself — the shapes `xobject-nested`, `xobject-depth-9` and `xobject-cycle` need. The
/// page's `/Resources` names `/F1` and every form, so a locator that resolves fonts against
/// the page still finds one by accident; the nested fixture is what tells the two readings
/// apart, and `xobject-shared-resources` is what tells the resource ownership apart.
fn xobject_document(
    forms: Vec<(String, Vec<Operation>, FormResources)>,
    pages: Vec<Vec<Operation>>,
) -> Document {
    xobject_document_with_font(forms, pages, winansi_helvetica())
}

/// The same builder with a caller-chosen `/F1` font, for the shapes where the *font* is what
/// the test is about (a subset that forces MECH-A substitution inside a form).
fn xobject_document_with_font(
    forms: Vec<(String, Vec<Operation>, FormResources)>,
    pages: Vec<Vec<Operation>>,
    font: Dictionary,
) -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(font);

    let ids: Vec<lopdf::ObjectId> = forms.iter().map(|_| document.new_object_id()).collect();
    let names: Vec<String> = forms.iter().map(|(name, _, _)| name.clone()).collect();
    let id_of = |name: &str| -> lopdf::ObjectId {
        let index = names
            .iter()
            .position(|candidate| candidate == name)
            .unwrap_or_else(|| panic!("a fixture may only name a form it declares: {name}"));
        ids[index]
    };

    let mut page_xobjects = Dictionary::new();
    let mut shared_resources: Option<lopdf::ObjectId> = None;
    for (index, (name, operations, resources)) in forms.into_iter().enumerate() {
        let mut form = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        };
        if let FormResources::Own(children) = &resources {
            let mut xobjects = Dictionary::new();
            for child in children {
                xobjects.set(child.as_str(), Object::Reference(id_of(child)));
            }
            form.set(
                "Resources",
                Object::Dictionary(dictionary! {
                    "Font" => dictionary! { "F1" => font_id },
                    "XObject" => xobjects,
                }),
            );
        }
        if let FormResources::Shared(children) = &resources {
            let resources_id = *shared_resources.get_or_insert_with(|| {
                let mut xobjects = Dictionary::new();
                for child in children {
                    xobjects.set(child.as_str(), Object::Reference(id_of(child)));
                }
                document.add_object(dictionary! {
                    "Font" => dictionary! { "F1" => font_id },
                    "XObject" => xobjects,
                })
            });
            form.set("Resources", Object::Reference(resources_id));
        }
        document.set_object(
            ids[index],
            Stream::new(form, Content { operations }.encode().unwrap()),
        );
        page_xobjects.set(name.as_str(), Object::Reference(ids[index]));
    }

    let page_resources = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "XObject" => page_xobjects,
    });
    let mut kids = Vec::new();
    for operations in pages {
        let content = document.add_object(page_body(operations));
        kids.push(Object::Reference(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content,
            "Resources" => page_resources,
        })));
    }
    let count = kids.len() as i64;
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );
    let catalog_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog_id);
    document.renumber_objects();
    document
}

/// `xobject-single.pdf`: the page draws `/X1 Do`, and `X1` draws `ALPHA`.
pub fn xobject_single() -> Document {
    xobject_document(
        vec![(
            "X1".into(),
            form_run("ALPHA"),
            FormResources::Own(Vec::new()),
        )],
        vec![draw_form("X1")],
    )
}

/// `xobject-nested.pdf`: the page draws `X1`, `X1` draws `X2`, `X2` draws `ALPHA`.
///
/// This is the fixture that separates a level-by-level walk (Q1's reading, which reaches
/// depth 2) from a one-level walk, which cannot see the run at all.
pub fn xobject_nested() -> Document {
    xobject_document(
        vec![
            (
                "X1".into(),
                draw_form("X2"),
                FormResources::Own(vec!["X2".into()]),
            ),
            (
                "X2".into(),
                form_run("ALPHA"),
                FormResources::Own(Vec::new()),
            ),
        ],
        vec![draw_form("X1")],
    )
}

/// `xobject-inherited-resources.pdf`: `X1` declares no `/Resources` at all, so its `/F1` and
/// its `/X2` resolve only through the page's dictionary (ISO 32000-1 §8.10).
///
/// Producers emit this shape routinely. A walker that replaces the resource stack whole
/// instead of falling back per key finds no font and no child here, and the text silently
/// disappears — the exact failure this phase exists to remove.
pub fn xobject_inherited_resources() -> Document {
    xobject_document(
        vec![
            ("X1".into(), draw_form("X2"), FormResources::Inherit),
            (
                "X2".into(),
                form_run("ALPHA"),
                FormResources::Own(Vec::new()),
            ),
        ],
        vec![draw_form("X1")],
    )
}

/// `xobject-cycle.pdf`: `X1` draws itself. Legal to write, and a naive walker never returns.
pub fn xobject_cycle() -> Document {
    xobject_document(
        vec![(
            "X1".into(),
            draw_form("X1"),
            FormResources::Own(vec!["X1".into()]),
        )],
        vec![draw_form("X1")],
    )
}

/// `xobject-depth-9.pdf`: the chain `X1 → … → X9`, the leaf drawing `ALPHA`.
///
/// The page is depth 0, so `X8` sits at depth 8 (editable, the cap) and `X9` at depth 9
/// (refused `nested-xobject`).
pub fn xobject_depth_nine() -> Document {
    let names: Vec<String> = (1..=9).map(|number| format!("X{number}")).collect();
    let forms = names
        .iter()
        .enumerate()
        .map(|(index, name)| match names.get(index + 1) {
            // The last form that is still within the cap draws its own text *and* the one
            // level that is over it, so one fixture proves both halves of the boundary.
            Some(child) if index + 1 == 8 => {
                let mut operations = form_run("EDGE");
                operations.extend(draw_form(child));
                (
                    name.clone(),
                    operations,
                    FormResources::Own(vec![child.clone()]),
                )
            }
            Some(child) => (
                name.clone(),
                draw_form(child),
                FormResources::Own(vec![child.clone()]),
            ),
            None => (
                name.clone(),
                form_run("ALPHA"),
                FormResources::Own(Vec::new()),
            ),
        })
        .collect();
    xobject_document(forms, vec![draw_form("X1")])
}

/// `xobject-two-draws.pdf`: the same `/X1` drawn twice on one page, the second after a `cm`
/// translation, so the two draws have different `Do` indices and different places on the page.
///
/// This is the fixture Q2 is about: editing the draw the user clicked must not change the
/// other one.
pub fn xobject_two_draws() -> Document {
    xobject_document(
        vec![(
            "X1".into(),
            form_run("ALPHA"),
            FormResources::Own(Vec::new()),
        )],
        vec![{
            let mut operations = draw_form("X1");
            operations.push(Operation::new(
                "cm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    0.into(),
                    (-100).into(),
                ],
            ));
            operations.extend(draw_form("X1"));
            operations
        }],
    )
}

/// `xobject-interleaved.pdf`: page-level text, then a form, then page-level text.
///
/// The order is the whole point. A walk that emits a form's children before the text that
/// precedes the draw, or after the text that follows it, produces a run order that no longer
/// matches `page.text()`; the picker would then pair each fragment to the wrong character
/// window. `emit-at-visit` is what keeps the two orders equal, and this fixture is the only
/// one whose shape can tell the difference.
pub fn xobject_interleaved() -> Document {
    xobject_document(
        vec![(
            "X1".into(),
            form_run("ALPHA"),
            FormResources::Own(Vec::new()),
        )],
        vec![{
            let mut operations = text_at("BEFORE", 600.0);
            operations.extend(draw_form("X1"));
            operations.extend(text_at("AFTER", 500.0));
            operations
        }],
    )
}

/// `xobject-two-pages.pdf`: `/X1` drawn on page 1 and page 2, with both pages sharing one
/// indirect `/Resources` object.
///
/// Two things are at stake: the edit must be page-private (Q2), and adding the clone's name to
/// a `/Resources` object two pages read must not mutate it for the other page (§5.1).
pub fn xobject_two_pages() -> Document {
    xobject_document(
        vec![(
            "X1".into(),
            form_run("ALPHA"),
            FormResources::Own(Vec::new()),
        )],
        vec![draw_form("X1"), draw_form("X1")],
    )
}

/// `xobject-shared-resources.pdf`: `X1` and `X2` share one **indirect** `/Resources` object.
///
/// A substitute font installed into that object would be visible to both forms — B-7 one level
/// down, which Q3's private copy exists to prevent.
pub fn xobject_shared_resources() -> Document {
    // The subset font is what makes the edit need the resources dictionary: keeping the
    // original is impossible, so a substitute has to be installed somewhere (Q3).
    xobject_document_with_font(
        vec![
            (
                "X1".into(),
                form_run("ALPHA"),
                FormResources::Shared(Vec::new()),
            ),
            (
                "X2".into(),
                form_run("BRAVO"),
                FormResources::Shared(Vec::new()),
            ),
        ],
        vec![{
            let mut operations = draw_form("X1");
            operations.push(Operation::new(
                "cm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    0.into(),
                    (-100).into(),
                ],
            ));
            operations.extend(draw_form("X2"));
            operations
        }],
        subset_font(),
    )
}

/// `xobject-subset-font.pdf`: `X1`'s own `/Resources` names a subset-flagged font, so the edit
/// must substitute a standard-14 face (MECH-A) — the case that has to give the clone a
/// **private** `/Resources` rather than write `FS1` into the page's or the form's dictionary.
///
/// `X1`'s dictionary is **inline** and names a sibling `/XObject` as well as the font. The
/// private copy therefore has to carry `/XObject` over: dropping it leaves the clone's trailing
/// `/X2 Do` naming nothing. Both branches of the copy matter — this fixture is the inline one,
/// `xobject-shared-resources.pdf` is the indirect one.
pub fn xobject_subset_font() -> Document {
    let mut operations = form_run("ALPHA");
    operations.extend(draw_form("X2"));
    xobject_document_with_font(
        vec![
            (
                "X1".into(),
                operations,
                FormResources::Own(vec!["X2".into()]),
            ),
            (
                "X2".into(),
                form_run("BRAVO"),
                FormResources::Own(Vec::new()),
            ),
        ],
        vec![draw_form("X1")],
        subset_font(),
    )
}

/// A one-form document whose form declares a caller-supplied `/Resources` **inline**, with no
/// `/F1` injected: the fixture's whole point is which resource names the form itself declares.
fn xobject_document_with_inline_resources(
    forms: Vec<(String, Vec<Operation>, Dictionary)>,
    pages: Vec<Vec<Operation>>,
) -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();

    let ids: Vec<lopdf::ObjectId> = forms.iter().map(|_| document.new_object_id()).collect();
    let mut page_xobjects = Dictionary::new();
    for (index, (name, operations, resources)) in forms.into_iter().enumerate() {
        let form = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => resources,
        };
        document.set_object(
            ids[index],
            Stream::new(form, Content { operations }.encode().unwrap()),
        );
        page_xobjects.set(name.as_str(), Object::Reference(ids[index]));
    }

    let page_resources = document.add_object(dictionary! { "XObject" => page_xobjects });
    let mut kids = Vec::new();
    for operations in pages {
        let content = document.add_object(page_body(operations));
        kids.push(Object::Reference(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content,
            "Resources" => page_resources,
        })));
    }
    let count = kids.len() as i64;
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );
    let catalog_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog_id);
    document.renumber_objects();
    document
}

/// `xobject-subset-font-siblings.pdf`: `X1`'s inline `/Resources` names the subset font under
/// **two** names, and both are drawn.
///
/// A substitution rewrites only the entry the edited run resolves. The other entry is a
/// different resource name for an object the clone still needs — dropping it (or replacing the
/// whole `/Font` map with just the substitute) leaves the clone's second run naming a font its
/// own dictionary no longer defines.
pub fn xobject_subset_font_siblings() -> Document {
    let mut operations = form_run("ALPHA");
    operations.extend(form_run_in("KEEPME", "F2", 600));
    let resources = dictionary! {
        "Font" => dictionary! {
            "F1" => subset_font(),
            "F2" => winansi_helvetica(),
        },
        "XObject" => Dictionary::new(),
    };
    xobject_document_with_inline_resources(
        vec![("X1".into(), operations, resources)],
        vec![draw_form("X1")],
    )
}

/// A subset-flagged simple font: no standard-14 name, no embedded program, so an edit has to
/// substitute (MECH-A) rather than keep it.
fn subset_font() -> Dictionary {
    dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "ABCDEF+SomeFont",
        "Encoding" => "WinAnsiEncoding",
    }
}

/// A form's stream, addressed by the resource names that reach it from a page.
///
/// Tests read the written document through this instead of through the picker: a name inside a
/// clone's resources is only reachable by descending, which is the same walk the writer does.
pub fn form_stream<'a>(
    document: &'a Document,
    page_id: lopdf::ObjectId,
    path: &[&str],
) -> Option<&'a lopdf::Stream> {
    let mut current: Option<&lopdf::Stream> = None;
    let mut resources = page_resources(document, page_id);
    for name in path {
        let dictionaries = resources.take()?;
        let stream = find_form(document, &dictionaries, name)?;
        resources = Some(form_resources(document, stream));
        current = Some(stream);
    }
    current
}

/// The resource dictionaries a page resolves, inline first, then the referenced ones.
fn page_resources(document: &Document, page_id: lopdf::ObjectId) -> Option<Vec<&Dictionary>> {
    let (inline, referenced) = document.get_page_resources(page_id).ok()?;
    let mut dictionaries: Vec<&Dictionary> = inline.into_iter().collect();
    dictionaries.extend(
        referenced
            .iter()
            .filter_map(|id| document.get_dictionary(*id).ok()),
    );
    Some(dictionaries)
}

/// A form's own `/Resources`, if it declares any.
fn form_resources<'a>(document: &'a Document, form: &'a lopdf::Stream) -> Vec<&'a Dictionary> {
    form.dict
        .get(b"Resources")
        .ok()
        .and_then(|entry| document.dereference(entry).ok())
        .and_then(|(_, object)| object.as_dict().ok())
        .into_iter()
        .collect()
}

fn find_form<'a>(
    document: &'a Document,
    dictionaries: &[&'a Dictionary],
    name: &str,
) -> Option<&'a lopdf::Stream> {
    dictionaries.iter().rev().find_map(|resources| {
        let xobjects = resources
            .get(b"XObject")
            .ok()
            .and_then(|entry| document.dereference(entry).ok())
            .and_then(|(_, object)| object.as_dict().ok())?;
        xobjects
            .get(name.as_bytes())
            .ok()
            .and_then(|object| document.dereference(object).ok())
            .and_then(|(_, object)| object.as_stream().ok())
    })
}

/// The decoded operations of a form's stream, reached by resource names from the page.
pub fn form_operations(
    document: &Document,
    page_id: lopdf::ObjectId,
    path: &[&str],
) -> Option<Vec<lopdf::content::Operation>> {
    let stream = form_stream(document, page_id, path)?;
    let bytes = stream.decompressed_content().ok()?;
    lopdf::content::Content::decode(&bytes)
        .ok()
        .map(|content| content.operations)
}

/// Serialises a document to bytes.
pub fn bytes(document: &Document) -> Vec<u8> {
    let mut copy = document.clone();
    let mut out = Vec::new();
    copy.save_to(&mut out).unwrap();
    out
}

// --- P2.4.1: fragmented-run fixtures (§5) --------------------------------------

/// Appends the operations for one per-glyph `TJ` run to `operations`, starting at `x`.
fn push_per_glyph_run(operations: &mut Vec<Operation>, text: &str, size: i64, x: i64, y: i64) {
    operations.push(Operation::new("BT", vec![]));
    operations.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    operations.push(Operation::new("Td", vec![x.into(), y.into()]));

    // One string per glyph, each followed by a kerning number, exactly as Word and
    // PowerPoint emit a kerned word: `[(H) -12 (e) -8 (l) 0 (l) 6 (o)] TJ`.
    let mut array = Vec::new();
    for (index, ch) in text.chars().enumerate() {
        array.push(Object::string_literal(ch.to_string()));
        if index + 1 < text.chars().count() {
            // A small negative kern keeps the glyphs adjacent without overlapping.
            array.push(Object::Integer(-4));
        }
    }
    operations.push(Operation::new("TJ", vec![Object::Array(array)]));
    operations.push(Operation::new("ET", vec![]));
}

/// §5 `fragmented-tj.pdf`: a per-glyph `TJ` array, WinAnsi subset.
///
/// This is the exact shape the 0.4.0 pairing bug could not handle: `TextEditPanel` offers one
/// picker entry per *content-stream operand*, so this word is five operands, while PDFium
/// reports it as one run. The gate the fixture must provoke is that pairing still succeeds and
/// every fragment is editable — not the `run-mismatch` flood 0.4.0 produced.
pub fn fragmented_tj() -> Document {
    let mut operations = Vec::new();
    push_per_glyph_run(&mut operations, "Hello", 12, 72, 700);
    document_with_pages(winansi_helvetica(), vec![operations])
}

/// §5 `fragmented-mixed-fonts.pdf`: two per-glyph runs in different fonts on one baseline.
///
/// T19: fragments may only merge when they share a font **and** a size. Here "AB" is 12pt
/// Helvetica and "CD" is 10pt Times with only a small gap between them, so a merger that
/// ignored font or size would join them into one unencodable run. Nothing here may merge
/// across the seam; per-fragment picking still works on both sides.
pub fn fragmented_mixed_fonts() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let regular = document.add_object(winansi_helvetica());
    let other = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Times-Roman",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => regular, "F2" => other },
    });

    let mut operations = Vec::new();
    push_per_glyph_run(&mut operations, "AB", 12, 72, 700);
    // One point to the right: within a 12pt gap tolerance, so only the font/size difference
    // can prevent a merge. `Td` is relative to the previous line matrix.
    operations.push(Operation::new("BT", vec![]));
    operations.push(Operation::new("Tf", vec!["F2".into(), 10.into()]));
    operations.push(Operation::new("Td", vec![14.into(), 700.into()]));
    let mut array = Vec::new();
    for ch in "CD".chars() {
        array.push(Object::string_literal(ch.to_string()));
        array.push(Object::Integer(-4));
    }
    array.pop();
    operations.push(Operation::new("TJ", vec![Object::Array(array)]));
    operations.push(Operation::new("ET", vec![]));

    let content_id = document.add_object(page_body(operations));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// §5 `fragmented-unpairable.pdf`: a real `Tj` run with **no** rendered box.
///
/// The unpaired run is drawn at font size 0. lopdf reads its operand and gates it as ordinary
/// text, but PDFium renders nothing at size 0 and reports no box at all — there is genuinely
/// nothing to pair with. T21: the refusal must *name* that no rendered box was found, rather
/// than the generic run-mismatch sentence. A normal per-glyph run on the same page pairs
/// correctly, so the fixture proves the refusal is specific to the unpaired run.
pub fn fragmented_unpairable() -> Document {
    let mut operations = Vec::new();
    push_per_glyph_run(&mut operations, "GHOST", 12, 72, 700);
    operations.push(Operation::new("BT", vec![]));
    operations.push(Operation::new("Tf", vec!["F1".into(), 0.into()]));
    operations.push(Operation::new("Td", vec![72.into(), 650.into()]));
    operations.push(Operation::new(
        "Tj",
        vec![Object::string_literal("ZEROSIZE")],
    ));
    operations.push(Operation::new("ET", vec![]));
    document_with_pages(winansi_helvetica(), vec![operations])
}

/// §5 `fragmented-wider-merged.pdf`: a merged run whose replacement is wider than its box.
///
/// The merged rect is the union of the fragments' boxes; a plausibly longer replacement
/// overflows it and must refuse per O1 quoting the **merged** measured width.
pub fn fragmented_wider_merged() -> Document {
    let mut operations = Vec::new();
    push_per_glyph_run(&mut operations, "Hi", 12, 72, 700);
    document_with_pages(winansi_helvetica(), vec![operations])
}

// --- Stage 2: embedded-subset fixtures (T11) ----------------------------------

/// A minimal, valid TrueType program whose `cmap` maps exactly `glyphs`, each with an explicit
/// `hmtx` `advanceWidth`.
///
/// Stage 2's T11 coverage check reads the *actual* glyph set of an embedded program, so
/// the fixtures need real, parseable program bytes. Rather than commit a system font
/// (whose bytes vary by machine and are not ours to distribute), the fixture carries this
/// hand-synthesised sfnt: `head`, `hhea`, `maxp`, `hmtx`, `cmap` (format 4), `loca` and
/// `glyf` — the tables `ttf-parser` reads, and no more. Coverage is exact, so a "subset
/// lacks the glyph" case is real rather than a name convention.
///
/// The P2.4.2 chain-break fixtures need the metric to be *larger* than the `0.5em`
/// per-character approximation the 0.4.1 code fell back to, so a chain that genuinely
/// continues is measurably broken by the approximation. `600`/1000 is a hair
/// under `0.5em`; `1000`/1000 clears it by a wide margin.
pub fn true_type_program_with_advance(glyphs: &[char], advance_width: u16) -> Vec<u8> {
    fn u16be(value: u16) -> [u8; 2] {
        value.to_be_bytes()
    }
    fn u32be(value: u32) -> [u8; 4] {
        value.to_be_bytes()
    }

    let mut segments: Vec<(u16, u16)> = {
        // Deduplicate: a repeated character (the fixtures intentionally reuse letters)
        // would emit two segments with the same code and break the `cmap`'s strictly
        // increasing order — the lookup then fails for every character, not just that one.
        let mut seen: Vec<char> = Vec::new();
        let mut mapped: Vec<(u16, u16)> = Vec::new();
        for ch in glyphs {
            if seen.contains(ch) {
                continue;
            }
            seen.push(*ch);
            mapped.push((*ch as u16, seen.len() as u16));
        }
        mapped
    };
    segments.sort_unstable();
    let num_glyphs = (segments.len() + 1) as u16;

    let mut body = Vec::new();
    let seg_count = (segments.len() + 1) as u16;
    let entry_selector = 15 - seg_count.leading_zeros() as u16;
    let search_range = 2 * (1u16 << entry_selector);
    // The first field is `segCountX2`, not `segCount` (the spec stores it doubled, and
    // ttf-parser's parser divides it back out).
    body.extend_from_slice(&u16be(seg_count * 2));
    body.extend_from_slice(&u16be(search_range));
    body.extend_from_slice(&u16be(entry_selector));
    body.extend_from_slice(&u16be(seg_count * 2 - search_range));
    for (code, _) in &segments {
        body.extend_from_slice(&u16be(*code));
    }
    body.extend_from_slice(&u16be(0xFFFF));
    body.extend_from_slice(&u16be(0));
    for (code, _) in &segments {
        body.extend_from_slice(&u16be(*code));
    }
    body.extend_from_slice(&u16be(0xFFFF));
    for (code, gid) in &segments {
        body.extend_from_slice(&u16be(gid.wrapping_sub(*code)));
    }
    body.extend_from_slice(&u16be(1));
    for _ in &segments {
        body.extend_from_slice(&u16be(0));
    }
    body.extend_from_slice(&u16be(0));
    // The `cmap` *table* wraps its subtables: version + numTables + one encoding record
    // (platform 3 = Windows, encoding 1 = Unicode BMP) whose offset points at the
    // format-4 subtable that follows. Without this header the subtable is unreadable.
    let subtable = {
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&u16be(4));
        cmap.extend_from_slice(&u16be((6 + body.len()) as u16));
        cmap.extend_from_slice(&u16be(0));
        cmap.extend_from_slice(&body);
        cmap
    };
    let mut cmap = Vec::new();
    cmap.extend_from_slice(&u16be(0)); // version
    cmap.extend_from_slice(&u16be(1)); // numTables
    cmap.extend_from_slice(&u16be(3)); // platformID = Windows
    cmap.extend_from_slice(&u16be(1)); // encodingID = Unicode BMP
    cmap.extend_from_slice(&u32be(12)); // offset to the subtable
    cmap.extend_from_slice(&subtable);

    let mut head = Vec::new();
    head.extend_from_slice(&u32be(0x0001_0000)); // version
    head.extend_from_slice(&u32be(0x0001_0000)); // fontRevision
    head.extend_from_slice(&u32be(0)); // checkSumAdjustment
    head.extend_from_slice(&u32be(0x5F0F_3CF5)); // magicNumber
    head.extend_from_slice(&u16be(0)); // flags
    head.extend_from_slice(&u16be(1000)); // unitsPerEm
    head.extend_from_slice(&[0; 8]); // created
    head.extend_from_slice(&[0; 8]); // modified
    head.extend_from_slice(&[0, 0, 0, 0]); // xMin/yMin
    head.extend_from_slice(&[0x03, 0xE8, 0x03, 0xE8]); // xMax/yMax
    head.extend_from_slice(&u16be(0)); // macStyle
    head.extend_from_slice(&u16be(8)); // lowestRecPPEM
    head.extend_from_slice(&[0, 2]); // fontDirectionHint
    head.extend_from_slice(&u16be(1)); // indexToLocFormat = long
    head.extend_from_slice(&[0, 0]); // glyphDataFormat

    let mut hhea = Vec::new();
    hhea.extend_from_slice(&u32be(0x0001_0000));
    hhea.extend_from_slice(&800i16.to_be_bytes());
    hhea.extend_from_slice(&(-200i16).to_be_bytes());
    hhea.extend_from_slice(&0i16.to_be_bytes());
    hhea.extend_from_slice(&u16be(1000)); // advanceWidthMax
                                          // ttf-parser skips 24 bytes after lineGap (advanceWidthMax + the rest of the fixed
                                          // fields) before reading numberOfHMetrics; hhea is 36 bytes total.
    hhea.extend_from_slice(&[0; 22]);
    hhea.extend_from_slice(&u16be(num_glyphs));

    let mut maxp = Vec::new();
    maxp.extend_from_slice(&u32be(0x0001_0000));
    maxp.extend_from_slice(&u16be(num_glyphs));

    let hmtx: Vec<u8> = (0..num_glyphs)
        .flat_map(|_| {
            let mut entry = advance_width.to_be_bytes().to_vec();
            entry.extend_from_slice(&[0x00, 0x00]); // lsb 0
            entry
        })
        .collect();
    let loca: Vec<u8> = (0..=num_glyphs).flat_map(|_| u32be(0)).collect();

    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"cmap", cmap),
        (b"glyf", Vec::new()),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    tables.sort_by(|a, b| a.0.cmp(b.0));

    let num_tables = tables.len() as u16;
    let entry_selector = 15 - num_tables.leading_zeros() as u16;
    let search_range = 16 * (1u16 << entry_selector);
    let mut out = Vec::new();
    out.extend_from_slice(&u32be(0x0001_0000));
    out.extend_from_slice(&u16be(num_tables));
    out.extend_from_slice(&u16be(search_range));
    out.extend_from_slice(&u16be(entry_selector));
    out.extend_from_slice(&u16be(num_tables * 16 - search_range));

    let mut offset = 12 + tables.len() * 16;
    let mut records = Vec::new();
    let mut blobs = Vec::new();
    for (tag, data) in &tables {
        records.extend_from_slice(*tag);
        records.extend_from_slice(&u32be(0));
        records.extend_from_slice(&u32be(offset as u32));
        records.extend_from_slice(&u32be(data.len() as u32));
        let padded = (4 - data.len() % 4) % 4;
        blobs.extend_from_slice(data);
        blobs.extend_from_slice(&vec![0; padded]);
        offset += data.len() + padded;
    }
    out.extend_from_slice(&records);
    out.extend_from_slice(&blobs);
    out
}

/// A one-page document whose `/F1` is an embedded-subset TrueType font.
///
/// The font dictionary is `/Subtype /TrueType`, `/BaseFont /ABCDEF+FixtureSubset` (a real
/// subset prefix), `/Encoding /WinAnsiEncoding`, and a `/FontDescriptor /FontFile2` stream
/// carrying a real program that maps exactly `program_glyphs`. `run_text` is the run on
/// the page. This is what lets T11 be tested against a font that genuinely does — or
/// genuinely does not — contain the glyphs a replacement needs.
pub fn embedded_subset_page(run_text: &str, program_glyphs: &[char]) -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();

    let font_id = embedded_subset_font(&mut document, program_glyphs, 600);

    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let content_id = document.add_object(page_body(single_run(run_text)));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// Adds the embedded-subset `/F1` font dictionary to `document` and returns its object id.
///
/// `advance_width` is written into the program's `hmtx`, so a fixture can choose a face whose
/// glyph advance differs from the `0.5em` per-character approximation (P2.4.2 Phase A).
fn embedded_subset_font(
    document: &mut Document,
    program_glyphs: &[char],
    advance_width: u16,
) -> lopdf::ObjectId {
    embedded_subset_font_with(
        document,
        program_glyphs,
        advance_width,
        false,
        "ABCDEF+FixtureSubset",
    )
}

/// Like [`embedded_subset_font`], but with a **FlateDecode-compressed** `FontFile2` program and a
/// caller-chosen `/BaseFont` (P2.4.4 T24-B).
///
/// This is the Maastricht font's real shape. The program is a genuine TrueType face with uniform
/// `0.6em` advances, and PDFium decompresses and renders it normally — but `substitute`'s
/// `stream_bytes` hands `ttf-parser` the *compressed* bytes, which do not parse, so
/// `FontMetrics::resolve` yields `Nominal` and the run has no in-file metric. That is the whole
/// `0.5em`-approximation class `docs/P2.4.4-tj-gridwalk-DESIGN.md` §1 describes as "A′: no
/// `FontFile*`" — measured precisely, it is a *compressed* program, not an absent one.
fn embedded_subset_font_with(
    document: &mut Document,
    program_glyphs: &[char],
    advance_width: u16,
    compress: bool,
    base_font: &str,
) -> lopdf::ObjectId {
    let program = true_type_program_with_advance(program_glyphs, advance_width);
    let mut stream = Stream::new(dictionary! {}, program);
    if compress {
        stream.compress().expect("a generated program compresses");
    }
    let program_id = document.add_object(stream);

    // Widths are nominal; coverage/metrics tests use the AFM substitute or a fixed
    // measurement, never this array.
    let first: i64 = 32;
    let last: i64 = 255;
    let widths: Vec<Object> = (first..=last)
        .map(|_| Object::Integer(i64::from(advance_width)))
        .collect();

    let descriptor_id = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => base_font,
        "Flags" => 32,
        "FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 800.into()],
        "ItalicAngle" => 0,
        "Ascent" => 800,
        "Descent" => -200,
        "CapHeight" => 700,
        "StemV" => 80,
        "FontFile2" => program_id,
    });

    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => base_font,
        "Encoding" => "WinAnsiEncoding",
        "FirstChar" => first,
        "LastChar" => last,
        "Widths" => widths,
        "FontDescriptor" => descriptor_id,
    })
}

// --- P2.4.2 Phase A: chain-break and space-kern fixtures (§3) -----------------

/// Emits one `TJ` line whose fragments are separate strings, with a kerning number after each.
///
/// A single `TJ` array is deliberate: PDFium joins the operands of one text object into **one**
/// box, so a line whose producer used a `TJ` per kerning group is exactly one PDFium box while
/// lopdf sees one operand per fragment. That is the shape both the `Sci|ence` orphan and the
/// space-kern gap need — the orphan's only plausible box is the whole line, which is the bug.
///
/// Each tuple is a fragment's text and the kerning number (thousandths of an em, positive shifts
/// the pen *left*) that follows it; the last fragment's number is ignored.
fn push_tj_array(
    operations: &mut Vec<Operation>,
    size: i64,
    x: i64,
    y: i64,
    parts: &[(&str, i64)],
) {
    operations.push(Operation::new("BT", vec![]));
    operations.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    operations.push(Operation::new(
        "Tm",
        vec![1.into(), 0.into(), 0.into(), 1.into(), x.into(), y.into()],
    ));
    let mut array = Vec::new();
    for (index, (text, kern)) in parts.iter().enumerate() {
        array.push(Object::string_literal(*text));
        if index + 1 < parts.len() {
            array.push(Object::Integer(*kern));
        }
    }
    operations.push(Operation::new("TJ", vec![Object::Array(array)]));
    operations.push(Operation::new("ET", vec![]));
}

/// §3 `chain-break-orphan.pdf`: a line whose tail fragment sits exactly where the head's
/// **exact** glyph advances end, so only the 0.4.1 `0.5em` approximation could sever it.
///
/// The embedded face's advance is `1000/1000 = 1.0em` per glyph, while the 0.4.1 approximation
/// assumed `0.5em`. `Sci` therefore measures `3.0em` exactly and `ence` begins at `x0 + 3.0em` —
/// a zero gap in *file* metrics. The old code computed `x0 + 1.5em` for the head, giving the tail
/// a gap of `1.5em` (18pt at 12pt) that no `0.25em` bound could cross, severing `Sci|ence`. With
/// in-file metrics the gap is zero and the line is one run again (P2.4.2 Phase A). All glyphs are
/// present in the program, so the font gates pass and the picker offers the word.
pub fn chain_break_orphan() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let glyphs: Vec<char> = "Science".chars().collect();
    let font_id = embedded_subset_font(&mut document, &glyphs, 1000);
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    // 12pt face, 1.0em advance -> each glyph is 12pt wide. `Sci` is 36pt, so `ence` starts at
    // 108pt with a zero seam.
    let mut operations = Vec::new();
    push_tj_array(&mut operations, 12, 72, 700, &[("Sci", 0), ("ence", 0)]);

    let content_id = document.add_object(page_body(operations));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// §3 `space-kern-gap.pdf`: an inter-word gap carried as **kerning**, not a space glyph.
///
/// `foo` and `bar` are two strings in one `TJ` array with no `0x20` between them; the kern widens
/// the seam to `0.4em` (4.8pt at 12pt) — past the `0.25em` bound, inside the `0.5em` one. PDFium
/// reconstructs the line as `foo bar`, so the widened gap applies and the two merge (P2.4.2
/// O-Chain-2). The companion case — the same `0.4em` seam with **no** space in the reconstructed
/// text — is asserted in the unit test `the_widened_gap_needs_a_real_space_at_the_seam`.
pub fn space_kern_gap() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let glyphs: Vec<char> = "fobar".chars().collect();
    // 0.6em advances: `foo` = 21.6pt at 12pt. A -400 kern adds 4.8pt before `bar`.
    let font_id = embedded_subset_font(&mut document, &glyphs, 600);
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut operations = Vec::new();
    push_tj_array(&mut operations, 12, 72, 700, &[("foo", -400), ("bar", 0)]);

    let content_id = document.add_object(page_body(operations));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// A font dictionary with **no embedded program at all** and a non-standard-14 base name —
/// the class `docs/P2.4.3-FINDINGS.md` records as "A′": `FontMetrics::resolve` finds no
/// `FontFile*` to read, the name is not in the AFM table, so it returns `Nominal` and the run
/// has no measurable in-file span.
fn unmeasurable_font_face() -> Dictionary {
    dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "ABCDEE+Monaco",
        "Encoding" => "WinAnsiEncoding",
        "FirstChar" => 32,
        "LastChar" => 255,
        // Nominal widths, so a producer would still lay the run out; the *program* these would
        // belong to is simply absent, which is what makes the run unmeasurable in-file.
        "Widths" => (32..=255).map(|_| Object::Integer(600)).collect::<Vec<Object>>(),
        "FontDescriptor" => dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "ABCDEE+Monaco",
            "Flags" => 4,
            "FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 800.into()],
            "ItalicAngle" => 0,
            "Ascent" => 800,
            "Descent" => -200,
            "CapHeight" => 700,
            "StemV" => 80,
        },
    }
}

/// §3 `unmeasurable-font.pdf`: a run whose font carries **no in-file metrics**, on one line,
/// inside one box (P2.4.3 T23).
///
/// Before T23 this is the Maastricht field report in miniature: the font class yields no
/// metric, so the width gate had nothing to measure a replacement against and refused the run
/// outright ("font carries no width metrics"), locking the page. After T23 the run's span comes
/// from PDFium's **character grid**, so the same run is editable and a wider replacement is
/// refused *quoting the rendered span*, which is the honest number.
pub fn unmeasurable_font_page() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(unmeasurable_font_face());
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    // One `Tj` at 12pt: PDFium renders it as one box whose glyphs the char grid can walk.
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new("Tj", vec![Object::string_literal("Data Science")]),
        Operation::new("ET", vec![]),
    ];

    let content_id = document.add_object(page_body(operations));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// §3 `unmeasurable-chain-break.pdf`: the Maastricht line-1 shape — a merge chain that the
/// **approximation** breaks — in a font with **no in-file metrics**.
///
/// The head and the tail are two separate `Tj` operators, so the tail's origin comes from its
/// own `Td` (authoritative) rather than from a walk of the head's width. Placing that `Td` at the
/// head's **rendered** end (`72 + 7 × 0.6em × 12pt = 122.4pt`) makes the true seam zero:
///
/// * **char-grid (T23b)** — `next.x0 − head.x1 = 0pt`: the chain is whole, the line is one run,
///   and its quota is the grid span `72..158.4 = 86.4pt`.
/// * **the approximation** — the font has no metric, so `fragment_advance` charges `0.5em` per
///   character: the head measures `42pt` instead of `50.4pt`, the computed gap becomes `8.4pt`,
///   past the bound, and the chain breaks — leaving `ence` an orphan with a line-sized box,
///   exactly the field report.
///
/// Under 0.4.2 both the break and the missing metric conspired: the orphan refused outright.
/// T23b re-merges the line; T23c gives the single run a char-grid quota, so a short replacement
/// fits and an over-wide one hard-refuses quoting both widths (O1).
pub fn unmeasurable_chain_break() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(unmeasurable_font_face());
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    // Each glyph is 0.6em = 7.2pt at 12pt, so `DataSci` (7 glyphs) is 50.4pt wide and the tail
    // starts at exactly 122.4pt. Two `Tj` operators, so PDFium reports one box per operator.
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new("Tj", vec![Object::string_literal("DataSci")]),
        Operation::new("ET", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
        Operation::new("Td", vec![Object::Real(122.4), 700.into()]),
        Operation::new("Tj", vec![Object::string_literal("ence")]),
        Operation::new("ET", vec![]),
    ];

    let content_id = document.add_object(page_body(operations));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// §3 `wider-replacement-orphan.pdf`: the user's exact 0.4.1 failure, in one fixture.
///
/// A `TJ` line `DataScien|ce` whose seam is wide enough (`0.6em`) to break the chain leaves the
/// tail `ce` an orphan — with no box of its own, so it pairs to the **whole line's** box, whose
/// width is several times the tail's own ~24pt span. Under 0.4.1 the width gate took the *box*
/// width as the old width, so a ~110pt replacement to that 14pt-class tail passed the gate and
/// was written into the tail operand alone. This fixture exists so the regression is caught: the
/// tail must be *box-suspicious* (flagged weak, never merged) and its quota must be its own
/// measured span, so the same ~110pt replacement **hard-refuses** quoting both widths (T22).
pub fn wider_replacement_orphan() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let glyphs: Vec<char> = "DataScience".chars().collect();
    // 1.0em advances at 12pt: the head `DataScien` is 108pt, the tail `ce` is 24pt.
    let font_id = embedded_subset_font(&mut document, &glyphs, 1000);
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    // A -600 kern adds a 7.2pt (0.6em) seam, past even the widened 0.5em bound, so the tail is a
    // chain-break orphan while the whole line is one PDFium box.
    let mut operations = Vec::new();
    push_tj_array(
        &mut operations,
        12,
        72,
        700,
        &[("DataScien", -600), ("ce", 0)],
    );

    let content_id = document.add_object(page_body(operations));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// §3 `tj-grid-walk.pdf` (T24-B): the Maastricht line-1 shape as a **synthetic** regression.
///
/// Twenty-two `TJ` operands spell `"5 month University level Data Science "` in a
/// **metrics-less** subset font (class A′: no `/FontFile*`, a non-standard-14 name) whose
/// glyphs really advance `0.6em`. The lopdf walk has no metric to read, so
/// [`super::textlocate::fragment_advance`] charges its `0.5em` approximation and every
/// fragment's computed origin drifts further left of the rendered page as the line runs on —
/// by `ence ` the error is ~0.1 em × 38 chars ≈ 6pt. That drift is what T23a's origin-anchored
/// walk tripped over: a long operand matched the wrong grid characters, the seam gaps came out
/// garbage, and the chain broke into 22 singletons.
///
/// T24 re-derives each fragment's span from the characters *it* occupies inside the object's own
/// char window, by character alignment — a quantity with no pen in it — so the drift is gone and
/// the line is one merged run whose quota is the true rendered span (38 glyphs × 0.6em at 15pt
/// = 342pt). The fixture is platform-independent: it reproduces by construction (wide glyphs in
/// a metrics-less font), not by an x86/arm difference.
pub fn tj_grid_walk() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    // A genuine 0.6em face with a **FlateDecode-compressed** `/FontFile2` and the Maastricht
    // `/BaseFont` name. PDFium renders the program (uniform 9pt loose boxes at 15pt), while
    // `FontMetrics::resolve` reads no metric — the exact A′ shape the drift needs.
    let font_id = embedded_subset_font_with(
        &mut document,
        &"5 monthUniversitylevelDataScience"
            .chars()
            .collect::<Vec<_>>(),
        600,
        true,
        "ABCDEE+Monaco",
    );
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut operations = Vec::new();
    // The real line's 22 operands, kerning included, so the drift accumulates exactly as it does
    // in the field document.
    push_tj_array(
        &mut operations,
        15,
        46,
        700,
        &[
            ("5", -7),
            (" ", 8),
            ("m", -7),
            ("o", -7),
            ("n", -7),
            ("th ", 24),
            ("U", -7),
            ("n", -7),
            ("i", -7),
            ("versity ", 24),
            ("l", -7),
            ("e", -7),
            ("v", -7),
            ("el ", 24),
            ("D", -7),
            ("a", -7),
            ("t", -7),
            ("a ", 24),
            ("S", -7),
            ("c", -7),
            ("i", -7),
            ("ence ", 0),
        ],
    );

    let content_id = document.add_object(page_body(operations));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// §3 `tj-kern-seam.pdf` (T24-C): a kern seam wide enough for PDFium to **synthesise** a space.
///
/// Two operands, `Data` and `Sci`, in a metrics-less `0.6em` face, separated by a `-600` kern
/// (a 0.6em seam). PDFium reconstructs that seam as a *generated* space — a grid character that
/// occupies a slot but no operand's bytes — so the object's text reads `"Data Sci"` while the
/// operands read `"Data"` and `"Sci"`.
///
/// T24's character walk must skip that generated character when it hands the operands their
/// characters: assigning it to either fragment would shift the rest of the line by one and
/// mis-measure both. The fixture pins that `Data` keeps exactly its four rendered characters and
/// `Sci` its three, and that the two still merge across the seam.
pub fn tj_kern_seam() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    // The same compressed-program A′ font as T24-B, so PDFium renders a real face while the file
    // offers no metric and synthesises the seam as a space.
    let font_id = embedded_subset_font_with(
        &mut document,
        &"DataSci".chars().collect::<Vec<_>>(),
        600,
        true,
        "ABCDEE+Monaco",
    );
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut operations = Vec::new();
    push_tj_array(&mut operations, 15, 46, 700, &[("Data", -400), ("Sci", 0)]);

    let content_id = document.add_object(page_body(operations));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
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

/// Two adjacent `Tj` operands on one baseline, one style and size: a line split across
/// operations, the shape a selection range exists for (P2.6 T26).
pub fn two_run_line() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal("ALPHA ")]),
            Operation::new("Tj", vec![Object::string_literal("BETA")]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// P2.8 R8b: two tagged units on **one baseline**, the second holding the edit target.
///
/// `reflow_tj_array` takes `line_origin` as the minimum `origin.0` over every run whose
/// `baseline()` matches, and `baseline()` is `origin.1 + rise` — no scope term. So the run in
/// the first unit, lying 128 pt to the left, makes `line_origin` too small and the `Td` offset
/// correspondingly too large: the tail is drawn 128 pt right of where it belongs, and the
/// record still reports the intended position (design R8b, R8b.1).
///
/// Both `BDC`s are `/P`, and the page carries a `/StructTreeRoot` whose `/ParentTree` resolves
/// each `MCID` to its own `/P` element — so the scopes differ by
/// [`crate::editor::textlocate::MergeScope::Block`], the coarse and correct boundary, rather than
/// by the raw-`MCID` fallback. The tag alone would not do: [`super::textlocate::MarkedContent`]
/// records the tag "for diagnostics" and never consults it (T27).
///
/// Geometry, at `Tf 24` in Helvetica: unit 0 draws `HI` at x=72, the target unit draws
/// `[(AB) (CD)]` at x=200, so `CD` sits at 200 + 32.016 = 232.016.
pub fn two_mcid_one_baseline() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(winansi_helvetica());
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content_id = document.add_object(page_body(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new(
            "BDC",
            vec!["P".into(), Object::Dictionary(dictionary! { "MCID" => 0 })],
        ),
        Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), 72.into(), 700.into()],
        ),
        Operation::new("Tj", vec![Object::string_literal("HI")]),
        Operation::new("EMC", vec![]),
        Operation::new(
            "BDC",
            vec!["P".into(), Object::Dictionary(dictionary! { "MCID" => 1 })],
        ),
        Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), 200.into(), 700.into()],
        ),
        Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("AB"),
                Object::string_literal("CD"),
            ])],
        ),
        Operation::new("EMC", vec![]),
        Operation::new("ET", vec![]),
    ]));

    // Two sibling paragraphs, one per tagged unit. A `/P` element is itself block-level, so the
    // `/ParentTree` walk stops on it rather than climbing to a parent.
    let unit_zero = document.add_object(dictionary! {
        "Type" => "StructElem",
        "S" => "P",
    });
    let unit_one = document.add_object(dictionary! {
        "Type" => "StructElem",
        "S" => "P",
    });
    let per_page = document.add_object(Object::Array(vec![
        Object::Reference(unit_zero),
        Object::Reference(unit_one),
    ]));
    let parent_tree = document.add_object(dictionary! {
        "Nums" => vec![0.into(), Object::Reference(per_page)],
    });
    let structure_root = document.add_object(dictionary! {
        "Type" => "StructTreeRoot",
        "ParentTree" => parent_tree,
    });

    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
        "StructParents" => 0,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "StructTreeRoot" => structure_root,
    });
    document.trailer.set("Root", catalog_id);
    document.renumber_objects();
    document
}

/// Two adjacent operands on one line, each inside its own tagged unit (P2.7.1).
///
/// The geometry is identical to [`two_run_line`] -- same font, same size, zero gap -- so
/// nothing but the `MCID` can separate them. A merge here would be the silent wrong write
/// the phase exists to prevent.
pub fn mcid_boundaries() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new(
                "BDC",
                vec!["P".into(), Object::Dictionary(dictionary! { "MCID" => 5 })],
            ),
            Operation::new("Tj", vec![Object::string_literal("ALPHA ")]),
            Operation::new("EMC", vec![]),
            Operation::new(
                "BDC",
                vec!["P".into(), Object::Dictionary(dictionary! { "MCID" => 6 })],
            ),
            Operation::new("Tj", vec![Object::string_literal("BETA")]),
            Operation::new("EMC", vec![]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// Text drawn with a rotated text matrix: `Tm [0 1 -1 0 300 100]` is a 90 degree rotation.
///
/// The run's `advance` is measured along the *text* axis, which here is vertical, so it is not
/// a horizontal width. Before the hotfix this run was editable and `fits-width` compared the
/// advance against a horizontal box (P2.7 hotfix H0).
pub fn rotated_text() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    0.into(),
                    1.into(),
                    (-1).into(),
                    0.into(),
                    300.into(),
                    100.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ROTATED")]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// Text rotated by the **graphics** matrix (P2.7 T-7), not by `Tm`.
///
/// `cm` sets the CTM; the renderer composes `tm x ctm`, so this run is drawn vertically
/// while its own `Tm` is the identity. H0 read only `Tm` and therefore called it editable
/// with a horizontal `advance` — editable *and* mis-measured, the failure H0 exists to
/// remove. `q`/`Q` bracket it so the CTM is a graphics-state property, as in real files.
pub fn rotated_by_graphics_matrix() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    0.into(),
                    1.into(),
                    (-1).into(),
                    0.into(),
                    300.into(),
                    100.into(),
                ],
            ),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    700.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ROTATED")]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
        ]],
    )
}

/// Text under a `cm` that **scales** it, with no rotation at all (P2.7.4).
///
/// `rotated` is false here, so the H0 gate never applies — and yet the run was refused
/// before P2.7.4, because `origin` is text-space and `pair_run` compared it to a page-space
/// box. This fixture is the proof that the uncomposed origin was a *live* defect on
/// untransformed-looking content rather than one latent behind the rotation refusal.
///
/// `factor` also makes the two spaces differ by exactly that factor, which is what catches a
/// width gate that mixes text-space widths against page-space boxes.
pub fn scaled_by_graphics_matrix(factor: f32) -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    factor.into(),
                    0.into(),
                    0.into(),
                    factor.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    700.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ROTATED")]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
        ]],
    )
}

/// Text mirrored by an `x`-flip `cm`: not rotated, but not readable left-to-right either
/// (P2.7.4 Q5). Kept as a fixture so the refusal can name what it is.
pub fn mirrored_by_graphics_matrix() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    (-1).into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    600.into(),
                    0.into(),
                ],
            ),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    700.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ROTATED")]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
        ]],
    )
}

/// Builds the PDFlib page idiom as a **Form**: one `cm` that flips the vertical axis, text
/// drawn inside the form at a caller-chosen `Tz` and `Tf` size.
///
/// This is the shape a real producer emits and none of the existing fixtures had: a page whose
/// only content is a form, with `1 0 0 -1 0 841 cm` at the top of the form and *negative*
/// `Tz` and `Tf` inside it. The two negatives cancel; the text renders upright, and the
/// handedness test must not call it mirrored.
fn flipped_form_page(tz: f32, size: f32) -> Document {
    xobject_document(
        vec![(
            "X1".into(),
            vec![
                Operation::new(
                    "cm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        (-1).into(),
                        0.into(),
                        792.into(),
                    ],
                ),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), size.into()]),
                Operation::new("Tz", vec![tz.into()]),
                Operation::new("Td", vec![72.into(), 100.into()]),
                Operation::new("Tj", vec![Object::string_literal("UPRIGHT")]),
                Operation::new("ET", vec![]),
            ],
            FormResources::Own(Vec::new()),
        )],
        vec![draw_form("X1")],
    )
}

/// The flip idiom as PDFlib writes it: `cm [1 0 0 -1 0 841]` with `Tz -100`.
///
/// The glyphs are drawn through `det(tm x ctm) * (Tz/100) = (-1) * (-1) = +1`, so the text is
/// upright. Measured against poppler: the same flip with `Tz +100` renders reflected.
pub fn pdflib_flip_with_negative_tz() -> Document {
    flipped_form_page(-100.0, -12.0)
}

/// The same flip with a positive `Tz`: genuinely reflected, and must still refuse.
pub fn flip_with_positive_tz() -> Document {
    flipped_form_page(100.0, 12.0)
}

/// The same flip with `Tz 0`: no advance at all, so no handedness and no width.
pub fn flip_with_zero_tz() -> Document {
    flipped_form_page(0.0, 12.0)
}

/// A 180-degree rotation: `det = +1`, `Tz +100`. Not mirrored — the glyphs are upside down
/// but read consistently along their own advance axis. Distinguished from a flip empirically.
pub fn upright_upside_down_by_rotation() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    (-1).into(),
                    0.into(),
                    0.into(),
                    (-1).into(),
                    600.into(),
                    792.into(),
                ],
            ),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    700.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("TURNED")]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
        ]],
    )
}

/// The control for [`rotated_by_graphics_matrix`]: the same `q`/`cm`/`Q` scaffolding with a
/// pure translation, which moves the run without rotating it. Detection must not refuse
/// text merely because a `cm` was present.
pub fn translated_by_graphics_matrix() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    100.into(),
                    50.into(),
                ],
            ),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    700.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ROTATED")]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
        ]],
    )
}

/// A rotated `cm` restored by `Q` before the text is shown (P2.7 T-7): the stack must put
/// the identity back, so this run is axis-aligned and editable. A detection that ignored
/// `Q` would refuse it.
pub fn rotation_restored_before_showing() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    0.into(),
                    1.into(),
                    (-1).into(),
                    0.into(),
                    300.into(),
                    100.into(),
                ],
            ),
            Operation::new("Q", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    700.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ROTATED")]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// The same text, same font, same size, with an axis-aligned matrix: the control for
/// [`rotated_text`], proving the refusal is caused by the rotation and nothing else.
pub fn unrotated_text() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    300.into(),
                    100.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ROTATED")]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// `Ts` between two showings (P2.7.1 B-5): the first operand sits on the pen baseline, the
/// second is raised 5pt without the pen moving.
///
/// The rise must be recorded on the run and must **not** be folded into the pen, so the two
/// runs share an origin y but not an ink baseline.
pub fn text_rise() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal("BASE ")]),
            Operation::new("Ts", vec![5.into()]),
            Operation::new("Tj", vec![Object::string_literal("HIGH")]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// One operand wrapped in `/Span <</ActualText …>>` (P2.7.1 T30), UTF-16BE with its BOM.
pub fn actualtext_span() -> Document {
    let actual: Vec<u8> = vec![0xFE, 0xFF]
        .into_iter()
        .chain("ALPHA".encode_utf16().flat_map(|unit| unit.to_be_bytes()))
        .collect();
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new(
                "BDC",
                vec![
                    "Span".into(),
                    Object::Dictionary(dictionary! {
                        "ActualText" => Object::String(actual, lopdf::StringFormat::Literal),
                    }),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
            Operation::new("EMC", vec![]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// Two `Tj` operands inside **one** inline `/Span <</ActualText…>>` (P2.7.1 T30). The span's
/// `ActualText` describes both, so rewriting it for one of them would change text outside the
/// edit: the write must disclose instead.
pub fn actualtext_shared_inline() -> Document {
    let actual: Vec<u8> = vec![0xFE, 0xFF]
        .into_iter()
        .chain(
            "ALPHA BETA"
                .encode_utf16()
                .flat_map(|unit| unit.to_be_bytes()),
        )
        .collect();
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new(
                "BDC",
                vec![
                    "Span".into(),
                    Object::Dictionary(dictionary! {
                        "ActualText" => Object::String(actual, lopdf::StringFormat::Literal),
                    }),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
            Operation::new("Td", vec![60.into(), 0.into()]),
            Operation::new("Tj", vec![Object::string_literal("BETA")]),
            Operation::new("EMC", vec![]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// `ActualText` in **PDFDocEncoding** (no `FE FF` BOM) carrying the bytes where that encoding
/// departs from Latin-1: `0x80` is a bullet and `0xA0` a euro sign (P2.7.1 T-2).
pub fn actualtext_pdf_doc_encoding() -> Document {
    let actual: Vec<u8> = b"ALPHA \x80 5\xA0".to_vec();
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new(
                "BDC",
                vec![
                    "Span".into(),
                    Object::Dictionary(dictionary! {
                        "ActualText" => Object::String(actual, lopdf::StringFormat::Hexadecimal),
                    }),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
            Operation::new("EMC", vec![]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// `ActualText` whose last byte (`0xAD`, the soft hyphen) is **undefined** in PDFDocEncoding
/// table D.2, so it decodes to no character at all (P2.7.1 T-2).
pub fn actualtext_undecodable() -> Document {
    let actual: Vec<u8> = b"ALPHA\xAD".to_vec();
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new(
                "BDC",
                vec![
                    "Span".into(),
                    Object::Dictionary(dictionary! {
                        "ActualText" => Object::String(actual, lopdf::StringFormat::Hexadecimal),
                    }),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
            Operation::new("EMC", vec![]),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// A named `/Properties` resource: the `ActualText` a write must not rewrite in place,
/// because another sequence may share it (P2.7.1 T30).
pub fn actualtext_named_property() -> Document {
    let actual: Vec<u8> = vec![0xFE, 0xFF]
        .into_iter()
        .chain("ALPHA".encode_utf16().flat_map(|unit| unit.to_be_bytes()))
        .collect();
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(winansi_helvetica());
    let properties_id = document.add_object(dictionary! {
        "P0" => dictionary! {
            "ActualText" => Object::String(actual, lopdf::StringFormat::Literal),
        },
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "Properties" => properties_id,
    });
    let content_id = document.add_object(page_body(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new("BDC", vec!["Span".into(), "P0".into()]),
        Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
        Operation::new("EMC", vec![]),
        Operation::new("ET", vec![]),
    ]));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog_id);
    document
}

/// Two sequences naming **one** `/Properties` entry that carries `ActualText`: the case
/// ruling B-7 sends through copy-on-write. Editing the first must leave the second's
/// `ActualText` byte-identical, which is only possible if the edit clones the entry
/// instead of mutating the shared dictionary.
pub fn named_property_shared_actualtext() -> Document {
    let actual: Vec<u8> = vec![0xFE, 0xFF]
        .into_iter()
        .chain("ALPHA".encode_utf16().flat_map(|unit| unit.to_be_bytes()))
        .collect();
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(winansi_helvetica());
    let properties_id = document.add_object(dictionary! {
        "P0" => dictionary! {
            "ActualText" => Object::String(actual, lopdf::StringFormat::Literal),
        },
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "Properties" => properties_id,
    });
    let content_id = document.add_object(page_body(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new("BDC", vec!["Span".into(), "P0".into()]),
        Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
        Operation::new("EMC", vec![]),
        // A second line far enough below to be its own run, naming the same entry.
        Operation::new("Td", vec![0.into(), (-40).into()]),
        Operation::new("BDC", vec!["Span".into(), "P0".into()]),
        Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
        Operation::new("EMC", vec![]),
        Operation::new("ET", vec![]),
    ]));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog_id);
    document
}

/// Two runs in one `TJ` array with `Tc 5`: the pen advance must include the character
/// spacing, or the second fragment appears 15pt to the left of where it is drawn (P2.7.1).
pub fn char_spacing_tj() -> Document {
    document_with_pages(
        winansi_helvetica(),
        vec![vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Tc", vec![5.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new(
                "TJ",
                vec![Object::Array(vec![
                    Object::string_literal("AB"),
                    Object::string_literal("CD"),
                ])],
            ),
            Operation::new("ET", vec![]),
        ]],
    )
}

/// The kern, in thousandths of an em, between `AB` and `CD` on the fixtures below.
///
/// A `TJ` number shifts the pen by `-number/1000 x Tf` in text space (ISO 32000-1 9.4.3), so a
/// positive value moves the pen *left*, pulling the second string back into the first. `-250`
/// therefore *widens* the gap by `0.25 em` — a displacement large enough to be unmistakable in
/// ink, and wrong by a visible amount on whichever side forgets it.
pub const KERNED_LINE_KERN: i64 = -250;

/// The horizontal scaling `kerned-line-tz.pdf` sets, in percent.
///
/// Deliberately not 100. Every other fixture in the tree is at `Tz 100`, where the kern term's
/// `Tz/100` factor is arithmetically invisible: `x / 100 * 100 == x`. At `Tz 50` the factor is
/// 0.5, so a walk that omits it places the second string exactly twice as far from the first as
/// the renderer drew it.
pub const KERNED_LINE_TZ: i64 = 50;

/// The kerned line: one `Td`, one `TJ`, two strings with a kerning number between them, over an
/// unedited control line.
///
/// This is P2.8's A10 fixture and the first test of the pen walk's kern term. Earlier `TJ`
/// fixtures put two strings side by side with **no** number between them — `char_spacing_tj`
/// carries `Tc 5` and two adjacent strings — so the kern branch of the walk had never been
/// exercised by a committed artefact.
///
/// Line 1 is a single `TJ` array with **one** `Td` before it, so moving `CD` requires *inserting*
/// a positioning operator rather than editing an existing one: the case
/// `P2.8-reflow-DESIGN.md` §2.2 exists to express.
///
/// Both lines share the font and size, so a comparison between them cannot be excused by a
/// difference in metrics.
pub fn kerned_line() -> Document {
    document_with_pages(winansi_helvetica(), vec![kerned_line_page(0)])
}

/// The same content at a horizontal scaling that is not 100, so the kern term's `Tz` factor is
/// falsifiable. See [`KERNED_LINE_TZ`].
pub fn kerned_line_tz() -> Document {
    document_with_pages(winansi_helvetica(), vec![kerned_line_page(KERNED_LINE_TZ)])
}

/// The shared page body. `tz == 0` means the page does not set `Tz` at all, which is why this
/// takes a value rather than always emitting `Tz 100`: the un-`Tz`'d file is the baseline a
/// reviewer diffs the other against, and `Tz 100` is not the same stream as no `Tz`.
fn kerned_line_page(tz: i64) -> Vec<Operation> {
    let mut operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
    ];
    if tz != 0 {
        operations.push(Operation::new("Tz", vec![tz.into()]));
    }
    operations.extend([
        // Line 1, the kerned line: `AB`, a `-250` kern, `CD`.
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("AB"),
                Object::Integer(KERNED_LINE_KERN),
                Object::string_literal("CD"),
            ])],
        ),
        // Line 2, the unedited control. Positioned by its own `Tm`, so it does not inherit any
        // pen state line 1 left behind and must sit still whatever a reflow does to line 1.
        Operation::new(
            "Tm",
            vec![
                1.into(),
                0.into(),
                0.into(),
                1.into(),
                72.into(),
                600.into(),
            ],
        ),
        Operation::new(
            "TJ",
            vec![Object::Array(vec![Object::string_literal("WXYZ")])],
        ),
        Operation::new("ET", vec![]),
    ]);
    operations
}

/// A `BDC` whose properties are **named** but carry only an `MCID`: no `ActualText` at all.
/// This is the overwhelmingly common InDesign shape, and the write path must not report a
/// divergence for it (P2.7.1 T30).
pub fn named_property_without_actualtext() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(winansi_helvetica());
    let properties_id = document.add_object(dictionary! {
        "P0" => dictionary! { "MCID" => 0 },
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "Properties" => properties_id,
    });
    let content_id = document.add_object(page_body(vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new("BDC", vec!["Span".into(), "P0".into()]),
        Operation::new("Tj", vec![Object::string_literal("ALPHA")]),
        Operation::new("EMC", vec![]),
        Operation::new("ET", vec![]),
    ]));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document
}

/// A three-page document whose `/Resources` are **inherited and indirect**, the shape real
/// producers write: the `/Pages` node carries the resource dictionary, page 1 shadows it
/// with an inline `/Resources` of its own, and pages 2 and 3 inherit it (P2.7.1 T-3b).
///
/// The shadowing page resolves the same resource *name* (`F1`) to a different font, so the
/// base names (`ABCDEF+Shadowed` / `ABCDEF+Inherited`) prove which dictionary a reader
/// consulted. The two inheriting pages share one dictionary object, which is what makes an
/// in-place write to it observable on the page that was not edited.
///
/// Every font is subset-flagged with no program, so any page substitutes `Helvetica`.
pub fn indirect_resources_inherited() -> Document {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let inherited_font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "ABCDEF+Inherited",
        "Encoding" => "WinAnsiEncoding",
    });
    let inherited_resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => inherited_font_id },
    });
    let shadowed_font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "ABCDEF+Shadowed",
        "Encoding" => "WinAnsiEncoding",
    });

    let shadowed_content = document.add_object(page_body(single_run("SUBSET")));
    let shadowed_page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => shadowed_content,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => shadowed_font_id } },
    });

    let mut inherited_pages = Vec::new();
    for text in ["ALPHA", "BRAVO"] {
        let content = document.add_object(page_body(single_run(text)));
        inherited_pages.push(Object::Reference(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content,
        })));
    }

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => std::iter::once(Object::Reference(shadowed_page_id))
                .chain(inherited_pages)
                .collect::<Vec<_>>(),
            "Count" => 3,
            "Resources" => inherited_resources_id,
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

// --- P2.7.7: the glyph-id preservation fixture (§5 row 6) ---------------------

/// One `Type0` font resource, drawn by three runs, so an edit to one can be checked for
/// collateral damage to the others (§5 row 6).
///
/// **The program is a real subset, built with `embed::subset` from the bundled face.** Three
/// earlier drafts of this fixture were wrong, and each was wrong in a way that made row 6
/// assert nothing:
///
/// 1. `compressed_program_cid_page`'s program comes from `true_type_program_with_advance`,
///    whose `glyf` is empty and `loca` all zeros. Every glyph draws nothing, so a pixel
///    comparison of two blank regions passes whatever the writer did.
/// 2. Embedding the *whole* `LiberationSans-Regular.ttf` draws real ink, but its `cmap` maps
///    every character — including the one an edit substitutes. `substitute::original_covers`
///    therefore returns true, the edit keeps the original font, and no new program is ever
///    written. Row 6 would be green and would have tested nothing.
/// 3. Hand-picking glyph ids into a synthetic program would duplicate the very writer under
///    test, so a fixture bug and a writer bug would look identical.
///
/// A real subset avoids all three: real outlines, real `hmtx`, and — being a subset — it maps
/// only the characters the original document used, so anything else genuinely requires a new
/// program. That is the situation row 6 exists for.
///
/// **`/CIDToGIDMap /Identity` is honest here** because `embed::subset` renumbers glyphs in
/// sorted source order (`.notdef` = 0, then `A`=1, `B`=2, `C`=3, `a`=4, `b`=5), and the CIDs
/// below are exactly those ids. A reader following `/Identity` reaches the right glyph.
///
/// The middle run is the edit target. The runs above and below are what the edit must not
/// disturb; they sit far apart so their bounding boxes cannot overlap the edit's.
/// The P2.7.7 §5 row-6 fixture, from an already-built subset program.
///
/// Takes the program rather than building it **because this file is compiled twice**: normally
/// as `editor::testpdf`, and again standalone by `examples/make_p24_fixtures.rs` and
/// `examples/make_p277_fixtures.rs` through `#[path]`, where `crate::` names the example crate.
/// A `crate::editor::embed` call here would therefore break both generators. Nothing in this
/// module may reach outside `lopdf` and `ttf-parser`.
pub fn type0_page_with_subset(
    program_bytes: &[u8],
    base_font: &str,
    bbox: [i16; 4],
    ascent: i16,
    descent: i16,
    cap_height: i16,
) -> Document {
    // The subset ids: `.notdef`=0, then sorted source ids 36,37,38,68,69 -> 1..5.
    const CID_A: u16 = 1;
    const CID_B: u16 = 2;
    const CID_C: u16 = 3;
    const CID_A_LOWER: u16 = 4;
    const CID_B_LOWER: u16 = 5;

    let parsed = ttf_parser::Face::parse(program_bytes, 0).expect("the subset parses");
    let upem = parsed.units_per_em();
    let width = |gid: u16| -> i64 {
        let advance = parsed
            .glyph_hor_advance(ttf_parser::GlyphId(gid))
            .unwrap_or(0) as i64;
        // Thousandths of an em, which is what a PDF width means. `hmtx` is in 2048ths.
        (advance * 1000 + upem as i64 / 2) / upem as i64
    };
    let adv = |cid: u16| -> i64 { width(cid) };

    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();

    // A `/ToUnicode` covering **every** CID the page draws, which is what a real subset does:
    // a producer writes the map for the glyphs it embedded, not for a subset of them.
    //
    // An earlier version mapped only the edited run's two CIDs. That looked plausible and was
    // wrong twice over: the untouched runs then decoded to `U+FFFD`, so their advances fell
    // back to a 0.5em default and the bounding boxes computed from them clipped the real ink —
    // which would have failed row 6a for a fixture reason that reads like a writer bug. The
    // substitution still needs a *new* program because the replacement (`Ω`) is in neither this
    // map nor the subset's `cmap`, which is the point of the row.
    let cmap = format!(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\nbegincmap\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
         5 beginbfchar\n\
         <{CID_A:04X}> <0041>\n<{CID_B:04X}> <0042>\n<{CID_C:04X}> <0043>\n\
         <{CID_A_LOWER:04X}> <0061>\n<{CID_B_LOWER:04X}> <0062>\n\
         endbfchar\n\
         endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n"
    );
    let mut cmap_stream = Stream::new(dictionary! {}, cmap.into_bytes());
    cmap_stream.compress().unwrap();
    let cmap_id = document.add_object(cmap_stream);

    let mut program = Stream::new(dictionary! {}, program_bytes.to_vec());
    program.compress().unwrap();
    let program_id = document.add_object(program);

    let descriptor_id = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => Object::string_literal(base_font.to_string()),
        "Flags" => 32,
        // `bbox` is in font units; a PDF descriptor wants thousandths of an em.
        "FontBBox" => vec![
            Object::Integer((bbox[0] as i64 * 1000) / upem as i64),
            Object::Integer((bbox[1] as i64 * 1000) / upem as i64),
            Object::Integer((bbox[2] as i64 * 1000) / upem as i64),
            Object::Integer((bbox[3] as i64 * 1000) / upem as i64),
        ],
        "ItalicAngle" => 0,
        "Ascent" => (ascent as i64 * 1000) / upem as i64,
        "Descent" => (descent as i64 * 1000) / upem as i64,
        "CapHeight" => (cap_height as i64 * 1000) / upem as i64,
        "StemV" => 80,
        "FontFile2" => program_id,
    });

    let descendant_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => Object::string_literal(base_font.to_string()),
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "CIDToGIDMap" => "Identity",
        "DW" => adv(CID_A_LOWER),
        "W" => vec![
            Object::Integer(CID_A as i64),
            vec![
                Object::Integer(adv(CID_A)),
                Object::Integer(adv(CID_B)),
                Object::Integer(adv(CID_C)),
            ]
            .into(),
            Object::Integer(CID_A_LOWER as i64),
            vec![
                Object::Integer(adv(CID_A_LOWER)),
                Object::Integer(adv(CID_B_LOWER)),
            ]
            .into(),
        ],
        "FontDescriptor" => descriptor_id,
    });

    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => Object::string_literal(base_font.to_string()),
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(descendant_id)],
        "ToUnicode" => cmap_id,
    });

    // ONE font resource, named once. All three runs resolve through it, so a writer that
    // renumbers the program corrupts the runs it did not touch.
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let hex = |codes: &[u16]| -> Object {
        Object::String(
            codes.iter().flat_map(|cid| cid.to_be_bytes()).collect(),
            StringFormat::Hexadecimal,
        )
    };

    let content_id = document.add_object(page_body(vec![
        // Untouched run 1, near the top. Draws `C` (CID 3).
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new("Tj", vec![hex(&[CID_C])]),
        Operation::new("ET", vec![]),
        // The edit target. Draws `AB` (CIDs 1, 2).
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 500.into()]),
        Operation::new("Tj", vec![hex(&[CID_A, CID_B])]),
        Operation::new("ET", vec![]),
        // Untouched run 2, near the bottom. Draws `ab` (CIDs 4, 5) — the last two characters a
        // first-use renumbering would meet, and therefore the two that move furthest.
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 300.into()]),
        Operation::new("Tj", vec![hex(&[CID_A_LOWER, CID_B_LOWER])]),
        Operation::new("ET", vec![]),
    ]));

    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources_id,
    });

    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
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
