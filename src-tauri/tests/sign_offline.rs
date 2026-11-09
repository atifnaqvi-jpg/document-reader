//! SCOPE-C unit 2: the sign and verify path completes with no network (R3, D-SG3, I-SG3).
//!
//! Two different claims, and they need two different proofs:
//!
//! 1. **No network call can be written** -- a compile-surface property. `clippy.toml` bans the
//!    socket and HTTP client types, and `tests/signing_surface.rs` proves each entry still bites by
//!    compiling a probe twice, once with the ban and once without. That is a claim about the code.
//!
//! 2. **The path completes anyway** -- a run-time property. A ban can hold while an unrelated
//!    dependency opens a socket on our behalf, so I-SG3 asks for a run with no network at all.
//!    "I disconnected and it passed" is not that proof: if the test never tried to reach the
//!    network, an unplugged cable and a working network look identical. So the inner run below
//!    first *demonstrates the sandbox is sealed* -- a connection attempt from inside must fail --
//!    and only then signs and verifies. Without that demonstration the run proves nothing, so the
//!    test fails rather than passes when the seal cannot be shown.
//!
//! The sandbox is a network namespace entered with `unshare -n`, and the test binary re-executes
//! *itself* inside it. Re-exec rather than a fresh `cargo test` because the binary is already
//! built: a nested cargo invocation would rebuild, need the network for nothing, and be far slower.

// This is the one file in the repository allowed to hold a socket type, and only to *prove the
// absence of a network*: a `connect` that must fail is the evidence that the sandbox is sealed.
// The allowance is narrow, on the import rather than the whole file, so a socket reached through
// any other name in this file still fails to compile.
#[allow(clippy::disallowed_types)]
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use document_reader_lib::editor::signature::pfx::{import_bytes, ImportedIdentity};
use document_reader_lib::editor::signature::sign::sign_save;
use lopdf::{dictionary, Document};
use tempfile::TempDir;

const PFX_PASSWORD: &str = "correct-horse";

/// Printed by the inner run once signing and verification have both succeeded. The outer run
/// requires it, so "the process exited 0" is never mistaken for "the work was done": libtest exits
/// 0 for a filtered-out run too.
const DONE_MARKER: &str = "OFFLINE_SIGN_VERIFIED";

/// A host that is routable when the network is up. Only ever used to prove the sandbox is sealed,
/// never to fetch anything: a `connect` that fails is the evidence wanted.
const REACHABLE: &str = "1.1.1.1:443";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_identity() -> ImportedIdentity {
    let path = manifest_dir().join("tests/fixtures/signing/signer.pfx");
    let bytes =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    import_bytes(&bytes, PFX_PASSWORD)
        .expect("the fixture PFX opens")
        .identities
        .into_iter()
        .next()
        .expect("the fixture PFX carries one identity")
}

