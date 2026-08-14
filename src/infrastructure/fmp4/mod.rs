//! Fragmented MP4 muxing (ISO/IEC 14496-12), so a browser can play the same
//! H.264 or H.265 samples the RTSP side sends.
//!
//! Nothing is transcoded. The container already stores length-prefixed
//! samples, which is exactly what an fMP4 `mdat` wants, so the sample bytes
//! are copied through untouched and only the boxes around them are new.

mod boxes;

use boxes::*;

use crate::domain::media::{H264Params, H265Params};
use crate::domain::ports::{FragmentSample, MediaFragmenter};
use crate::domain::Result;

/// The video codecs the fragmenter can wrap.
pub enum VideoParams {
    H264(H264Params),
    H265(H265Params),
}

impl VideoParams {
    pub fn width(&self) -> u16 {
        match self {
            VideoParams::H264(p) => p.width,
            VideoParams::H265(p) => p.width,
        }
    }

    pub fn height(&self) -> u16 {
        match self {
            VideoParams::H264(p) => p.height,
            VideoParams::H265(p) => p.height,
        }
    }
}

/// Video track id inside the generated file. There is only ever one.
const TRACK_ID: u32 = 1;

struct PendingSample {
    data: Vec<u8>,
    duration: u32,
    composition_offset: i32,
    keyframe: bool,
}

pub struct Fmp4Fragmenter {
    params: VideoParams,
    timescale: u32,
    sequence: u32,
    /// Decode time of the next fragment, in `timescale` units.
    base_decode_time: u64,
    pending: Vec<PendingSample>,
    pending_duration: u64,
}

impl Fmp4Fragmenter {
    pub fn new(params: VideoParams, timescale: u32) -> Self {
        Self {
            params,
            timescale: timescale.max(1),
            sequence: 1,
            base_decode_time: 0,
            pending: Vec::new(),
            pending_duration: 0,
        }
    }
}

impl MediaFragmenter for Fmp4Fragmenter {
    fn init_segment(&self) -> Result<Vec<u8>> {
        let mut out = ftyp();
        out.extend_from_slice(&moov(&self.params, self.timescale, TRACK_ID));
        Ok(out)
    }

    fn push(&mut self, sample: FragmentSample<'_>) {
        self.pending_duration += sample.duration as u64;
        self.pending.push(PendingSample {
            data: sample.data.to_vec(),
            duration: sample.duration,
            composition_offset: sample.composition_offset,
            keyframe: sample.keyframe,
        });
    }

    fn flush(&mut self) -> Option<Vec<u8>> {
        if self.pending.is_empty() {
            return None;
        }

        let samples: Vec<TrunSample> = self
            .pending
            .iter()
            .map(|s| TrunSample {
                duration: s.duration,
                size: s.data.len() as u32,
                flags: sample_flags(s.keyframe),
                composition_offset: s.composition_offset,
            })
            .collect();

        let payload: usize = self.pending.iter().map(|s| s.data.len()).sum();
        let mut fragment =
            moof(self.sequence, TRACK_ID, self.base_decode_time, &samples);

        // mdat: an 8-byte header followed by the sample data, back to back.
        fragment.extend_from_slice(&((payload + 8) as u32).to_be_bytes());
        fragment.extend_from_slice(b"mdat");
        for sample in &self.pending {
            fragment.extend_from_slice(&sample.data);
        }

        self.sequence += 1;
        self.base_decode_time += self.pending_duration;
        self.pending.clear();
        self.pending_duration = 0;

        Some(fragment)
    }

    fn buffered_duration(&self) -> u64 {
        self.pending_duration
    }
}

/// Per-sample flags from ISO/IEC 14496-12 §8.8.3.1.
fn sample_flags(keyframe: bool) -> u32 {
    if keyframe {
        // sample_depends_on = 2 (an I-frame depends on nothing), sync sample.
        0x0200_0000
    } else {
        // sample_depends_on = 1, and is_non_sync_sample set.
        0x0101_0000
    }
}
