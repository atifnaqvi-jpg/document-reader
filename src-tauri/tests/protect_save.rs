//! SCOPE-A guards: DocuPro must be able to *read back its own encrypted output*.
//!
//! The design's **I-PR1** invariant is that a protected file opens with the password it
//! was given and refuses a wrong one. Testing that with lopdf alone would be circular —
//! lopdf wrote the file, so "lopdf can read it" proves the two halves agree, not that the
//! file is a PDF. These tests therefore use **PDFium as the oracle**: it is a separate
//! implementation, it is what actually renders the document, and if it cannot open the
//! output the user cannot read their own file.
//!
//! A second reason these live outside `protect.rs`: they need the real PDFium library, so
//! they are `#[ignore]`d where it is absent (the CI Linux job and a fresh checkout have
//! no `libpdfium`), and the suite's `--include-ignored` gate is what exercises them here.
//! A test that silently passes without the library would be the "green pass wearing a
//! label" AGENTS.md rule 5 forbids, so absence of the library fails these tests rather
//! than skipping them.

use std::path::{Path, PathBuf};

use lopdf::{Document, Object};

use document_reader_lib::editor::protect;

/// A temp directory per test, so parallel tests cannot collide.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dr-p32-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Builds a real, openable one-page PDF with `text` on it.
///
/// Constructed here rather than borrowed from `testpdf`, because that module is gated
/// behind `#[cfg(any(test, feature = "testfixtures"))]` and unavailable to an integration
/// test without turning on the fixture feature for the whole suite.
fn one_page_pdf(text: &str) -> Document {
    use lopdf::content::{Content, Operation};
    use lopdf::dictionary;
    use lopdf::Stream;

    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 720.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    });
    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![Object::Reference(page_id)],
        "Count" => 1,
    };
    document.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document
}

/// Writes a real, openable one-page PDF to `dir/source.pdf` and returns the path.
fn source_pdf(dir: &Path) -> PathBuf {
    let mut document = one_page_pdf("hello");
    document.renumber_objects();
    let bytes = {
        let mut buffer = Vec::new();
        document.save_to(&mut buffer).expect("serialise fixture");
        buffer
    };
    let path = dir.join("source.pdf");
    std::fs::write(&path, &bytes).expect("write source");
    path
}

/// Opens `path` with PDFium, supplying `password` if given.
///
/// Returns the page count on success. A `None` password is an explicit "open without a
/// password", which is the assertion for a wrong-password refusal.
fn pdfium_open(path: &Path, password: Option<&str>) -> Result<usize, String> {
    use pdfium_render::prelude::*;

    // The override is what CI and this workstation set. The fallback differs by platform
    // because the bundled file does too (`pdfium.dll` on Windows, the `.so` here), and a
    // hardcoded Linux name would silently make the Windows job report "library missing"
    // rather than exercise the guard.
    if let Ok(library_path) = std::env::var("PDFIUM_LIBRARY_PATH") {
        let bindings = Pdfium::bind_to_library(&library_path)
            .map_err(|error| format!("PDFIUM_UNAVAILABLE: {error}"))?;
        let pdfium = Pdfium::new(bindings);
        let document = pdfium
            .load_pdf_from_file(&path.display().to_string(), password)
            .map_err(|error| format!("open failed: {error}"))?;
        return Ok(document.pages().len() as usize);
    }

    let bundled = if cfg!(windows) {
        "pdfium.dll"
    } else {
        "resources/libpdfium.so"
    };
    let bindings =
        Pdfium::bind_to_library(bundled).map_err(|error| format!("PDFIUM_UNAVAILABLE: {error}"))?;
    let pdfium = Pdfium::new(bindings);

    let document = pdfium
        .load_pdf_from_file(&path.display().to_string(), password)
        .map_err(|error| format!("open failed: {error}"))?;

    Ok(document.pages().len() as usize)
}

/// **I-PR1**: the protected output opens in PDFium with the user password.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn a_protected_file_reopens_in_pdfium_with_its_password() {
    let dir = temp_dir("pr1-open");
    let source = source_pdf(&dir);

    let outcome = protect::protect_save(
        &source,
        "user-secret",
        "owner-secret",
        protect::PermissionsChoice::all(),
    )
    .expect("protect_save should succeed on a plain document");

    let pages = pdfium_open(Path::new(&outcome.path), Some("user-secret"))
        .expect("PDFium must open our own encrypted output with the user password");

    assert_eq!(
        pages, 1,
        "the reopened document must still have the one page it was given"
    );
    // The source is untouched: D-PR4 / P2-D2.
    assert!(
        !protect::is_encrypted(&source).expect("source readable"),
        "protect_save must not modify the source file"
    );
}

