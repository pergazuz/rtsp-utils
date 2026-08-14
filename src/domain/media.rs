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

/// Everything a receiver needs to decode an H.265 (HEVC) elementary stream.
#[derive(Debug, Clone)]
pub struct H265Params {
    /// The raw HEVCDecoderConfigurationRecord from the container. Kept whole
    /// because its fixed head carries profile/tier/level fields that would
    /// otherwise need an SPS bitstream parser to recover.
    pub config: Vec<u8>,
    pub vps: Vec<u8>,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    /// Byte width of the length prefix in length-prefixed samples (1, 2 or 4).
    pub nal_length_size: usize,
    pub width: u16,
    pub height: u16,
}

impl H265Params {
    /// The `codecs=` identifier (ISO/IEC 14496-15 Annex E), e.g.
    /// `hvc1.1.6.L150.B0`: profile space and idc, the compatibility flags
    /// bit-reversed, tier and level, then constraint bytes with the zero tail
    /// dropped — all read straight out of the configuration record.
    pub fn codec_string(&self) -> String {
        let byte = |i: usize| self.config.get(i).copied().unwrap_or(0);

        let profile_space = match byte(1) >> 6 {
            1 => "A",
            2 => "B",
            3 => "C",
            _ => "",
        };
        let profile_idc = byte(1) & 0x1f;
        let tier = if byte(1) & 0x20 != 0 { "H" } else { "L" };
        let level_idc = byte(12);

        let compat = u32::from_be_bytes([byte(2), byte(3), byte(4), byte(5)]);
        let compat = compat.reverse_bits();

        let mut out = format!(
            "hvc1.{profile_space}{profile_idc}.{compat:X}.{tier}{level_idc}"
        );

        let constraints: Vec<u8> = (6..12).map(byte).collect();
        let keep = constraints
            .iter()
            .rposition(|&b| b != 0)
            .map_or(0, |i| i + 1);
        for b in &constraints[..keep] {
            out.push_str(&format!(".{b:X}"));
        }
        out
    }
}

/// AAC decoder setup, taken from the AudioSpecificConfig in the container.
#[derive(Debug, Clone)]
pub struct AacParams {
    pub config: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u8,
}

/// Everything RTP/JPEG (RFC 2435) needs to describe a baseline JPEG frame.
///
/// The wire format strips the JPEG headers and has the receiver rebuild them,
/// so the probe extracts here exactly what the payload headers must carry.
#[derive(Debug, Clone)]
pub struct JpegParams {
    /// RFC 2435 type code: 0 for YCbCr 4:2:2 chroma sampling, 1 for 4:2:0.
    pub type_code: u8,
    pub width: u16,
    pub height: u16,
    /// Restart interval from the DRI segment; zero when there is none.
    pub restart_interval: u16,
    /// Quantization tables in zigzag order: luma first, then chroma when the
    /// scan uses two distinct tables (64 or 128 bytes).
    pub quant_tables: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum CodecParams {
    H264(H264Params),
    H265(H265Params),
    Aac(AacParams),
    Jpeg(JpegParams),
}

impl CodecParams {
    /// RTP clock rate for this payload, in Hz.
    pub fn clock_rate(&self) -> u32 {
        match self {
            CodecParams::H264(_) | CodecParams::H265(_) | CodecParams::Jpeg(_) => 90_000,
            CodecParams::Aac(a) => a.sample_rate,
        }
    }

    pub fn encoding_name(&self) -> &'static str {
        match self {
            CodecParams::H264(_) => "H264",
            CodecParams::H265(_) => "H265",
            CodecParams::Aac(_) => "mpeg4-generic",
            CodecParams::Jpeg(_) => "JPEG",
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
