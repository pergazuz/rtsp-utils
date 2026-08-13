//! Baseline JPEG probing for RTP/JPEG (RFC 2435): reads the markers the
//! payload headers must describe and locates the entropy-coded scan that the
//! packetizer puts on the wire.
//!
//! RFC 2435 does not transmit the JPEG headers; the receiver reconstructs them
//! from a handful of payload fields. That makes its constraints hard ones:
//! baseline sequential DCT, YCbCr with 4:2:0 or 4:2:2 chroma sampling, the
//! standard Huffman tables from ITU-T T.81 Annex K, and dimensions of at most
//! 2040 pixels. A file outside those limits would not fail here — it would
//! decode as garbage at the far end — so the probe rejects it with
//! instructions instead.

use std::path::Path;

use crate::domain::media::{CodecParams, JpegParams, MediaSource, Sample, Track, TrackKind};
use crate::domain::ports::MediaProbe;
use crate::domain::{Error, Result};

/// How often the still frame is resent. The image never changes, so this only
/// trades bandwidth (the whole scan goes out each frame) against how promptly
/// a joining player shows the picture; 5 fps matches a modest IP camera.
pub const FRAMES_PER_SECOND: u32 = 5;

/// RTP video clock; RFC 2435 mandates 90 kHz.
const RTP_CLOCK: u32 = 90_000;

/// The payload header stores each dimension in units of 8 pixels in a single
/// byte, so nothing larger than 255 * 8 can be described.
const MAX_DIMENSION: u16 = 2040;

/// The fragment-offset field is 24 bits, so a scan must fit below this.
const MAX_SCAN_BYTES: usize = 1 << 24;

pub struct JpegProbe;

impl MediaProbe for JpegProbe {
    fn probe(&self, path: &Path, name: &str) -> Result<MediaSource> {
        let data = std::fs::read(path)?;
        let parsed = parse(&data)?;

        let frame_ticks = (RTP_CLOCK / FRAMES_PER_SECOND) as u64;
        let track = Track {
            index: 0,
            kind: TrackKind::Video,
            timescale: RTP_CLOCK,
            // One sample lasting one frame interval: the looping session
            // replays it, which is what turns a still into a stream.
            duration: frame_ticks,
            codec: CodecParams::Jpeg(parsed.params),
            samples: vec![Sample {
                offset: parsed.scan_offset as u64,
                size: parsed.scan_len as u32,
                dts: 0,
                pts: 0,
                keyframe: true,
            }],
        };

        Ok(MediaSource {
            name: name.to_string(),
            path: path.to_path_buf(),
            duration_secs: frame_ticks as f64 / RTP_CLOCK as f64,
            tracks: vec![track],
        })
    }
}

/// What the marker walk collects: the codec parameters plus where the
/// entropy-coded scan sits in the file.
#[derive(Debug)]
struct ParsedJpeg {
    params: JpegParams,
    scan_offset: usize,
    scan_len: usize,
}

/// Start-of-frame layout: dimensions plus per-component sampling factors and
/// quantization table ids, in component order (Y, Cb, Cr).
struct FrameHeader {
    width: u16,
    height: u16,
    sampling: [u8; 3],
    quant_ids: [u8; 3],
}

