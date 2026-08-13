//! The use case the web UI drives: load files, put them on air, take them off
//! again, and report what is currently running.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use super::config::ServerConfig;
use super::publish::PublishMedia;
use super::registry::{PublishedStream, StreamRegistry};
use crate::domain::media::{CodecParams, MediaSource, TrackKind};
use crate::domain::ports::MediaProbe;
use crate::domain::{Error, Result};

/// A flattened view of one stream, ready to be serialised for the UI.
pub struct StreamStatus {
    pub name: String,
    pub path: String,
    pub url: String,
    pub active: bool,
    pub looping: bool,
    pub duration_secs: f64,
    pub viewers: usize,
    pub started_at: Option<SystemTime>,
    pub preview: Option<&'static str>,
    pub tracks: Vec<TrackStatus>,
}

pub struct TrackStatus {
    pub kind: &'static str,
    pub codec: String,
    pub detail: String,
    pub control: String,
    /// The `codecs=` identifier, for tracks a browser can play directly.
    pub codec_string: Option<String>,
}

pub struct StreamControl {
    registry: Arc<StreamRegistry>,
    publish: PublishMedia,
    config: ServerConfig,
}

impl StreamControl {
    pub fn new(
        probe: Arc<dyn MediaProbe>,
        registry: Arc<StreamRegistry>,
        config: ServerConfig,
    ) -> Self {
        let publish = PublishMedia::new(probe, Arc::clone(&registry), config.clone());
        Self {
            registry,
            publish,
            config,
        }
    }

    /// Loads a file and registers it. `active` decides whether it starts on air.
    pub fn add(
        &self,
        path: &Path,
        name: Option<&str>,
        active: bool,
    ) -> Result<Arc<PublishedStream>> {
        self.publish.execute(path, name, active)
    }

    pub fn start(&self, name: &str) -> Result<Arc<PublishedStream>> {
        let stream = self.lookup(name)?;
        stream.start();
        Ok(stream)
    }

    /// Takes a stream off air. Sessions already playing it stop within a beat,
    /// since they check the flag between samples.
    pub fn stop(&self, name: &str) -> Result<Arc<PublishedStream>> {
        let stream = self.lookup(name)?;
        stream.stop();
        Ok(stream)
    }

    /// Unloads a stream entirely, stopping it first.
    pub fn remove(&self, name: &str) -> Result<()> {
        self.registry
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| Error::NotFound(format!("no stream named '{name}'")))
    }

    pub fn status(&self, name: &str) -> Result<StreamStatus> {
        Ok(self.describe(&self.lookup(name)?))
    }

    pub fn list(&self) -> Vec<StreamStatus> {
        self.registry
            .list()
            .iter()
            .map(|stream| self.describe(stream))
            .collect()
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// The stream itself, for callers that need to read from it rather than
    /// just describe it.
    pub fn stream(&self, name: &str) -> Option<Arc<PublishedStream>> {
        self.registry.get(name)
    }

    fn lookup(&self, name: &str) -> Result<Arc<PublishedStream>> {
        self.registry
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("no stream named '{name}'")))
    }

    fn describe(&self, stream: &Arc<PublishedStream>) -> StreamStatus {
        let source = &stream.source;
        StreamStatus {
            name: source.name.clone(),
            path: source.path.display().to_string(),
            url: self.publish.url_for(&source.name).to_string(),
            active: stream.is_active(),
            looping: self.config.looping,
            duration_secs: source.duration_secs,
            viewers: stream.viewers(),
            started_at: stream.started_at(),
            preview: preview_kind(source),
            tracks: describe_tracks(source),
        }
    }
}

fn preview_kind(source: &MediaSource) -> Option<&'static str> {
    source.tracks.iter().find_map(|t| match &t.codec {
        CodecParams::H264(_) => Some("video"),
        CodecParams::Jpeg(_) => Some("image"),
        CodecParams::Aac(_) => None,
    })
}

fn describe_tracks(source: &MediaSource) -> Vec<TrackStatus> {
    source
        .tracks
        .iter()
        .map(|track| {
            let codec_string = match &track.codec {
                CodecParams::H264(p) => Some(p.codec_string()),
                // Neither goes through the MSE preview: AAC is not carried
                // at all, and JPEG previews as a still image instead.
                CodecParams::Aac(_) | CodecParams::Jpeg(_) => None,
            };

            let fps = if track.duration_secs() > 0.0 {
                track.samples.len() as f64 / track.duration_secs()
            } else {
                0.0
            };

            let (codec, detail) = match &track.codec {
                CodecParams::H264(p) => {
                    // Plain ASCII: this string goes to a Windows console as
                    // well as to the browser, and consoles mangle punctuation
                    // outside their code page.
                    (
                        "H.264".to_string(),
                        format!("{}x{} at {fps:.2} fps", p.width, p.height),
                    )
                }
                CodecParams::Aac(p) => (
                    "AAC".to_string(),
                    format!(
                        "{:.1} kHz {}",
                        p.sample_rate as f64 / 1000.0,
                        if p.channels == 1 { "mono" } else { "stereo" }
                    ),
                ),
                CodecParams::Jpeg(p) => (
                    "JPEG".to_string(),
                    format!("{}x{} still at {fps:.0} fps", p.width, p.height),
                ),
            };

            TrackStatus {
                kind: match track.kind {
                    TrackKind::Video => "video",
                    TrackKind::Audio => "audio",
                },
                codec,
                detail,
                control: track.control(),
                codec_string,
            }
        })
        .collect()
}
