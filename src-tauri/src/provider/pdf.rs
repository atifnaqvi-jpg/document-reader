use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pdfium_render::prelude::*;

use crate::model::{AppError, DocumentMeta, Format, LibraryEntry};

use super::ReaderProvider;

const PDFIUM_DEV_ENV_VAR: &str = "PDFIUM_LIBRARY_PATH";
/// Shipped beside the executable in release installs (docs/P0-DESIGN.md §2).
const PDFIUM_BUNDLED_FILE: &str = "pdfium.dll";

/// Every place a bundled `pdfium.dll` may live, in priority order, given the
/// executable's directory. Pure so the resolution order is unit-testable.
fn candidate_paths_in(exe_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![exe_dir.join(PDFIUM_BUNDLED_FILE)];

    // Walk up a few levels looking for a `resources/` directory. One walk covers
    // both layouts we care about: a Tauri install (`<install>/resources/`) and a
    // dev build (`src-tauri/target/<profile>/` reaching `src-tauri/resources/`).
    // Tauri's exact resource placement has varied across versions, so we probe
    // rather than assume — a wrong guess here silently costs PDF support.
    let mut current = Some(exe_dir);
    for _ in 0..3 {
        let Some(dir) = current else { break };
        candidates.push(dir.join("resources").join(PDFIUM_BUNDLED_FILE));
        current = dir.parent();
    }

    candidates
}

fn exe_directory() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Resolution order, first hit wins:
///   1. `PDFIUM_LIBRARY_PATH` — explicit override, for dev and troubleshooting.
///   2. `pdfium.dll` beside the executable — where NSIS puts bundled resources.
///   3. `resources/pdfium.dll` under the executable directory or a close ancestor —
///      the other layout Tauri uses, depending on version.
///
/// Pure on purpose: environment and executable paths are passed in, so the whole
/// priority order is testable without mutating process-global env state.
fn resolve_library_path(
    env_override: Option<&str>,
    exe_dir: Option<&Path>,
) -> Result<PathBuf, AppError> {
    if let Some(raw) = env_override {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Ok(path);
        }
        // An explicit override that does not resolve is a configuration mistake
        // worth reporting, not something to silently paper over by falling back.
        return Err(AppError::PdfiumUnavailable(format!(
            "{PDFIUM_DEV_ENV_VAR} points at a missing file: {}",
            path.display()
        )));
    }

    if let Some(dir) = exe_dir {
        for candidate in candidate_paths_in(dir) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(AppError::PdfiumUnavailable(format!(
        "set {PDFIUM_DEV_ENV_VAR} or place {PDFIUM_BUNDLED_FILE} next to the executable"
    )))
}

/// Native PDFium rendering (DECISIONS Q1) — the OS webview PDF viewer is never used.
/// P0 reads metadata only; `render_page`/`extract_text` land in P1.
#[derive(Debug)]
pub struct PdfProvider;

impl PdfProvider {
    fn ensure_file(path: &Path) -> Result<(), AppError> {
        if path.is_file() {
            Ok(())
        } else {
            Err(AppError::FileNotFound(path.display().to_string()))
        }
    }

    /// Resolves once per process. Deliberately lazy: `current_exe()` only reflects
    /// the real install path once Tauri has run `set_current_dir` during setup.
    fn library_path() -> Result<&'static PathBuf, AppError> {
        static RESOLVED: OnceLock<Result<PathBuf, AppError>> = OnceLock::new();

        RESOLVED
            .get_or_init(|| {
                let env_override = std::env::var(PDFIUM_DEV_ENV_VAR).ok();
                resolve_library_path(env_override.as_deref(), exe_directory().as_deref())
            })
            .as_ref()
            .map_err(|e| AppError::PdfiumUnavailable(e.to_string()))
    }

    /// Whether a usable PDFium library is present on this machine.
    ///
    /// Tests gate real rasterization on this: the build machines (aarch64 Linux)
    /// cannot load Windows PDFium, so a plain `cargo test` must stay green without it.
    pub fn pdfium_available() -> bool {
        Self::bind().is_ok()
    }

    fn bind() -> Result<Box<dyn PdfiumLibraryBindings>, AppError> {
        let library = Self::library_path()?;
        Pdfium::bind_to_library(library)
            .map_err(|e| AppError::PdfiumUnavailable(format!("{}: {e}", library.display())))
    }
}