fn parse(data: &[u8]) -> Result<ParsedJpeg> {
    if data.len() < 2 || data[0..2] != [0xff, 0xd8] {
        return Err(Error::MalformedContainer(
            "the file does not start with a JPEG SOI marker".into(),
        ));
    }

    let mut quant: [Option<[u8; 64]>; 4] = [None; 4];
    // Huffman tables keyed by (class, id); class 0 is DC, 1 is AC. Kept as
    // the raw code-length counts followed by the symbols, which is exactly
    // the form the standard tables are declared in below.
    let mut huffman: Vec<((u8, u8), Vec<u8>)> = Vec::new();
    let mut frame: Option<FrameHeader> = None;
    let mut restart_interval: u16 = 0;

    let mut pos = 2usize;
    loop {
        // A marker is 0xff followed by its code; 0xff fill bytes may pad it.
        while data.get(pos) == Some(&0xff) && data.get(pos + 1) == Some(&0xff) {
            pos += 1;
        }
        if data.get(pos) != Some(&0xff) {
            return Err(Error::MalformedContainer(format!(
                "expected a JPEG marker at offset {pos}"
            )));
        }
        let marker = *data.get(pos + 1).ok_or_else(truncated)?;
        pos += 2;

        match marker {
            // Standalone markers with no length field.
            0x01 | 0xd0..=0xd7 => continue,
            0xd8 => continue, // a stray SOI; harmless
            0xd9 => {
                return Err(Error::MalformedContainer(
                    "the JPEG ends before any scan data".into(),
                ))
            }
            _ => {}
        }

        let len = read_u16(data, pos).ok_or_else(truncated)? as usize;
        if len < 2 {
            return Err(Error::MalformedContainer(format!(
                "JPEG segment 0x{marker:02x} declares an impossible length"
            )));
        }
        let body = data.get(pos + 2..pos + len).ok_or_else(truncated)?;
        pos += len;

        match marker {
            0xc0 | 0xc1 => frame = Some(parse_frame_header(body)?),
            0xc2 => {
                return Err(Error::UnsupportedMedia(
                    "this is a progressive JPEG; RTP/JPEG (RFC 2435) carries baseline \
                     only — re-save it as a baseline JPEG"
                        .into(),
                ))
            }
            // The remaining SOF markers: arithmetic coding, lossless,
            // hierarchical. None of them can go on the wire either.
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Err(Error::UnsupportedMedia(format!(
                    "unsupported JPEG coding process (SOF marker 0x{marker:02x}); \
                     RTP/JPEG carries baseline sequential DCT only"
                )))
            }
            0xdb => parse_quant_tables(body, &mut quant)?,
            0xc4 => parse_huffman_tables(body, &mut huffman)?,
            0xdd => {
                restart_interval = read_u16(body, 0).ok_or_else(truncated)?;
            }
            0xda => {
                let frame = frame.ok_or_else(|| {
                    Error::MalformedContainer("the JPEG scan starts before any frame header".into())
                })?;
                let selectors = parse_scan_header(body)?;
                ensure_standard_huffman(&huffman, &selectors)?;

                let scan_offset = pos;
                let scan_len = scan_end(data, scan_offset) - scan_offset;
                if scan_len == 0 {
                    return Err(Error::MalformedContainer("the JPEG scan is empty".into()));
                }
                if scan_len >= MAX_SCAN_BYTES {
                    return Err(Error::UnsupportedMedia(
                        "the JPEG scan exceeds the 16 MB RTP/JPEG fragment-offset range".into(),
                    ));
                }

                let params = assemble_params(&frame, &quant, restart_interval)?;
                return Ok(ParsedJpeg {
                    params,
                    scan_offset,
                    scan_len,
                });
            }
            _ => {} // APPn, COM and friends carry nothing we need
        }
    }
}

fn truncated() -> Error {
    Error::MalformedContainer("the JPEG is truncated".into())
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *data.get(at)?,
        *data.get(at + 1)?,
    ]))
}

fn parse_frame_header(body: &[u8]) -> Result<FrameHeader> {
    let precision = *body.first().ok_or_else(truncated)?;
    if precision != 8 {
        return Err(Error::UnsupportedMedia(format!(
            "{precision}-bit JPEG samples; RTP/JPEG carries 8-bit only"
        )));
    }

    let height = read_u16(body, 1).ok_or_else(truncated)?;
    let width = read_u16(body, 3).ok_or_else(truncated)?;
    if width == 0 || height == 0 {
        return Err(Error::MalformedContainer(
            "the JPEG declares a zero dimension".into(),
        ));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(Error::UnsupportedMedia(format!(
            "the image is {width}x{height}; RTP/JPEG (RFC 2435) caps both dimensions \
             at {MAX_DIMENSION} — scale it down first, e.g. `sips -Z {MAX_DIMENSION} photo.jpg` \
             or `ffmpeg -i photo.jpg -vf scale={MAX_DIMENSION}:-1 smaller.jpg`"
        )));
    }

    let components = *body.get(5).ok_or_else(truncated)? as usize;
    if components != 3 {
        return Err(Error::UnsupportedMedia(format!(
            "the JPEG has {components} colour component(s); RTP/JPEG carries \
             3-component YCbCr only{}",
            if components == 1 {
                " (this file is grayscale)"
            } else {
                ""
            }
        )));
    }

    let mut sampling = [0u8; 3];
    let mut quant_ids = [0u8; 3];
    for i in 0..3 {
        sampling[i] = *body.get(7 + 3 * i).ok_or_else(truncated)?;
        quant_ids[i] = *body.get(8 + 3 * i).ok_or_else(truncated)?;
    }

    Ok(FrameHeader {
        width,
        height,
        sampling,
        quant_ids,
    })
}

