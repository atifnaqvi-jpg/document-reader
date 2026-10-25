//! The P2 editing seam (P2-D1). Commands delegate here; no editing logic lives
//! in the Tauri command layer.

pub mod afm;
pub mod annotate;
pub mod cid;
pub mod events;
pub mod export;
pub mod gc;
pub mod geometry;
pub mod lopdf_write;
pub mod pages;
pub mod protect;
pub mod runmetrics;
pub mod save;
pub mod session;
pub mod substitute;
#[cfg(any(test, feature = "testfixtures"))]
pub mod testpdf;
pub mod textedit;
pub mod textlocate;
pub mod undo;