impl ReaderProvider for PdfProvider {
    fn open(&self, path: &Path) -> Result<DocumentMeta, AppError> {
        Self::ensure_file(path)?;
        let bindings = Self::bind()?;

        let pdfium = Pdfium::new(bindings);
        let document = pdfium
            .load_pdf_from_file(&path.display().to_string(), None)
            .map_err(|e| AppError::Other(format!("failed to open PDF: {e}")))?;

        let metadata = document.metadata();
        let tag = |kind: PdfDocumentMetadataTagType| {
            metadata
                .get(kind)
                .map(|tag| tag.value().trim().to_owned())
                .filter(|value| !value.is_empty())
        };

        Ok(DocumentMeta {
            id: String::new(), // assigned by the library layer on import
            title: tag(PdfDocumentMetadataTagType::Title).or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned)
            }),
            author: tag(PdfDocumentMetadataTagType::Author),
            pages: Some(document.pages().len() as u32),
            format: Format::Pdf,
        })
    }

    /// Real rasterization lands in P1.2 (PDFium -> PNG).
    fn render_page(
        &self,
        _entry: &LibraryEntry,
        _page: u32,
        _scale: f32,
    ) -> Result<Vec<u8>, AppError> {
        Err(AppError::RenderUnavailable)
    }

    /// PDFium text-layer extraction lands in P1.2.
    fn extract_text(&self, _entry: &LibraryEntry, _page: u32) -> Result<String, AppError> {
        Err(AppError::RenderUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal library row; these tests never reach the filesystem through it.
    fn entry() -> LibraryEntry {
        LibraryEntry {
            meta: DocumentMeta {
                id: "x".into(),
                title: None,
                author: None,
                pages: Some(1),
                format: Format::Pdf,
            },
            path: "C:/docs/fixture.pdf".into(),
            missing: false,
            added_at: 0,
            last_opened_at: None,
        }
    }

    #[test]
    fn missing_file_fails_before_touching_pdfium() {
        let error = PdfProvider
            .open(Path::new("definitely/not/here.pdf"))
            .unwrap_err();
        assert_eq!(error.code(), "FILE_NOT_FOUND");
    }

    #[test]
    fn absent_pdfium_reports_a_clear_error_rather_than_panicking() {
        let path = std::env::temp_dir().join("docreader-p0-not-a-real.pdf");
        std::fs::write(&path, b"%PDF-1.4\nnot really a pdf\n").unwrap();

        // Either pdfium is present (dev machine) or we get a typed, actionable
        // error — the one thing that must never happen is a panic.
        if let Err(error) = PdfProvider.open(&path) {
            assert!(
                matches!(error.code(), "PDFIUM_UNAVAILABLE" | "OTHER"),
                "unexpected error: {error}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn render_is_deferred_to_a_later_phase() {
        assert_eq!(
            PdfProvider
                .render_page(&entry(), 0, 1.0)
                .unwrap_err()
                .code(),
            "RENDER_UNAVAILABLE"
        );
        assert_eq!(
            PdfProvider.extract_text(&entry(), 0).unwrap_err().code(),
            "RENDER_UNAVAILABLE"
        );
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn dll_beside_the_executable_is_tried_first() {
        let found = names(&candidate_paths_in(Path::new("C:/Apps/DocumentReader")));
        assert_eq!(found[0], "C:/Apps/DocumentReader/pdfium.dll");
    }

    #[test]
    fn install_layouts_are_covered() {
        // NSIS install: resources alongside the exe.
        let install = names(&candidate_paths_in(Path::new("C:/Apps/DocumentReader")));
        assert!(
            install.contains(&"C:/Apps/DocumentReader/resources/pdfium.dll".to_string()),
            "install layout missing: {install:?}"
        );

        // Dev build: src-tauri/target/debug -> src-tauri/resources.
        let dev = names(&candidate_paths_in(Path::new(
            "C:/repo/src-tauri/target/debug",
        )));
        assert!(
            dev.contains(&"C:/repo/src-tauri/resources/pdfium.dll".to_string()),
            "dev layout missing: {dev:?}"
        );
    }

    #[test]
    fn candidate_discovery_never_panics_on_shallow_paths() {
        // A root-level exe dir has no ancestors; the walk must terminate cleanly.
        for dir in ["/", "C:/"] {
            let paths = candidate_paths_in(Path::new(dir));
            assert!(!paths.is_empty());
        }
    }

    #[test]
    fn the_first_existing_candidate_wins() {
        let dir = std::env::temp_dir().join(format!("docreader-pdfium-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Nothing present yet: no candidate resolves.
        assert!(!candidate_paths_in(&dir).iter().any(|p| p.is_file()));

        // Only the nested resource copy exists; it must still be found.
        let nested = dir.join("resources");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(PDFIUM_BUNDLED_FILE), b"fake").unwrap();

        let resolved = candidate_paths_in(&dir).into_iter().find(|p| p.is_file());
        assert_eq!(resolved, Some(nested.join(PDFIUM_BUNDLED_FILE)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("docreader-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn installed_layout_resolves_the_dll_beside_the_executable() {
        // The exact shape the NSIS installer produces (verified against a real
        // installer payload): pdfium.dll sits next to document-reader.exe.
        let dir = scratch_dir("installed");
        std::fs::write(dir.join(PDFIUM_BUNDLED_FILE), b"dll").unwrap();

        let resolved = resolve_library_path(None, Some(&dir)).unwrap();
        assert_eq!(resolved, dir.join(PDFIUM_BUNDLED_FILE));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_layout_resolves_the_resource_copy_up_the_tree() {
        // target/debug/ -> walk up -> src-tauri/resources/pdfium.dll
        let root = scratch_dir("dev");
        let resources = root.join("resources");
        let profile = root.join("target").join("debug");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(resources.join(PDFIUM_BUNDLED_FILE), b"dll").unwrap();

        let resolved = resolve_library_path(None, Some(&profile)).unwrap();
        assert_eq!(resolved, resources.join(PDFIUM_BUNDLED_FILE));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn env_override_takes_priority_over_bundled_copies() {
        let root = scratch_dir("override");
        let bundled = root.join(PDFIUM_BUNDLED_FILE);
        let override_path = root.join("elsewhere.pdfium");
        std::fs::write(&bundled, b"bundled").unwrap();
        std::fs::write(&override_path, b"override").unwrap();

        let resolved =
            resolve_library_path(Some(&override_path.display().to_string()), Some(&root)).unwrap();
        assert_eq!(resolved, override_path, "env override must win");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_env_override_is_reported_rather_than_silently_ignored() {
        // Falling back to a bundled copy would mask a real misconfiguration.
        let root = scratch_dir("broken-override");
        std::fs::write(root.join(PDFIUM_BUNDLED_FILE), b"bundled").unwrap();

        let error = resolve_library_path(Some("C:/nope/missing.dll"), Some(&root)).unwrap_err();
        assert_eq!(error.code(), "PDFIUM_UNAVAILABLE");
        assert!(error.to_string().contains("missing file"), "{error}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn absent_pdfium_reports_an_actionable_error() {
        let root = scratch_dir("absent");
        let error = resolve_library_path(None, Some(&root)).unwrap_err();
        assert_eq!(error.code(), "PDFIUM_UNAVAILABLE");
        // The message must tell the user how to fix it.
        assert!(error.to_string().contains(PDFIUM_DEV_ENV_VAR), "{error}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolution_without_an_exe_directory_still_reports_cleanly() {
        // current_exe() can fail; that must be an error, never a panic.
        assert_eq!(
            resolve_library_path(None, None).unwrap_err().code(),
            "PDFIUM_UNAVAILABLE"
        );
    }

    #[test]
    fn pdfium_availability_check_never_panics() {
        // Gated render tests call this; it must be safe on machines without PDFium.
        let _ = PdfProvider::pdfium_available();
    }
}
