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
use lopdf::{dictionary, Dictionary, Document, Object, Stream};

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
                    vec![Object::string_literal("Document Reader P1 fixture, page one.")],
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
    page_with_run(
        "SUBSET",
        dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "ABCDEF+SomeFont",
            "Encoding" => "WinAnsiEncoding",
        },
    )
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

/// Serialises a document to bytes.
pub fn bytes(document: &Document) -> Vec<u8> {
    let mut copy = document.clone();
    let mut out = Vec::new();
    copy.save_to(&mut out).unwrap();
    out
}
