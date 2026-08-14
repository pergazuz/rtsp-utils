//! MOV / MP4 probing: turns a container file into `MediaSource` track tables.
//!
//! Only `moov` is read into memory. Sample payloads stay on disk and are
//! fetched lazily by `FileSampleReader` while a session plays.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::atom;
use crate::domain::media::{
    AacParams, CodecParams, H264Params, H265Params, MediaSource, Sample, Track, TrackKind,
};
use crate::domain::ports::{MediaProbe, SampleReader, SampleReaderFactory};
use crate::domain::{Error, Result};

/// Sampling frequencies indexed by the AudioSpecificConfig frequency index.
const AAC_SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

pub struct Mp4Probe;

impl MediaProbe for Mp4Probe {
    fn probe(&self, path: &Path, name: &str) -> Result<MediaSource> {
        let mut file = File::open(path)?;
        let moov = read_moov(&mut file)?;
        let mvhd = atom::find(&moov, b"mvhd")
            .ok_or_else(|| Error::MalformedContainer("moov has no mvhd".into()))?;
        let (movie_timescale, movie_duration) = parse_mvhd(mvhd)?;

        let mut tracks = Vec::new();
        for trak in atom::find_all(&moov, *b"trak") {
            match parse_track(trak, tracks.len()) {
                Ok(Some(track)) => tracks.push(track),
                // A track we cannot packetise (timecode, subtitles, ProRes…)
                // is skipped rather than failing the whole file.
                Ok(None) => {}
                Err(e) => eprintln!("[probe] skipping track: {e}"),
            }
        }

        if tracks.is_empty() {
            return Err(Error::UnsupportedMedia(format!(
                "{} contains no H.264/H.265 video or AAC audio track",
                path.display()
            )));
        }

        let duration_secs = if movie_timescale > 0 {
            movie_duration as f64 / movie_timescale as f64
        } else {
            tracks.iter().map(|t| t.duration_secs()).fold(0.0, f64::max)
        };

        Ok(MediaSource {
            name: name.to_string(),
            path: path.to_path_buf(),
            duration_secs,
            tracks,
        })
    }
}

/// Walks the top-level boxes and loads `moov` (wherever it sits — QuickTime
/// recordings usually park it after a multi-hundred-megabyte `mdat`).
fn read_moov(file: &mut File) -> Result<Vec<u8>> {
    let file_len = file.metadata()?.len();
    let mut pos = 0u64;

    while pos + 8 <= file_len {
        file.seek(SeekFrom::Start(pos))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let kind = [header[4], header[5], header[6], header[7]];

        let (header_len, size) = match size32 {
            1 => {
                let mut ext = [0u8; 8];
                file.read_exact(&mut ext)?;
                (16u64, u64::from_be_bytes(ext))
            }
            0 => (8u64, file_len - pos),
            n => (8u64, n as u64),
        };

        if size < header_len {
            return Err(Error::MalformedContainer(format!(
                "box '{}' at offset {pos} declares an impossible size of {size}",
                String::from_utf8_lossy(&kind)
            )));
        }

        if &kind == b"moov" {
            let body_len = (size - header_len) as usize;
            let mut body = vec![0u8; body_len];
            file.seek(SeekFrom::Start(pos + header_len))?;
            file.read_exact(&mut body)?;
            return Ok(body);
        }

        pos += size;
    }

    Err(Error::MalformedContainer(
        "no moov box found; the file is not a MOV/MP4 or is truncated".into(),
    ))
}

fn parse_mvhd(body: &[u8]) -> Result<(u32, u64)> {
    match atom::full_box_version(body)? {
        1 => Ok((atom::u32_at(body, 20)?, atom::u64_at(body, 24)?)),
        _ => Ok((atom::u32_at(body, 12)?, atom::u32_at(body, 16)? as u64)),
    }
}

