//! Ports: the interfaces the domain and application layers depend on.
//! Infrastructure supplies the implementations.

use std::path::Path;

use super::media::MediaSource;
use super::Result;

/// Inspects a container file and produces the track layout we can publish.
pub trait MediaProbe: Send + Sync {
    fn probe(&self, path: &Path, name: &str) -> Result<MediaSource>;
}

/// Pulls sample payloads out of the underlying storage on demand.
///
/// One reader per playing session, so each session seeks independently.
pub trait SampleReader: Send {
    fn read_sample(&mut self, offset: u64, size: u32, buf: &mut Vec<u8>) -> Result<()>;
}

/// Opens a fresh reader for a source. Shared across sessions, hence `Sync`.
pub trait SampleReaderFactory: Send + Sync {
    fn open(&self, source: &MediaSource) -> Result<Box<dyn SampleReader>>;
}

/// Turns one coded sample into a series of ready-to-send RTP packets.
pub trait Packetizer: Send {
    /// Appends complete RTP packets (header included) to `out`.
    fn packetize(
        &mut self,
        timestamp: u32,
        keyframe: bool,
        data: &[u8],
        out: &mut Vec<Vec<u8>>,
    ) -> Result<()>;

    fn ssrc(&self) -> u32;
    fn clock_rate(&self) -> u32;
    /// Sequence number the next packet will carry; reported in RTP-Info so a
    /// client knows where the stream starts.
    fn next_sequence(&self) -> u16;
}

/// One coded sample on its way into a container fragment.
pub struct FragmentSample<'a> {
    pub data: &'a [u8],
    /// How long this sample is shown, in the track's timescale.
    pub duration: u32,
    /// Presentation time relative to decode time; non-zero with B-frames.
    pub composition_offset: i32,
    pub keyframe: bool,
}

/// Packages coded samples into a container a browser can play, emitted as an
/// initialisation segment followed by media fragments.
pub trait MediaFragmenter: Send {
    /// Everything a decoder needs before the first fragment arrives.
    fn init_segment(&self) -> Result<Vec<u8>>;
    fn push(&mut self, sample: FragmentSample<'_>);
    /// Emits the accumulated samples as one fragment, if there are any.
    fn flush(&mut self) -> Option<Vec<u8>>;
    /// Duration accumulated since the last flush, in the track's timescale.
    fn buffered_duration(&self) -> u64;
}

/// A byte stream to a connected client, such as a chunked HTTP response.
pub trait ByteSink {
    fn write_chunk(&mut self, bytes: &[u8]) -> Result<()>;
}

/// Formats the periodic RTCP Sender Reports that let a receiver line up the
/// audio and video clocks.
pub trait RtcpReporter: Send + Sync {
    fn sender_report(&self, ssrc: u32, rtp_timestamp: u32, packets: u32, octets: u32) -> Vec<u8>;
}

/// Where a playing session writes its RTP/RTCP packets.
pub trait RtpSink: Send + Sync {
    fn send_rtp(&self, track: usize, packet: &[u8]) -> Result<()>;
    fn send_rtcp(&self, track: usize, packet: &[u8]) -> Result<()>;
    /// True once the transport is known to be dead, so playback can stop early.
    fn is_closed(&self) -> bool;
}
