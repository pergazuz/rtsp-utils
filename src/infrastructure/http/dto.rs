//! Shapes the control API sends to the browser.

use std::time::UNIX_EPOCH;

use super::files::{FileBrowser, Listing, Segment};
use super::json::Json;
use crate::application::{ServerConfig, StreamStatus};

pub fn stream(status: &StreamStatus) -> Json {
    Json::object(vec![
        ("name", Json::string(&status.name)),
        ("path", Json::string(&status.path)),
        ("url", Json::string(&status.url)),
        // `state` rather than a bare boolean: the UI switches on it, and there
        // is room for more states later without breaking the contract.
        (
            "state",
            Json::string(if status.active { "running" } else { "stopped" }),
        ),
        ("looping", Json::Bool(status.looping)),
        ("durationSeconds", Json::Number(status.duration_secs)),
        ("viewers", Json::Number(status.viewers as f64)),
        (
            "startedAt",
            match status.started_at {
                Some(at) => at
                    .duration_since(UNIX_EPOCH)
                    .map(|d| Json::Number(d.as_millis() as f64))
                    .unwrap_or(Json::Null),
                None => Json::Null,
            },
        ),
        // Whether the browser can show a live preview of this stream.
        (
            "previewable",
            Json::Bool(status.tracks.iter().any(|t| t.codec_string.is_some())),
        ),
        (
            "tracks",
            Json::Array(
                status
                    .tracks
                    .iter()
                    .map(|track| {
                        Json::object(vec![
                            ("kind", Json::string(track.kind)),
                            ("codec", Json::string(&track.codec)),
                            ("detail", Json::string(&track.detail)),
                            ("control", Json::string(&track.control)),
                            (
                                // The `codecs=` value MSE needs, for video.
                                "codecString",
                                match &track.codec_string {
                                    Some(c) => Json::string(c),
                                    None => Json::Null,
                                },
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

pub fn listing(listing: &Listing, browser: &FileBrowser) -> Json {
    Json::object(vec![
        ("path", Json::string(&listing.path)),
        (
            "parent",
            match &listing.parent {
                Some(parent) => Json::string(parent),
                None => Json::Null,
            },
        ),
        // Where the picker opens, so the UI can offer a way back.
        ("start", Json::string(browser.start_display())),
        ("confined", Json::Bool(browser.is_confined())),
        ("truncated", Json::Bool(listing.truncated)),
        ("roots", segments(browser.roots())),
        ("segments", segments(&listing.segments)),
        (
            "entries",
            Json::Array(
                listing
                    .entries
                    .iter()
                    .map(|entry| {
                        Json::object(vec![
                            ("name", Json::string(&entry.name)),
                            ("path", Json::string(&entry.path)),
                            ("directory", Json::Bool(entry.directory)),
                            ("size", Json::Number(entry.size as f64)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn segments(segments: &[Segment]) -> Json {
    Json::Array(
        segments
            .iter()
            .map(|segment| {
                Json::object(vec![
                    ("label", Json::string(&segment.label)),
                    ("path", Json::string(&segment.path)),
                ])
            })
            .collect(),
    )
}

pub fn streams(all: &[StreamStatus]) -> Json {
    Json::object(vec![(
        "streams",
        Json::Array(all.iter().map(stream).collect()),
    )])
}

pub fn health(config: &ServerConfig, stream_count: usize, browser: &FileBrowser) -> Json {
    Json::object(vec![
        ("ok", Json::Bool(true)),
        ("version", Json::string(env!("CARGO_PKG_VERSION"))),
        ("mediaRoot", Json::string(browser.start_display())),
        ("confined", Json::Bool(browser.is_confined())),
        (
            "rtsp",
            Json::object(vec![
                ("host", Json::string(&config.advertised_host)),
                ("port", Json::Number(config.port() as f64)),
                ("bind", Json::string(config.bind.to_string())),
                ("looping", Json::Bool(config.looping)),
            ]),
        ),
        ("streamCount", Json::Number(stream_count as f64)),
    ])
}

pub fn error(message: &str) -> Json {
    Json::object(vec![("error", Json::string(message))])
}
