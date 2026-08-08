//! Media entities: what a source file contains, expressed independently of any
//! container format or wire protocol.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

impl TrackKind {
    /// The SDP media type for this kind of track.
    pub fn sdp_media(&self) -> &'static str {
        match self {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
        }
    }
}

/// Everything a receiver needs to decode an H.264 elementary stream.
#[derive(Debug, Clone)]
pub struct H264Params {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    /// Byte width of the length prefix in AVCC-formatted samples (1, 2 or 4).
    pub nal_length_size: usize,
    pub width: u16,
    pub height: u16,
}

impl H264Params {
    /// The `codecs=` identifier used by MIME types and Media Source
    /// Extensions, e.g. `avc1.4d001f`: the profile, constraint flags and level
    /// bytes that follow the SPS NAL header.
    pub fn codec_string(&self) -> String {
        let profile = self.sps.get(1).copied().unwrap_or(0x42);
        let compatibility = self.sps.get(2).copied().unwrap_or(0);
        let level = self.sps.get(3).copied().unwrap_or(0x1e);
        format!("avc1.{profile:02x}{compatibility:02x}{level:02x}")
    }
}

/// AAC decoder setup, taken from the AudioSpecificConfig in the container.
#[derive(Debug, Clone)]
pub struct AacParams {
    pub config: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u8,
}

#[derive(Debug, Clone)]
pub enum CodecParams {
    H264(H264Params),
    Aac(AacParams),
}

impl CodecParams {
    /// RTP clock rate for this payload, in Hz.
    pub fn clock_rate(&self) -> u32 {
        match self {
            CodecParams::H264(_) => 90_000,
            CodecParams::Aac(a) => a.sample_rate,
        }
    }

    pub fn encoding_name(&self) -> &'static str {
        match self {
            CodecParams::H264(_) => "H264",
            CodecParams::Aac(_) => "mpeg4-generic",
        }
    }
}

/// One coded unit (a video frame or an audio access unit) located in the file.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    /// Decode timestamp, in the track's timescale.
    pub dts: u64,
    /// Presentation timestamp, in the track's timescale.
    pub pts: u64,
    pub keyframe: bool,
}

#[derive(Debug, Clone)]
pub struct Track {
    /// Position of the track within its source; also its RTSP control id.
    pub index: usize,
    pub kind: TrackKind,
    /// Ticks per second for this track's sample timestamps.
    pub timescale: u32,
    pub duration: u64,
    pub codec: CodecParams,
    pub samples: Vec<Sample>,
}

impl Track {
    /// The `a=control:` suffix advertised in SDP and echoed back on SETUP.
    pub fn control(&self) -> String {
        format!("trackID={}", self.index)
    }

    pub fn duration_secs(&self) -> f64 {
        if self.timescale == 0 {
            0.0
        } else {
            self.duration as f64 / self.timescale as f64
        }
    }

    /// Presentation time of a sample in nanoseconds from the start of the track.
    pub fn pts_nanos(&self, sample: &Sample) -> u128 {
        if self.timescale == 0 {
            return 0;
        }
        sample.pts as u128 * 1_000_000_000 / self.timescale as u128
    }

    /// Decode time of a sample in nanoseconds; used for send ordering.
    pub fn dts_nanos(&self, sample: &Sample) -> u128 {
        if self.timescale == 0 {
            return 0;
        }
        sample.dts as u128 * 1_000_000_000 / self.timescale as u128
    }
}

/// A file that has been probed and is ready to be published.
#[derive(Debug, Clone)]
pub struct MediaSource {
    /// Stream name; becomes the RTSP path segment.
    pub name: String,
    pub path: PathBuf,
    pub duration_secs: f64,
    pub tracks: Vec<Track>,
}

impl MediaSource {
    pub fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.iter().find(|t| t.index == index)
    }
}
