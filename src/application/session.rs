//! Playback: walks a source's samples in presentation order, paced against a
//! wall clock, and pushes the resulting RTP packets into a sink.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::domain::media::MediaSource;
use crate::domain::ports::{Packetizer, RtcpReporter, RtpSink, SampleReader};

/// Longest single sleep; bounds how quickly a session notices TEARDOWN.
const SLEEP_SLICE: Duration = Duration::from_millis(100);

/// Interval between RTCP Sender Reports.
const REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// The first report goes out early: until a receiver has one report per track
/// it has no way to line the audio and video clocks up against each other.
const FIRST_REPORT_AFTER: Duration = Duration::from_secs(1);

/// One track that a client has SETUP and that will receive packets on PLAY.
pub struct TrackStream {
    pub track_index: usize,
    pub packetizer: Box<dyn Packetizer>,
    /// Random RTP timestamp base, reported back to the client in RTP-Info.
    pub rtp_timestamp_offset: u32,
}

/// A running playback thread. Dropping it stops the thread.
pub struct PlaybackSession {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PlaybackSession {
    pub fn start(
        source: Arc<MediaSource>,
        reader: Box<dyn SampleReader>,
        streams: Vec<TrackStream>,
        sink: Arc<dyn RtpSink>,
        reporter: Arc<dyn RtcpReporter>,
        looping: bool,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            run(source, reader, streams, sink, reporter, looping, worker_stop);
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PlaybackSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Per-track bookkeeping for RTCP.
struct Stats {
    packets: u32,
    octets: u32,
}

fn run(
    source: Arc<MediaSource>,
    mut reader: Box<dyn SampleReader>,
    mut streams: Vec<TrackStream>,
    sink: Arc<dyn RtpSink>,
    reporter: Arc<dyn RtcpReporter>,
    looping: bool,
    stop: Arc<AtomicBool>,
) {
    // Interleave the configured tracks into one decode-ordered timeline so the
    // audio and video packets leave in the order a receiver expects them.
    let mut timeline: Vec<(usize, usize, u128)> = Vec::new();
    for (stream_pos, stream) in streams.iter().enumerate() {
        let Some(track) = source.track(stream.track_index) else {
            continue;
        };
        for (sample_index, sample) in track.samples.iter().enumerate() {
            timeline.push((stream_pos, sample_index, track.dts_nanos(sample)));
        }
    }
    timeline.sort_by_key(|(_, _, dts)| *dts);

    if timeline.is_empty() {
        return;
    }

    let total_nanos = loop_length_nanos(&source, &streams);
    let mut stats: Vec<Stats> = streams
        .iter()
        .map(|_| Stats {
            packets: 0,
            octets: 0,
        })
        .collect();

    let start = Instant::now();
    let mut loop_offset: u128 = 0;
    let mut payload = Vec::with_capacity(256 * 1024);
    let mut packets: Vec<Vec<u8>> = Vec::with_capacity(64);
    let mut next_report = FIRST_REPORT_AFTER;

    'playback: loop {
        for &(stream_pos, sample_index, dts_nanos) in &timeline {
            if stop.load(Ordering::Relaxed) || sink.is_closed() {
                break 'playback;
            }

            let send_at = dts_nanos + loop_offset;
            if !sleep_until(start, send_at, &stop) {
                break 'playback;
            }

            let (track_index, clock_rate, ts_offset) = {
                let stream = &streams[stream_pos];
                (
                    stream.track_index,
                    stream.packetizer.clock_rate(),
                    stream.rtp_timestamp_offset,
                )
            };
            let Some(track) = source.track(track_index) else {
                continue;
            };
            let sample = track.samples[sample_index];

            if let Err(e) = reader.read_sample(sample.offset, sample.size, &mut payload) {
                eprintln!("[session] dropping sample: {e}");
                continue;
            }

            // Deriving the RTP timestamp from elapsed nanoseconds keeps both
            // tracks on one timeline across loop restarts.
            let pts_nanos = track.pts_nanos(&sample) + loop_offset;
            let timestamp = ts_offset
                .wrapping_add((pts_nanos * clock_rate as u128 / 1_000_000_000) as u32);

            packets.clear();
            let stream = &mut streams[stream_pos];
            if let Err(e) =
                stream
                    .packetizer
                    .packetize(timestamp, sample.keyframe, &payload, &mut packets)
            {
                eprintln!("[session] cannot packetise sample: {e}");
                continue;
            }

            for packet in &packets {
                if sink.send_rtp(track_index, packet).is_err() {
                    break 'playback;
                }
                stats[stream_pos].packets = stats[stream_pos].packets.wrapping_add(1);
                stats[stream_pos].octets = stats[stream_pos]
                    .octets
                    .wrapping_add(packet.len().saturating_sub(12) as u32);
            }

            if start.elapsed() >= next_report {
                emit_reports(&streams, &stats, &sink, &reporter, start);
                next_report += REPORT_INTERVAL;
            }
        }

        if !looping {
            break;
        }
        loop_offset += total_nanos;
    }
}

/// How far to advance every timestamp when the file restarts.
fn loop_length_nanos(source: &MediaSource, streams: &[TrackStream]) -> u128 {
    let from_tracks = streams
        .iter()
        .filter_map(|s| source.track(s.track_index))
        .map(|t| {
            // Track duration is authoritative; fall back to the last sample if
            // the header lies (some recorders leave it at zero).
            let declared = t.duration_secs();
            let observed = t
                .samples
                .last()
                .map(|s| t.pts_nanos(s) as f64 / 1e9)
                .unwrap_or(0.0);
            declared.max(observed)
        })
        .fold(0.0, f64::max);

    let secs = from_tracks.max(source.duration_secs).max(0.001);
    (secs * 1e9) as u128
}

fn emit_reports(
    streams: &[TrackStream],
    stats: &[Stats],
    sink: &Arc<dyn RtpSink>,
    reporter: &Arc<dyn RtcpReporter>,
    start: Instant,
) {
    // Timestamps advance with the wall clock, loops included, so elapsed time
    // is all a report needs.
    let elapsed = start.elapsed().as_nanos();
    for (i, stream) in streams.iter().enumerate() {
        let clock_rate = stream.packetizer.clock_rate() as u128;
        let now_ts = stream
            .rtp_timestamp_offset
            .wrapping_add((elapsed * clock_rate / 1_000_000_000) as u32);
        let report = reporter.sender_report(
            stream.packetizer.ssrc(),
            now_ts,
            stats[i].packets,
            stats[i].octets,
        );
        let _ = sink.send_rtcp(stream.track_index, &report);
    }
}

/// Sleeps until `target_nanos` after `start`, waking early if asked to stop.
/// Returns false when the session should terminate.
fn sleep_until(start: Instant, target_nanos: u128, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let elapsed = start.elapsed().as_nanos();
        if elapsed >= target_nanos {
            return true;
        }
        let remaining = Duration::from_nanos((target_nanos - elapsed).min(u64::MAX as u128) as u64);
        thread::sleep(remaining.min(SLEEP_SLICE));
    }
}