/// A DQT segment holds one or more tables, each led by a precision/id byte.
fn parse_quant_tables(body: &[u8], quant: &mut [Option<[u8; 64]>; 4]) -> Result<()> {
    let mut at = 0usize;
    while at < body.len() {
        let pq_tq = body[at];
        if pq_tq >> 4 != 0 {
            return Err(Error::UnsupportedMedia(
                "16-bit quantization tables; RTP/JPEG carries 8-bit tables only".into(),
            ));
        }
        let id = (pq_tq & 0x0f) as usize;
        if id >= quant.len() {
            return Err(Error::MalformedContainer(format!(
                "JPEG quantization table id {id} is out of range"
            )));
        }
        let table = body.get(at + 1..at + 65).ok_or_else(truncated)?;
        quant[id] = Some(table.try_into().expect("a 64-byte slice window"));
        at += 65;
    }
    Ok(())
}

/// A DHT segment holds one or more tables: a class/id byte, sixteen
/// code-length counts, then that many symbols.
fn parse_huffman_tables(body: &[u8], huffman: &mut Vec<((u8, u8), Vec<u8>)>) -> Result<()> {
    let mut at = 0usize;
    while at < body.len() {
        let tc_th = body[at];
        let key = (tc_th >> 4, tc_th & 0x0f);
        let counts = body.get(at + 1..at + 17).ok_or_else(truncated)?;
        let symbols: usize = counts.iter().map(|&c| c as usize).sum();
        let table = body.get(at + 1..at + 17 + symbols).ok_or_else(truncated)?;
        huffman.retain(|(k, _)| *k != key);
        huffman.push((key, table.to_vec()));
        at += 17 + symbols;
    }
    Ok(())
}

/// The Huffman table each component's scan selects: (DC id, AC id) for Y,
/// then for Cb/Cr, which must agree with each other.
struct ScanSelectors {
    luma: (u8, u8),
    chroma: (u8, u8),
}

fn parse_scan_header(body: &[u8]) -> Result<ScanSelectors> {
    let components = *body.first().ok_or_else(truncated)? as usize;
    if components != 3 {
        return Err(Error::UnsupportedMedia(format!(
            "the JPEG scan interleaves {components} component(s); RTP/JPEG needs \
             a single 3-component scan"
        )));
    }

    let mut selectors = [(0u8, 0u8); 3];
    for (i, slot) in selectors.iter_mut().enumerate() {
        let td_ta = *body.get(2 + 2 * i).ok_or_else(truncated)?;
        *slot = (td_ta >> 4, td_ta & 0x0f);
    }
    if selectors[1] != selectors[2] {
        return Err(Error::UnsupportedMedia(
            "the JPEG chroma components use different Huffman tables".into(),
        ));
    }

    Ok(ScanSelectors {
        luma: selectors[0],
        chroma: selectors[1],
    })
}

/// Walks the entropy-coded data to the next real marker. Inside a scan, 0xff
/// is either stuffed (followed by 0x00) or a restart marker; anything else
/// ends the scan — EOI, in a well-formed file.
fn scan_end(data: &[u8], start: usize) -> usize {
    let mut i = start;
    while i + 1 < data.len() {
        if data[i] != 0xff {
            i += 1;
        } else {
            match data[i + 1] {
                0x00 | 0xd0..=0xd7 => i += 2,
                _ => return i,
            }
        }
    }
    data.len()
}