/// Returns `Ok(None)` when the track exists but carries a codec we don't serve.
fn parse_track(trak: &[u8], index: usize) -> Result<Option<Track>> {
    let mdia = atom::find(trak, b"mdia")
        .ok_or_else(|| Error::MalformedContainer("trak has no mdia".into()))?;
    let mdhd = atom::find(mdia, b"mdhd")
        .ok_or_else(|| Error::MalformedContainer("mdia has no mdhd".into()))?;
    let (timescale, duration) = parse_mdhd(mdhd)?;

    let hdlr = atom::find(mdia, b"hdlr")
        .ok_or_else(|| Error::MalformedContainer("mdia has no hdlr".into()))?;
    let handler = hdlr.get(8..12).unwrap_or(&[0; 4]);
    let kind = match handler {
        b"vide" => TrackKind::Video,
        b"soun" => TrackKind::Audio,
        _ => return Ok(None),
    };

    let stbl = atom::find_path(mdia, &[b"minf", b"stbl"])
        .ok_or_else(|| Error::MalformedContainer("mdia has no minf/stbl".into()))?;
    let stsd = atom::find(stbl, b"stsd")
        .ok_or_else(|| Error::MalformedContainer("stbl has no stsd".into()))?;

    let codec = match parse_sample_description(stsd, kind)? {
        Some(codec) => codec,
        None => return Ok(None),
    };

    let samples = build_sample_table(stbl)?;
    if samples.is_empty() {
        return Ok(None);
    }

    Ok(Some(Track {
        index,
        kind,
        timescale,
        duration,
        codec,
        samples,
    }))
}

fn parse_mdhd(body: &[u8]) -> Result<(u32, u64)> {
    match atom::full_box_version(body)? {
        1 => Ok((atom::u32_at(body, 20)?, atom::u64_at(body, 24)?)),
        _ => Ok((atom::u32_at(body, 12)?, atom::u32_at(body, 16)? as u64)),
    }
}

/// Reads the first sample entry in `stsd` and extracts decoder setup from it.
fn parse_sample_description(stsd: &[u8], kind: TrackKind) -> Result<Option<CodecParams>> {
    // FullBox header (4) + entry_count (4), then the entries themselves.
    let entries = stsd
        .get(8..)
        .ok_or_else(|| Error::MalformedContainer("stsd is truncated".into()))?;
    let entry = match atom::children(entries).next() {
        Some(e) => e,
        None => return Ok(None),
    };

    match (kind, &entry.kind) {
        (TrackKind::Video, b"avc1") | (TrackKind::Video, b"avc3") => {
            Ok(Some(parse_avc_entry(entry.body)?))
        }
        (TrackKind::Video, b"hvc1") | (TrackKind::Video, b"hev1") => {
            Ok(Some(parse_hevc_entry(entry.body)?))
        }
        (TrackKind::Audio, b"mp4a") => parse_mp4a_entry(entry.body),
        (_, other) => {
            eprintln!(
                "[probe] sample format '{}' is not supported over RTP",
                String::from_utf8_lossy(other)
            );
            Ok(None)
        }
    }
}

/// VisualSampleEntry is a fixed 78-byte preamble followed by child boxes.
const VISUAL_SAMPLE_ENTRY_LEN: usize = 78;

fn parse_avc_entry(body: &[u8]) -> Result<CodecParams> {
    let width = atom::u16_at(body, 24)?;
    let height = atom::u16_at(body, 26)?;
    let children = body.get(VISUAL_SAMPLE_ENTRY_LEN..).ok_or_else(|| {
        Error::MalformedContainer("avc1 entry is shorter than a VisualSampleEntry".into())
    })?;
    let avcc = atom::find(children, b"avcC").ok_or_else(|| {
        Error::UnsupportedMedia("H.264 track has no avcC configuration record".into())
    })?;
    let (sps, pps, nal_length_size) = parse_avcc(avcc)?;

    Ok(CodecParams::H264(H264Params {
        sps,
        pps,
        nal_length_size,
        width,
        height,
    }))
}

/// AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.2.4.1).
fn parse_avcc(b: &[u8]) -> Result<(Vec<u8>, Vec<u8>, usize)> {
    let nal_length_size = (atom::u8_at(b, 4)? & 0x03) as usize + 1;
    let sps_count = (atom::u8_at(b, 5)? & 0x1f) as usize;

    let mut pos = 6usize;
    let mut sps = Vec::new();
    for i in 0..sps_count {
        let len = atom::u16_at(b, pos)? as usize;
        pos += 2;
        let unit = b
            .get(pos..pos + len)
            .ok_or_else(|| Error::MalformedContainer("avcC SPS runs past the box".into()))?;
        // Extra parameter sets are legal but rare; the first is the one we
        // advertise in SDP.
        if i == 0 {
            sps = unit.to_vec();
        }
        pos += len;
    }

    let pps_count = atom::u8_at(b, pos)? as usize;
    pos += 1;
    let mut pps = Vec::new();
    for i in 0..pps_count {
        let len = atom::u16_at(b, pos)? as usize;
        pos += 2;
        let unit = b
            .get(pos..pos + len)
            .ok_or_else(|| Error::MalformedContainer("avcC PPS runs past the box".into()))?;
        if i == 0 {
            pps = unit.to_vec();
        }
        pos += len;
    }

    if sps.is_empty() || pps.is_empty() {
        return Err(Error::UnsupportedMedia(
            "avcC carries no SPS/PPS, so receivers could never initialise a decoder".into(),
        ));
    }
    Ok((sps, pps, nal_length_size))
}

fn parse_hevc_entry(body: &[u8]) -> Result<CodecParams> {
    let width = atom::u16_at(body, 24)?;
    let height = atom::u16_at(body, 26)?;
    let children = body.get(VISUAL_SAMPLE_ENTRY_LEN..).ok_or_else(|| {
        Error::MalformedContainer("hvc1 entry is shorter than a VisualSampleEntry".into())
    })?;
    let hvcc = atom::find(children, b"hvcC").ok_or_else(|| {
        Error::UnsupportedMedia("H.265 track has no hvcC configuration record".into())
    })?;
    let (vps, sps, pps, nal_length_size) = parse_hvcc(hvcc)?;

    Ok(CodecParams::H265(H265Params {
        config: hvcc.to_vec(),
        vps,
        sps,
        pps,
        nal_length_size,
        width,
        height,
    }))
}

/// HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3.1): a 22-byte fixed
/// head, then arrays of parameter-set NAL units grouped by type.
fn parse_hvcc(b: &[u8]) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>, usize)> {
    let nal_length_size = (atom::u8_at(b, 21)? & 0x03) as usize + 1;
    let num_arrays = atom::u8_at(b, 22)? as usize;

    let mut pos = 23usize;
    let mut vps = Vec::new();
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    for _ in 0..num_arrays {
        let nal_type = atom::u8_at(b, pos)? & 0x3f;
        let count = atom::u16_at(b, pos + 1)? as usize;
        pos += 3;
        for _ in 0..count {
            let len = atom::u16_at(b, pos)? as usize;
            pos += 2;
            let unit = b.get(pos..pos + len).ok_or_else(|| {
                Error::MalformedContainer("hvcC parameter set runs past the box".into())
            })?;
            // As with avcC, extra parameter sets are legal; the first of each
            // type is the one we advertise.
            match nal_type {
                32 if vps.is_empty() => vps = unit.to_vec(),
                33 if sps.is_empty() => sps = unit.to_vec(),
                34 if pps.is_empty() => pps = unit.to_vec(),
                _ => {}
            }
            pos += len;
        }
    }

    // hev1 permits parameter sets to travel only in-band, in which case the
    // arrays are empty and there is nothing to put in the SDP.
    if vps.is_empty() || sps.is_empty() || pps.is_empty() {
        return Err(Error::UnsupportedMedia(
            "hvcC carries no VPS/SPS/PPS; in-band-only parameter sets are not supported".into(),
        ));
    }
    Ok((vps, sps, pps, nal_length_size))
}

