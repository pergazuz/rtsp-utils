//! The HTTP control API: a thin adapter that exposes the `StreamControl` use
//! case to the web UI.

pub mod dto;
pub mod files;
pub mod json;
pub mod mime;
pub mod server;

pub use files::FileBrowser;
pub use server::ApiServer;
