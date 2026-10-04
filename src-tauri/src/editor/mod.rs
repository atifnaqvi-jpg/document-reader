//! The P2 editing seam (P2-D1). Commands delegate here; no editing logic lives
//! in the Tauri command layer.

pub mod annotate;
pub mod events;
pub mod export;
pub mod gc;
pub mod geometry;
pub mod lopdf_write;
pub mod pages;
pub mod save;
pub mod undo;