fn parse_mp4a_entry(body: &[u8]) -> Result<Option<CodecParams>> {
    // QuickTime sound sample entries come in three layouts; the version field
    // says how much fixed data precedes the child boxes.
    let version = atom::u16_at(body, 8)?;
    let fixed_len = match version {
        0 => 28,
        1 => 28 + 16,
        2 => 28 + 36,
        v => {
            return Err(Error::UnsupportedMedia(format!(
                "unknown AudioSampleEntry version {v}"
            )))
        }
    };

    let channels = atom::u16_at(body, 16)? as u8;
    let entry_rate = (atom::u32_at(body, 24)? >> 16) as u32;

    let children = match body.get(fixed_len..) {
        Some(c) => c,
        None => return Ok(None),
    };
    // Plain MP4 puts esds directly under mp4a; QuickTime may wrap it in 'wave'.
    let esds = atom::find(children, b"esds")
        .or_else(|| atom::find(children, b"wave").and_then(|w| atom::find(w, b"esds")));
    let esds = match esds {
        Some(e) => e,
        None => {
            eprintln!("[probe] mp4a track has no esds descriptor; skipping audio");
            return Ok(None);
        }
    };

    let config = match parse_esds(esds)? {
        Some(c) => c,
        None => return Ok(None),
    };
    let (asc_rate, asc_channels) = parse_audio_specific_config(&config);

    Ok(Some(CodecParams::Aac(AacParams {
        config,
        sample_rate: asc_rate.unwrap_or(entry_rate.max(1)),
        channels: asc_channels.unwrap_or(channels.max(1)),
    })))
}

/// Digs the DecoderSpecificInfo (AudioSpecificConfig) out of an esds box.
fn parse_esds(body: &[u8]) -> Result<Option<Vec<u8>>> {
    // Skip the FullBox version/flags.
    let mut pos = 4usize;

    let (tag, len) = read_descriptor_header(body, &mut pos)?;
    if tag != 0x03 {
        return Ok(None);
    }
    let end = (pos + len).min(body.len());

    // ES_Descriptor: ES_ID (2) + flags (1), then optional extras the flags gate.
    pos += 2;
    let flags = atom::u8_at(body, pos)?;
    pos += 1;
    if flags & 0x80 != 0 {
        pos += 2; // dependsOn_ES_ID
    }
    if flags & 0x40 != 0 {
        let url_len = atom::u8_at(body, pos)? as usize;
        pos += 1 + url_len;
    }
    if flags & 0x20 != 0 {
        pos += 2; // OCR_ES_Id
    }

    while pos < end {
        let (tag, len) = read_descriptor_header(body, &mut pos)?;
        let next = (pos + len).min(body.len());
        match tag {
            // DecoderConfigDescriptor: 13 fixed bytes, then nested descriptors.
            0x04 => pos += 13,
            // DecoderSpecificInfo — the AudioSpecificConfig we came for.
            0x05 => {
                let cfg = body
                    .get(pos..next)
                    .ok_or_else(|| {
                        Error::MalformedContainer("esds DecoderSpecificInfo is truncated".into())
                    })?
                    .to_vec();
                return Ok(Some(cfg));
            }
            _ => pos = next,
        }
    }
    Ok(None)
}

/// MPEG-4 descriptor header: one tag byte then a 7-bits-per-byte length.
fn read_descriptor_header(b: &[u8], pos: &mut usize) -> Result<(u8, usize)> {
    let tag = atom::u8_at(b, *pos)?;
    *pos += 1;
    let mut len = 0usize;
    for _ in 0..4 {
        let byte = atom::u8_at(b, *pos)?;
        *pos += 1;
        len = (len << 7) | (byte & 0x7f) as usize;
        if byte & 0x80 == 0 {
            break;
        }
    }
    Ok((tag, len))
}

