//! Browser preview: walks a stream's video samples at the same real-time pace
//! the RTSP side uses, handing container fragments to a byte sink.
//!
//! This is a second, independent reader of the same source, not a mirror of
//! some other client's session — the same thing an RTSP client would receive
//! if it connected right now.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::registry::PublishedStream;
use crate::domain::media::{Track, TrackKind};
use crate::domain::ports::{ByteSink, FragmentSample, MediaFragmenter, SampleReader};
use crate::domain::{Error, Result};

/// Longest single sleep, so a stopped stream is noticed promptly.
const SLEEP_SLICE: Duration = Duration::from_millis(100);

/// Target fragment length. Short fragments keep latency down; too short and
/// the per-fragment box overhead starts to matter.
const FRAGMENT_MILLIS: u64 = 250;

/// Streams `stream`'s video track into `sink` until it stops, the client goes
/// away, or the file ends without looping.
pub fn run(
    stream: Arc<PublishedStream>,
    mut reader: Box<dyn SampleReader>,
    fragmenter: &mut dyn MediaFragmenter,
    sink: &mut dyn ByteSink,
    looping: bool,
    cancel: &AtomicBool,
) -> Result<()> {
    let source = Arc::clone(&stream.source);
    let track = source
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .ok_or_else(|| Error::UnsupportedMedia("this stream has no video track".into()))?;

    // Counts against the same viewer total as an RTSP client, because it is
    // one more consumer of the same source.
    let _viewer = stream.attach_viewer();

    sink.write_chunk(&fragmenter.init_segment()?)?;

    let durations = sample_durations(track);
    let fragment_ticks = FRAGMENT_MILLIS * track.timescale as u64 / 1000;
    let loop_length = loop_length_nanos(track);

    let start = Instant::now();
    let mut loop_offset: u128 = 0;
    let mut payload = Vec::with_capacity(512 * 1024);

    loop {
        for (index, sample) in track.samples.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) || !stream.is_active() {
                return Ok(());
            }

            let send_at = track.dts_nanos(sample) + loop_offset;
            if !sleep_until(start, send_at, cancel) {
                return Ok(());
            }

            if let Err(e) = reader.read_sample(sample.offset, sample.size, &mut payload) {
                eprintln!("[preview] dropping sample: {e}");
                continue;
            }

            fragmenter.push(FragmentSample {
                data: &payload,
                duration: durations[index],
                composition_offset: (sample.pts as i64 - sample.dts as i64) as i32,
                keyframe: sample.keyframe,
            });

            if fragmenter.buffered_duration() >= fragment_ticks {
                if let Some(fragment) = fragmenter.flush() {
                    // A write failure means the browser closed the tab; that
                    // is an ordinary end, not an error worth reporting.
                    if sink.write_chunk(&fragment).is_err() {
                        return Ok(());
                    }
                }
            }
        }

        if !looping {
            break;
        }
        loop_offset += loop_length;
    }

    if let Some(fragment) = fragmenter.flush() {
        let _ = sink.write_chunk(&fragment);
    }
    Ok(())
}

/// Each sample lasts until the next one decodes; the last reuses the previous
/// gap, since there is nothing after it to measure against.
fn sample_durations(track: &Track) -> Vec<u32> {
    let samples = &track.samples;
    let mut durations = Vec::with_capacity(samples.len());

    for i in 0..samples.len() {
        let duration = if i + 1 < samples.len() {
            samples[i + 1].dts.saturating_sub(samples[i].dts)
        } else {
            durations.last().copied().unwrap_or(1) as u64
        };
        // A zero-duration frame would stall a decoder.
        durations.push(duration.max(1) as u32);
    }
    durations
}

fn loop_length_nanos(track: &Track) -> u128 {
    let observed = track
        .samples
        .last()
        .map(|s| track.pts_nanos(s))
        .unwrap_or(0);
    let declared = (track.duration_secs() * 1e9) as u128;
    observed.max(declared).max(1)
}

fn sleep_until(start: Instant, target_nanos: u128, cancel: &AtomicBool) -> bool {
    loop {
        if cancel.load(Ordering::Relaxed) {
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