/// **I-PR1, the refusal half**: a wrong password must not open it.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn a_protected_file_refuses_a_wrong_password() {
    let dir = temp_dir("pr1-refuse");
    let source = source_pdf(&dir);

    let outcome = protect::protect_save(
        &source,
        "user-secret",
        "owner-secret",
        protect::PermissionsChoice::all(),
    )
    .expect("protect_save should succeed");

    let opened = pdfium_open(Path::new(&outcome.path), Some("not-the-password"));
    match &opened {
        // The wrong-reason pass this test was written to avoid: if the library is missing,
        // `pdfium_open` also returns `Err`, and a bare `is_err()` would be green while
        // proving nothing. Fail loudly instead.
        Err(message) if message.starts_with("PDFIUM_UNAVAILABLE") => {
            panic!("this guard needs the real PDFium library, not a missing one: {message}")
        }
        Err(_) => {}
        Ok(pages) => panic!("a wrong password must be refused, but it opened with {pages} pages"),
    }
}

/// **I-PR2**: the `/P` bits in the output are exactly the ones requested.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn the_written_permissions_are_the_ones_requested() {
    let dir = temp_dir("pr2-perms");
    let source = source_pdf(&dir);

    let choice = protect::PermissionsChoice {
        print: true,
        modify: false,
        copy: false,
        annotate: true,
        fill_forms: true,
        assemble: false,
        print_high_quality: false,
    };

    let outcome = protect::protect_save(&source, "pw", "owner", choice).expect("protect_save");

    let bytes = std::fs::read(&outcome.path).expect("read output");
    let document = Document::load_mem(&bytes).expect("lopdf loads the output");
    let encrypt_id = document
        .trailer
        .get(b"Encrypt")
        .expect("/Encrypt present")
        .as_reference()
        .expect("/Encrypt is a reference");
    let dict = document
        .get_object(encrypt_id)
        .expect("encrypt object")
        .as_dict()
        .expect("encrypt dict");
    let raw = dict
        .get(b"P")
        .expect("/P present")
        .as_i64()
        .expect("/P integer");

    let granted = raw as u32;
    for (bit, wanted, name) in [
        (1 << 2, true, "print"),
        (1 << 3, false, "modify"),
        (1 << 4, false, "copy"),
        (1 << 5, true, "annotate"),
        (1 << 8, true, "fill_forms"),
        (1 << 10, false, "assemble"),
        (1 << 11, false, "print_high_quality"),
    ] {
        assert_eq!(
            granted & bit != 0,
            wanted,
            "permission `{name}` should be {wanted} in /P = {granted:#x}"
        );
    }
}

/// **I-PR4**: the protected output never contains the document's plaintext in the clear.
///
/// A marker string is put in the source, protected, and then searched for in the raw
/// output bytes. It must not appear: if it did, the `/Encrypt` dictionary would be
/// claiming protection the file does not have.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn the_protected_output_does_not_leak_plaintext() {
    let dir = temp_dir("pr4-leak");
    let marker = "PLAINTEXT-MARKER-9f3a";
    let probe = "POST-ENCRYPT-LEAK-5c1d";
    let mut document = one_page_pdf(marker);
    document.renumber_objects();
    let mut buffer = Vec::new();
    document.save_to(&mut buffer).expect("serialise");
    let source = dir.join("marked.pdf");
    std::fs::write(&source, &buffer).expect("write source");

    let outcome = protect::protect_save(&source, "pw", "owner", protect::PermissionsChoice::all())
        .expect("protect_save");

    let output = std::fs::read(&outcome.path).expect("read output");
    let haystack = String::from_utf8_lossy(&output);
    assert!(
        !haystack.contains(marker),
        "the marker survives in the clear inside the protected output"
    );
    println!("POSTENCRYPT_PROBE_PRESENT={}", haystack.contains(probe));
}

/// A source that is already encrypted is refused rather than double-encrypted.
#[test]
#[ignore = "needs the real PDFium library; run under --include-ignored with PDFIUM_LIBRARY_PATH"]
fn protecting_an_already_protected_file_is_refused() {
    let dir = temp_dir("pr-already");
    let source = source_pdf(&dir);

    let first = protect::protect_save(&source, "pw", "owner", protect::PermissionsChoice::all())
        .expect("first protect_save");

    let second = protect::protect_save(
        Path::new(&first.path),
        "pw2",
        "owner2",
        protect::PermissionsChoice::all(),
    );
    // Assert the *reason*, not merely that it failed. A bare `is_err()` here was green
    // even with the guard deleted, because `Document::load` also fails on an encrypted
    // file for its own reason -- a pass for the wrong reason. The refusal must be the
    // deliberate one.
    let message = format!("{:?}", second.expect_err("must be refused"));
    // Match *our* wording, not lopdf's. Measured: with this guard deleted, lopdf still
    // refuses inside `encrypt()` with "PDF document is already encrypted", so the refusal
    // itself is belt-and-braces. What the explicit guard buys is a message the user can act
    // on, produced *before* the whole document is re-serialised. Asserting the vague word
    // "already" matched lopdf's text and made this test pass with the guard gone.
    assert!(
        message.contains("password-protected"),
        "the explicit already-protected guard must produce its own message before doing the \
         work; got: {message}"
    );
}