/// Reads sampling rate and channel count out of an AudioSpecificConfig.
fn parse_audio_specific_config(cfg: &[u8]) -> (Option<u32>, Option<u8>) {
    if cfg.len() < 2 {
        return (None, None);
    }
    let bits = u16::from_be_bytes([cfg[0], cfg[1]]);
    let freq_index = ((bits >> 7) & 0x0f) as usize;
    let channel_config = ((bits >> 3) & 0x0f) as u8;

    let rate = if freq_index == 0x0f {
        // Escape value: a 24-bit rate follows the index.
        if cfg.len() >= 5 {
            let raw = ((cfg[1] as u32 & 0x7f) << 17)
                | ((cfg[2] as u32) << 9)
                | ((cfg[3] as u32) << 1)
                | ((cfg[4] as u32) >> 7);
            Some(raw)
        } else {
            None
        }
    } else {
        AAC_SAMPLE_RATES.get(freq_index).copied()
    };

    let channels = if channel_config == 0 {
        None
    } else {
        Some(channel_config)
    };
    (rate, channels)
}

// ---- sample tables ----------------------------------------------------------

/// Reassembles stsz/stsc/stco/stts/ctts/stss into a flat, ordered sample list.
fn build_sample_table(stbl: &[u8]) -> Result<Vec<Sample>> {
    let sizes = parse_stsz(stbl)?;
    let chunk_offsets = parse_chunk_offsets(stbl)?;
    let stsc = parse_stsc(stbl)?;

    let mut samples: Vec<Sample> = Vec::with_capacity(sizes.len());

    // stsc describes runs of chunks that share a samples-per-chunk value.
    let mut sample_index = 0usize;
    for (i, entry) in stsc.iter().enumerate() {
        let last_chunk = if i + 1 < stsc.len() {
            stsc[i + 1].0.saturating_sub(1)
        } else {
            chunk_offsets.len() as u32
        };
        if entry.0 == 0 {
            return Err(Error::MalformedContainer(
                "stsc uses a zero chunk index (chunks are 1-based)".into(),
            ));
        }
        for chunk in entry.0..=last_chunk {
            let Some(&chunk_start) = chunk_offsets.get(chunk as usize - 1) else {
                break;
            };
            let mut offset = chunk_start;
            for _ in 0..entry.1 {
                let Some(&size) = sizes.get(sample_index) else {
                    break;
                };
                samples.push(Sample {
                    offset,
                    size,
                    dts: 0,
                    pts: 0,
                    keyframe: false,
                });
                offset += size as u64;
                sample_index += 1;
            }
        }
    }

    apply_timing(stbl, &mut samples)?;
    apply_sync_flags(stbl, &mut samples)?;
    Ok(samples)
}

fn parse_stsz(stbl: &[u8]) -> Result<Vec<u32>> {
    let stsz = atom::find(stbl, b"stsz")
        .ok_or_else(|| Error::MalformedContainer("stbl has no stsz".into()))?;
    let uniform = atom::u32_at(stsz, 4)?;
    let count = atom::u32_at(stsz, 8)? as usize;

    if uniform != 0 {
        return Ok(vec![uniform; count]);
    }
    let mut sizes = Vec::with_capacity(count);
    for i in 0..count {
        sizes.push(atom::u32_at(stsz, 12 + i * 4)?);
    }
    Ok(sizes)
}

fn parse_chunk_offsets(stbl: &[u8]) -> Result<Vec<u64>> {
    if let Some(stco) = atom::find(stbl, b"stco") {
        let count = atom::u32_at(stco, 4)? as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            out.push(atom::u32_at(stco, 8 + i * 4)? as u64);
        }
        return Ok(out);
    }
    if let Some(co64) = atom::find(stbl, b"co64") {
        let count = atom::u32_at(co64, 4)? as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            out.push(atom::u64_at(co64, 8 + i * 8)?);
        }
        return Ok(out);
    }
    Err(Error::MalformedContainer(
        "stbl has neither stco nor co64 chunk offsets".into(),
    ))
}