fn assemble_params(
    frame: &FrameHeader,
    quant: &[Option<[u8; 64]>; 4],
    restart_interval: u16,
) -> Result<JpegParams> {
    if frame.sampling[1] != 0x11 || frame.sampling[2] != 0x11 {
        return Err(unsupported_sampling(frame));
    }
    let type_code = match frame.sampling[0] {
        0x21 => 0, // 4:2:2
        0x22 => 1, // 4:2:0
        _ => return Err(unsupported_sampling(frame)),
    };

    if frame.quant_ids[1] != frame.quant_ids[2] {
        return Err(Error::UnsupportedMedia(
            "the JPEG chroma components use different quantization tables".into(),
        ));
    }
    let table = |id: u8| -> Result<[u8; 64]> {
        quant[id as usize].ok_or_else(|| {
            Error::MalformedContainer(format!("the JPEG never defines quantization table {id}"))
        })
    };

    // Luma first, then chroma — unless the scan shares one table, in which
    // case it is sent once and the receiver reuses it (RFC 2435 derives the
    // table count from the length field).
    let mut quant_tables = Vec::with_capacity(128);
    quant_tables.extend_from_slice(&table(frame.quant_ids[0])?);
    if frame.quant_ids[1] != frame.quant_ids[0] {
        quant_tables.extend_from_slice(&table(frame.quant_ids[1])?);
    }

    Ok(JpegParams {
        type_code,
        width: frame.width,
        height: frame.height,
        restart_interval,
        quant_tables,
    })
}

fn unsupported_sampling(frame: &FrameHeader) -> Error {
    let describe = |s: u8| format!("{}x{}", s >> 4, s & 0x0f);
    Error::UnsupportedMedia(format!(
        "chroma sampling {} {} {} ; RTP/JPEG (RFC 2435) carries YCbCr 4:2:0 or \
         4:2:2 only — re-encode it, e.g. `ffmpeg -i photo.jpg -pix_fmt yuvj420p out.jpg`",
        describe(frame.sampling[0]),
        describe(frame.sampling[1]),
        describe(frame.sampling[2]),
    ))
}

// ---- standard Huffman tables ------------------------------------------------
//
// ITU-T T.81 Annex K, in DHT form: sixteen code-length counts followed by the
// symbols. RFC 2435 receivers rebuild exactly these, so a scan encoded with
// any other tables would decode to garbage — the probe refuses such files.

#[rustfmt::skip]
const DC_LUMA: [u8; 28] = [
    0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0,
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,
];

#[rustfmt::skip]
const DC_CHROMA: [u8; 28] = [
    0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0,
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,
];

