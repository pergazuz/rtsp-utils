//! Application layer: use cases that orchestrate the domain. Knows nothing
//! about MP4 boxes, sockets or the RTSP grammar.

pub mod config;
pub mod control;
pub mod preview;
pub mod publish;
pub mod registry;
pub mod session;

pub use config::ServerConfig;
pub use control::{StreamControl, StreamStatus, TrackStatus};
pub use publish::PublishMedia;
pub use registry::{PublishedStream, StreamRegistry};
pub use session::{PlaybackSession, TrackStream};