/// `(first_chunk, samples_per_chunk)` runs.
fn parse_stsc(stbl: &[u8]) -> Result<Vec<(u32, u32)>> {
    let stsc = atom::find(stbl, b"stsc")
        .ok_or_else(|| Error::MalformedContainer("stbl has no stsc".into()))?;
    let count = atom::u32_at(stsc, 4)? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = 8 + i * 12;
        out.push((atom::u32_at(stsc, at)?, atom::u32_at(stsc, at + 4)?));
    }
    Ok(out)
}

/// Fills in DTS from stts and PTS from stts+ctts.
fn apply_timing(stbl: &[u8], samples: &mut [Sample]) -> Result<()> {
    let stts = atom::find(stbl, b"stts")
        .ok_or_else(|| Error::MalformedContainer("stbl has no stts".into()))?;
    let entry_count = atom::u32_at(stts, 4)? as usize;

    let mut dts = 0u64;
    let mut index = 0usize;
    for i in 0..entry_count {
        let at = 8 + i * 8;
        let count = atom::u32_at(stts, at)?;
        let delta = atom::u32_at(stts, at + 4)? as u64;
        for _ in 0..count {
            let Some(sample) = samples.get_mut(index) else {
                break;
            };
            sample.dts = dts;
            sample.pts = dts;
            dts += delta;
            index += 1;
        }
    }

    // ctts shifts presentation ahead of decode when B-frames are present.
    if let Some(ctts) = atom::find(stbl, b"ctts") {
        let version = atom::full_box_version(ctts)?;
        let entry_count = atom::u32_at(ctts, 4)? as usize;
        let mut index = 0usize;
        for i in 0..entry_count {
            let at = 8 + i * 8;
            let count = atom::u32_at(ctts, at)?;
            // Version 1 offsets are signed and may be negative.
            let offset = if version == 0 {
                atom::u32_at(ctts, at + 4)? as i64
            } else {
                atom::i32_at(ctts, at + 4)? as i64
            };
            for _ in 0..count {
                let Some(sample) = samples.get_mut(index) else {
                    break;
                };
                sample.pts = (sample.dts as i64 + offset).max(0) as u64;
                index += 1;
            }
        }
    }
    Ok(())
}

/// Marks keyframes. A missing stss means every sample is a sync sample.
fn apply_sync_flags(stbl: &[u8], samples: &mut [Sample]) -> Result<()> {
    let Some(stss) = atom::find(stbl, b"stss") else {
        for sample in samples.iter_mut() {
            sample.keyframe = true;
        }
        return Ok(());
    };

    let count = atom::u32_at(stss, 4)? as usize;
    for i in 0..count {
        let number = atom::u32_at(stss, 8 + i * 4)? as usize;
        if number > 0 {
            if let Some(sample) = samples.get_mut(number - 1) {
                sample.keyframe = true;
            }
        }
    }
    Ok(())
}

// ---- reading sample payloads ------------------------------------------------

pub struct FileSampleReaderFactory;

impl SampleReaderFactory for FileSampleReaderFactory {
    fn open(&self, source: &MediaSource) -> Result<Box<dyn SampleReader>> {
        Ok(Box::new(FileSampleReader {
            file: File::open(&source.path)?,
        }))
    }
}

struct FileSampleReader {
    file: File,
}

