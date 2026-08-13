//! rtsp-utils — publish local video or JPEG files as live RTSP streams.
//!
//! Layers (dependencies only ever point inwards):
//!   presentation -> application -> domain
//!   infrastructure -> domain   (implements the domain ports)
//!
//! The binary in `main.rs` is the composition root that wires them together.

pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod presentation;
