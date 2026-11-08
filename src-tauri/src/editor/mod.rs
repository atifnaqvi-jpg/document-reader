//! The P2 editing seam (P2-D1). Commands delegate here; no editing logic lives
//! in the Tauri command layer.

pub mod afm;
pub mod annotate;
pub mod cid;
pub mod embed;
pub mod events;
pub mod export;
pub mod gc;
pub mod geometry;
pub mod lopdf_write;
pub mod migrate;
pub mod object_gc;
pub mod pages;
pub mod protect;
pub mod redact;
pub mod reflow;
pub mod runmetrics;
pub mod sanitize;
pub mod save;
pub mod session;
pub mod signature;
pub mod substitute;
#[cfg(any(test, feature = "testfixtures"))]
pub mod testpdf;
pub mod textedit;
pub mod textlocate;
pub mod xfdf;