impl SampleReader for FileSampleReader {
    fn read_sample(&mut self, offset: u64, size: u32, buf: &mut Vec<u8>) -> Result<()> {
        buf.clear();
        buf.resize(size as usize, 0);
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a payload in a box header.
    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn full_box_table(entries: &[u32]) -> Vec<u8> {
        let mut body = vec![0u8; 4]; // version + flags
        body.extend_from_slice(&((entries.len()) as u32).to_be_bytes());
        for e in entries {
            body.extend_from_slice(&e.to_be_bytes());
        }
        body
    }

    /// Two chunks of two samples each: sizes 10/20 and 30/40.
    fn sample_stbl(extra: Vec<u8>) -> Vec<u8> {
        let mut stsz = vec![0u8; 4];
        stsz.extend_from_slice(&0u32.to_be_bytes()); // per-sample sizes follow
        stsz.extend_from_slice(&4u32.to_be_bytes());
        for size in [10u32, 20, 30, 40] {
            stsz.extend_from_slice(&size.to_be_bytes());
        }

        let mut stsc = vec![0u8; 4];
        stsc.extend_from_slice(&1u32.to_be_bytes()); // one run
        stsc.extend_from_slice(&1u32.to_be_bytes()); // starting at chunk 1
        stsc.extend_from_slice(&2u32.to_be_bytes()); // two samples per chunk
        stsc.extend_from_slice(&1u32.to_be_bytes()); // sample description 1

        // Chunks at 1000 and 5000.
        let stco = full_box_table(&[1000, 5000]);

        // Four samples, 100 ticks apart.
        let mut stts = vec![0u8; 4];
        stts.extend_from_slice(&1u32.to_be_bytes());
        stts.extend_from_slice(&4u32.to_be_bytes());
        stts.extend_from_slice(&100u32.to_be_bytes());

        let mut stbl = Vec::new();
        stbl.extend_from_slice(&bx(b"stsz", &stsz));
        stbl.extend_from_slice(&bx(b"stsc", &stsc));
        stbl.extend_from_slice(&bx(b"stco", &stco));
        stbl.extend_from_slice(&bx(b"stts", &stts));
        stbl.extend_from_slice(&extra);
        stbl
    }

    #[test]
    fn sample_offsets_follow_the_chunk_layout() {
        let samples = build_sample_table(&sample_stbl(Vec::new())).unwrap();

        assert_eq!(samples.len(), 4);
        // Within a chunk each sample starts where the previous one ended.
        assert_eq!(samples[0].offset, 1000);
        assert_eq!(samples[1].offset, 1010);
        assert_eq!(samples[2].offset, 5000);
        assert_eq!(samples[3].offset, 5030);
        assert_eq!(
            samples.iter().map(|s| s.size).collect::<Vec<_>>(),
            vec![10, 20, 30, 40]
        );
    }

    #[test]
    fn decode_timestamps_accumulate_the_stts_deltas() {
        let samples = build_sample_table(&sample_stbl(Vec::new())).unwrap();
        assert_eq!(
            samples.iter().map(|s| s.dts).collect::<Vec<_>>(),
            vec![0, 100, 200, 300]
        );
        // Without ctts, presentation order equals decode order.
        assert_eq!(
            samples.iter().map(|s| s.pts).collect::<Vec<_>>(),
            vec![0, 100, 200, 300]
        );
    }

    #[test]
    fn ctts_offsets_shift_presentation_ahead_of_decode() {
        let mut ctts = vec![0u8; 4];
        ctts.extend_from_slice(&1u32.to_be_bytes());
        ctts.extend_from_slice(&4u32.to_be_bytes());
        ctts.extend_from_slice(&50u32.to_be_bytes());

        let samples = build_sample_table(&sample_stbl(bx(b"ctts", &ctts))).unwrap();
        assert_eq!(
            samples.iter().map(|s| s.pts).collect::<Vec<_>>(),
            vec![50, 150, 250, 350]
        );
    }

    #[test]
    fn every_sample_is_a_keyframe_when_stss_is_absent() {
        let samples = build_sample_table(&sample_stbl(Vec::new())).unwrap();
        assert!(samples.iter().all(|s| s.keyframe));
    }

    #[test]
    fn stss_marks_only_the_listed_one_based_sample_numbers() {
        // Samples 1 and 3 are sync samples.
        let samples = build_sample_table(&sample_stbl(bx(b"stss", &full_box_table(&[1, 3])))).unwrap();
        assert_eq!(
            samples.iter().map(|s| s.keyframe).collect::<Vec<_>>(),
            vec![true, false, true, false]
        );
    }

    #[test]
    fn avcc_yields_the_parameter_sets_and_length_prefix_width() {
        let sps = [0x67u8, 0x42, 0x00, 0x1e, 0xaa];
        let pps = [0x68u8, 0xce, 0x3c, 0x80];

        let mut avcc = vec![1, 0x42, 0x00, 0x1e, 0xff, 0xe1];
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&sps);
        avcc.push(1);
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&pps);