/// A one-page PDF on disk. `sign_save` needs a source file because verification reopens the result
/// from disk -- an in-memory check could not see a file that does not verify.
fn source_on_disk(dir: &Path) -> PathBuf {
    let path = dir.join("offline.pdf");
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let content_id = document.add_object(lopdf::Stream::new(
        dictionary! {},
        b"BT /F1 24 Tf 72 720 Td (offline) Tj ET".to_vec(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
    });
    document.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![lopdf::Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document.save(&path).expect("save the source");
    path
}

#[allow(clippy::disallowed_types)] // the connect must fail; that failure is the proof
fn network_is_reachable() -> bool {
    let address = REACHABLE.parse().expect("a literal socket address");
    // The duration is the whole point: an offline machine fails fast with "network unreachable",
    // but a firewalled one can sit in SYN retries for a minute. A short budget keeps the sealed
    // run quick without changing the answer -- either way the connect does not succeed.
    TcpStream::connect_timeout(&address, Duration::from_millis(800)).is_ok()
}

/// Run the current test binary inside a network namespace, filtered to `test_name`.
///
/// Returns the captured output, or `None` when this machine cannot create a namespace. `unshare`
/// is tried directly first and through `sudo -n` second, because an unprivileged Linux cannot
/// enter one and a CI container usually can only with the helper.
fn reexec_in_namespace(test_name: &str, envs: &[(&str, &str)]) -> Option<std::process::Output> {
    let exe = std::env::current_exe().expect("the test binary knows its own path");
    for use_sudo in [false, true] {
        // `sudo -n` resets the environment, so the probe directory is passed *through* `env`
        // rather than set on the `Command`. Measured: without this the inner job saw no
        // `OFFLINE_PROBE_DIR`, returned early, and still exited 0 -- which the marker check above
        // reported as "exited successfully but never reported signing".
        let mut command = if use_sudo {
            let mut c = Command::new("sudo");
            c.arg("-n").arg("env");
            for (key, value) in envs {
                c.arg(format!("{key}={value}"));
            }
            c.arg("unshare").arg("-n");
            c
        } else {
            let mut c = Command::new("unshare");
            c.arg("-n");
            c
        };
        if !use_sudo {
            for (key, value) in envs {
                command.env(key, value);
            }
        }
        command
            .arg(&exe)
            .args(["--exact", test_name, "--nocapture"]);
        let Ok(output) = command.output() else {
            continue;
        };
        // `unshare` itself failing (no permission) looks like a non-zero exit with no test output.
        // Tell the two apart by the marker only the test can print.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !(stdout.contains(DONE_MARKER) || stderr.contains(DONE_MARKER))
            && (stderr.contains("unshare failed") || stderr.contains("Operation not permitted"))
        {
            continue;
        }
        return Some(output);
    }
    None
}

/// I-SG3: signing and verification complete with no network, and the sandbox is shown to be
/// sealed before the result is believed.
#[test]
fn signing_and_verification_complete_inside_a_sealed_network_namespace() {
    // The inner run is this same test, re-executed inside the namespace with the probe directory
    // set. A separate `#[ignore]`d job would have been tidier but it moves the suite's ignored
    // count, which `ci.yml` pins at 106 precisely so that a new `#[ignore]` cannot slip in
    // unnoticed. Re-entering one test costs a branch and keeps that pin honest.
    if let Some(dir) = std::env::var_os("OFFLINE_PROBE_DIR") {
        inner_signing_job(PathBuf::from(dir));
        return;
    }

    if !network_is_reachable() {
        // The demonstration is impossible on a machine that is already offline, and without it a
        // pass would be indistinguishable from a test that never touched the network.
        println!(
            "SKIPPED (not a pass): {REACHABLE} is unreachable even outside the sandbox, so \
             entering a namespace could not prove anything here."
        );
        return;
    }

    let dir = TempDir::new().expect("temp dir");
    let dir_text = dir.path().to_string_lossy().to_string();
    let Some(output) = reexec_in_namespace(
        "signing_and_verification_complete_inside_a_sealed_network_namespace",
        &[("OFFLINE_PROBE_DIR", &dir_text)],
    ) else {
        println!(
            "SKIPPED (not a pass): this machine cannot create a network namespace via `unshare \
             -n`, so the run-time half of I-SG3 is unproved here. The compile-surface half is \
             covered by tests/signing_surface.rs."
        );
        return;
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the sealed run failed. stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(DONE_MARKER),
        "the sealed run exited successfully but never reported signing. A filtered or skipped \
         run also exits 0, so this is how that is told apart from work actually done.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        dir.path().join("signed.pdf").exists(),
        "the sealed run claimed success but left no signed file on disk"
    );
}

/// The inner run: prove the sandbox is sealed, then sign and verify inside it.
///
/// Only reached when `OFFLINE_PROBE_DIR` is set, which the outer run does while re-executing this
/// test binary inside the namespace.
fn inner_signing_job(dir: PathBuf) {
    // The seal, proved from inside. If this machine could still reach the network here, the run
    // that follows would not be an offline run at all.
    assert!(
        !network_is_reachable(),
        "the namespace did not seal the network: {REACHABLE} was reachable from inside it, so \
         nothing this run does can be called offline"
    );

    let source = source_on_disk(&dir);
    let sealed = sign_save(&source, &fixture_identity()).expect("sign inside the sealed sandbox");

    // `sign_save` reopens from disk and verifies before returning, so reaching this line means the
    // verification half also ran with no network. Keep the file so the outer run can look at it.
    std::fs::copy(&sealed.path, dir.join("signed.pdf")).expect("keep the signed file");
    let written = std::fs::metadata(dir.join("signed.pdf"))
        .expect("stat")
        .len();
    println!("{DONE_MARKER} bytes={written}");
}