#[rustfmt::skip]
const AC_LUMA: [u8; 178] = [
    0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d,
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06,
    0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08,
    0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
    0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
    0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
    0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75,
    0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3,
    0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
    0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9,
    0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4,
    0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

#[rustfmt::skip]
const AC_CHROMA: [u8; 178] = [
    0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77,
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41,
    0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91,
    0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1,
    0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
    0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
    0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74,
    0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
    0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
    0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7,
    0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4,
    0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

/// A file with no DHT at all is accepted: an abbreviated stream implies the
/// standard tables, which is exactly what the receiver assumes anyway.
fn ensure_standard_huffman(
    huffman: &[((u8, u8), Vec<u8>)],
    selectors: &ScanSelectors,
) -> Result<()> {
    if huffman.is_empty() {
        return Ok(());
    }

    let lookup = |key: (u8, u8)| -> Result<&[u8]> {
        huffman
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, table)| table.as_slice())
            .ok_or_else(|| {
                Error::MalformedContainer(format!(
                    "the JPEG scan selects Huffman table (class {}, id {}) that is never defined",
                    key.0, key.1
                ))
            })
    };

    let expectations: [((u8, u8), &[u8]); 4] = [
        ((0, selectors.luma.0), &DC_LUMA),
        ((1, selectors.luma.1), &AC_LUMA),
        ((0, selectors.chroma.0), &DC_CHROMA),
        ((1, selectors.chroma.1), &AC_CHROMA),
    ];
    for (key, standard) in expectations {
        if lookup(key)? != standard {
            return Err(Error::UnsupportedMedia(
                "the JPEG uses customized (\"optimized\") Huffman tables, which RTP/JPEG \
                 receivers cannot decode; re-save it with the standard tables, e.g. \
                 `ffmpeg -i photo.jpg -huffman default out.jpg`"
                    .into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![0xff, marker];
        out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    fn quant_segment(id: u8, fill: u8) -> Vec<u8> {
        let mut body = vec![id];
        body.extend_from_slice(&[fill; 64]);
        segment(0xdb, &body)
    }

    /// SOF0 for a 3-component image; `luma_sampling` packs h<<4|v.
    fn frame_segment(width: u16, height: u16, luma_sampling: u8, chroma_quant: u8) -> Vec<u8> {
        let mut body = vec![8];
        body.extend_from_slice(&height.to_be_bytes());
        body.extend_from_slice(&width.to_be_bytes());
        body.push(3);
        body.extend_from_slice(&[1, luma_sampling, 0]);
        body.extend_from_slice(&[2, 0x11, chroma_quant]);
        body.extend_from_slice(&[3, 0x11, chroma_quant]);
        segment(0xc0, &body)
    }

    fn scan_segment() -> Vec<u8> {
        // Y selects DC0/AC0, chroma DC1/AC1 — the conventional layout.
        segment(0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0])
    }

    fn huffman_segment(class: u8, id: u8, table: &[u8]) -> Vec<u8> {
        let mut body = vec![class << 4 | id];
        body.extend_from_slice(table);
        segment(0xc4, &body)
    }

    /// A structurally valid baseline JPEG around `scan`; without DHT segments
    /// unless the test adds them, which the probe reads as "standard tables".
    fn build(width: u16, height: u16, luma_sampling: u8, chroma_quant: u8, scan: &[u8]) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8];
        out.extend(segment(0xe0, b"JFIF\0"));
        out.extend(quant_segment(0, 16));
        out.extend(quant_segment(1, 17));
        out.extend(frame_segment(width, height, luma_sampling, chroma_quant));
        out.extend(scan_segment());
        out.extend_from_slice(scan);
        out.extend_from_slice(&[0xff, 0xd9]);
        out
    }

    #[test]
    fn parses_dimensions_sampling_and_tables() {
        let scan = [1u8, 2, 3, 4, 5];
        let jpeg = build(640, 480, 0x22, 1, &scan);
        let parsed = parse(&jpeg).unwrap();

        assert_eq!(parsed.params.width, 640);
        assert_eq!(parsed.params.height, 480);
        assert_eq!(parsed.params.type_code, 1, "2x2 sampling is 4:2:0");
        assert_eq!(parsed.params.restart_interval, 0);
        assert_eq!(parsed.params.quant_tables.len(), 128, "two distinct tables");
        assert!(parsed.params.quant_tables[..64].iter().all(|&b| b == 16));
        assert!(parsed.params.quant_tables[64..].iter().all(|&b| b == 17));
        assert_eq!(
            &jpeg[parsed.scan_offset..parsed.scan_offset + parsed.scan_len],
            &scan
        );
    }

    #[test]
    fn horizontal_only_subsampling_is_type_zero() {
        let parsed = parse(&build(64, 48, 0x21, 1, &[9; 4])).unwrap();
        assert_eq!(parsed.params.type_code, 0, "2x1 sampling is 4:2:2");
    }

    #[test]
    fn a_shared_quant_table_is_sent_once() {
        let parsed = parse(&build(64, 48, 0x22, 0, &[9; 4])).unwrap();
        assert_eq!(parsed.params.quant_tables.len(), 64);
    }

    #[test]
    fn the_scan_runs_through_stuffed_bytes_and_restart_markers() {
        let scan = [0x12, 0xff, 0x00, 0x34, 0xff, 0xd0, 0x56];
        let jpeg = build(64, 48, 0x22, 1, &scan);
        let parsed = parse(&jpeg).unwrap();
        assert_eq!(
            &jpeg[parsed.scan_offset..parsed.scan_offset + parsed.scan_len],
            &scan
        );
    }

    #[test]
    fn a_restart_interval_is_carried_through() {
        let mut jpeg = build(64, 48, 0x22, 1, &[9; 4]);
        // Splice a DRI segment in ahead of the SOS.
        let sos_at = jpeg
            .windows(2)
            .position(|w| w == [0xff, 0xda])
            .expect("the SOS marker");
        let dri = segment(0xdd, &8u16.to_be_bytes());
        jpeg.splice(sos_at..sos_at, dri);

        let parsed = parse(&jpeg).unwrap();
        assert_eq!(parsed.params.restart_interval, 8);
    }

    #[test]
    fn progressive_files_are_rejected_with_advice() {
        let mut jpeg = build(64, 48, 0x22, 1, &[9; 4]);
        let sof_at = jpeg
            .windows(2)
            .position(|w| w == [0xff, 0xc0])
            .expect("the SOF marker");
        jpeg[sof_at + 1] = 0xc2;

        let err = parse(&jpeg).unwrap_err().to_string();
        assert!(err.contains("progressive"), "got: {err}");
    }

    #[test]
    fn oversized_dimensions_are_rejected_with_advice() {
        let err = parse(&build(2048, 480, 0x22, 1, &[9; 4]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("2040"), "got: {err}");
    }

    #[test]
    fn unsupported_sampling_is_rejected() {
        // 1x1 luma sampling would be 4:4:4.
        let err = parse(&build(64, 48, 0x11, 1, &[9; 4]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("4:2:0"), "got: {err}");
    }

    #[test]
    fn grayscale_is_rejected() {
        let mut out = vec![0xff, 0xd8];
        out.extend(quant_segment(0, 16));
        let mut body = vec![8];
        body.extend_from_slice(&48u16.to_be_bytes());
        body.extend_from_slice(&64u16.to_be_bytes());
        body.extend_from_slice(&[1, 0x11, 0]);
        out.extend(segment(0xc0, &body));
        out.extend(segment(0xda, &[1, 1, 0x00, 0, 63, 0]));
        out.extend_from_slice(&[9, 9, 0xff, 0xd9]);

        let err = parse(&out).unwrap_err().to_string();
        assert!(err.contains("grayscale"), "got: {err}");
    }

    #[test]
    fn sixteen_bit_quant_tables_are_rejected() {
        let mut jpeg = vec![0xff, 0xd8];
        let mut body = vec![0x10]; // Pq=1: 16-bit
        body.extend_from_slice(&[0; 128]);
        jpeg.extend(segment(0xdb, &body));

        let err = parse(&jpeg).unwrap_err().to_string();
        assert!(err.contains("16-bit"), "got: {err}");
    }

    #[test]
    fn the_standard_huffman_tables_are_accepted() {
        let mut jpeg = build(64, 48, 0x22, 1, &[9; 4]);
        let sos_at = jpeg.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
        let mut tables = huffman_segment(0, 0, &DC_LUMA);
        tables.extend(huffman_segment(1, 0, &AC_LUMA));
        tables.extend(huffman_segment(0, 1, &DC_CHROMA));
        tables.extend(huffman_segment(1, 1, &AC_CHROMA));
        jpeg.splice(sos_at..sos_at, tables);

        assert!(parse(&jpeg).is_ok());
    }

    #[test]
    fn customized_huffman_tables_are_rejected_with_advice() {
        let mut altered = AC_LUMA.to_vec();
        *altered.last_mut().unwrap() ^= 1;

        let mut jpeg = build(64, 48, 0x22, 1, &[9; 4]);
        let sos_at = jpeg.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
        let mut tables = huffman_segment(0, 0, &DC_LUMA);
        tables.extend(huffman_segment(1, 0, &altered));
        tables.extend(huffman_segment(0, 1, &DC_CHROMA));
        tables.extend(huffman_segment(1, 1, &AC_CHROMA));
        jpeg.splice(sos_at..sos_at, tables);

        let err = parse(&jpeg).unwrap_err().to_string();
        assert!(err.contains("Huffman"), "got: {err}");
    }

    #[test]
    fn the_standard_tables_have_the_declared_shape() {
        for table in [&DC_LUMA[..], &DC_CHROMA[..], &AC_LUMA[..], &AC_CHROMA[..]] {
            let symbols: usize = table[..16].iter().map(|&c| c as usize).sum();
            assert_eq!(
                table.len(),
                16 + symbols,
                "code-length counts must match the symbol count"
            );
        }
    }

    #[test]
    fn probing_a_file_yields_one_looping_video_track() {
        let jpeg = build(640, 480, 0x22, 1, &[7; 100]);
        let path = std::env::temp_dir().join(format!("rtsp-utils-probe-{}.jpg", std::process::id()));
        std::fs::write(&path, &jpeg).unwrap();

        let source = JpegProbe.probe(&path, "photo").unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(source.tracks.len(), 1);
        let track = &source.tracks[0];
        assert_eq!(track.kind, TrackKind::Video);
        assert_eq!(track.timescale, 90_000);
        assert_eq!(track.samples.len(), 1, "one frame, replayed by the loop");
        assert!(track.samples[0].keyframe);
        let fps = track.samples.len() as f64 / track.duration_secs();
        assert!((fps - FRAMES_PER_SECOND as f64).abs() < 0.01);
    }
}