        let (got_sps, got_pps, length_size) = parse_avcc(&avcc).unwrap();
        assert_eq!(got_sps, sps);
        assert_eq!(got_pps, pps);
        // 0xff & 0x03 == 3, so the prefix is four bytes wide.
        assert_eq!(length_size, 4);
    }

    #[test]
    fn avcc_without_parameter_sets_is_rejected() {
        // Zero SPS and zero PPS entries.
        let avcc = vec![1, 0x42, 0x00, 0x1e, 0xff, 0xe0, 0x00];
        assert!(parse_avcc(&avcc).is_err());
    }

    /// A minimal HEVCDecoderConfigurationRecord: 22 fixed bytes, then arrays.
    fn hvcc_with_arrays(arrays: &[(u8, &[u8])]) -> Vec<u8> {
        let mut hvcc = vec![0u8; 21];
        hvcc[0] = 1; // configurationVersion
        hvcc.push(0xfc | 3); // four-byte NAL length prefixes
        hvcc.push(arrays.len() as u8);
        for (nal_type, unit) in arrays {
            hvcc.push(0x80 | nal_type);
            hvcc.extend_from_slice(&1u16.to_be_bytes());
            hvcc.extend_from_slice(&(unit.len() as u16).to_be_bytes());
            hvcc.extend_from_slice(unit);
        }
        hvcc
    }

    #[test]
    fn hvcc_yields_the_parameter_sets_and_length_prefix_width() {
        let vps = [0x40u8, 0x01, 0x0c];
        let sps = [0x42u8, 0x01, 0x01];
        let pps = [0x44u8, 0x01, 0xc0];
        let hvcc = hvcc_with_arrays(&[(32, &vps), (33, &sps), (34, &pps)]);

        let (got_vps, got_sps, got_pps, length_size) = parse_hvcc(&hvcc).unwrap();
        assert_eq!(got_vps, vps);
        assert_eq!(got_sps, sps);
        assert_eq!(got_pps, pps);
        assert_eq!(length_size, 4);
    }

    #[test]
    fn hvcc_without_parameter_sets_is_rejected() {
        // An SEI array alone leaves a decoder with nothing to start from.
        let hvcc = hvcc_with_arrays(&[(39, &[0x4e, 0x01])]);
        assert!(parse_hvcc(&hvcc).is_err());
    }

    #[test]
    fn audio_specific_config_decodes_rate_and_channels() {
        // AOT 2 (AAC-LC), frequency index 3 (48 kHz), 1 channel.
        assert_eq!(
            parse_audio_specific_config(&[0x11, 0x88]),
            (Some(48_000), Some(1))
        );
        // Frequency index 4 (44.1 kHz), 2 channels.
        assert_eq!(
            parse_audio_specific_config(&[0x12, 0x10]),
            (Some(44_100), Some(2))
        );
    }

    #[test]
    fn esds_descriptors_yield_the_decoder_specific_info() {
        let asc = [0x11u8, 0x90];
        let mut dsi = vec![0x05, asc.len() as u8];
        dsi.extend_from_slice(&asc);

        let mut dcd = vec![0x04, (13 + dsi.len()) as u8];
        dcd.extend_from_slice(&[0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        dcd.extend_from_slice(&dsi);

        let mut esds = vec![0u8; 4]; // FullBox version/flags
        esds.push(0x03);
        esds.push((3 + dcd.len()) as u8);
        esds.extend_from_slice(&[0x00, 0x01, 0x00]); // ES_ID + flags
        esds.extend_from_slice(&dcd);

        assert_eq!(parse_esds(&esds).unwrap(), Some(asc.to_vec()));
    }
}
