//! Domain layer: entities, value objects and ports. No I/O, no protocol details.

pub mod error;
pub mod media;
pub mod ports;
pub mod url;

pub use error::{Error, Result};
