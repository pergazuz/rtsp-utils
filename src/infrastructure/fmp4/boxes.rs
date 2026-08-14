//! Box construction for fragmented MP4.

use super::VideoParams;
use crate::domain::media::{H264Params, H265Params};

/// Wraps a payload in a box header.
fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// A FullBox: a box whose payload starts with a version and 24-bit flags.
fn full_bx(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![version];
    payload.extend_from_slice(&flags.to_be_bytes()[1..]);
    payload.extend_from_slice(body);
    bx(kind, &payload)
}

/// The identity transform, as a 3x3 fixed-point matrix.
const UNITY_MATRIX: [u8; 36] = [
    0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x00, 0x00, 0x00,
];

pub fn ftyp() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"iso5");
    body.extend_from_slice(&512u32.to_be_bytes());
    for brand in [b"iso5", b"iso6", b"mp41", b"avc1"] {
        body.extend_from_slice(brand);
    }
    bx(b"ftyp", &body)
}

/// The initialisation segment's movie box. Durations are zero because the
/// stream is open-ended.
pub fn moov(params: &VideoParams, timescale: u32, track_id: u32) -> Vec<u8> {
    let mut body = mvhd(track_id);
    body.extend_from_slice(&trak(params, timescale, track_id));
    body.extend_from_slice(&mvex(track_id));
    bx(b"moov", &body)
}

fn mvhd(track_id: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // creation time
    body.extend_from_slice(&0u32.to_be_bytes()); // modification time
    body.extend_from_slice(&1000u32.to_be_bytes()); // movie timescale
    body.extend_from_slice(&0u32.to_be_bytes()); // duration: unknown
    body.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    body.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    body.extend_from_slice(&[0; 2]); // reserved
    body.extend_from_slice(&[0; 8]); // reserved
    body.extend_from_slice(&UNITY_MATRIX);
    body.extend_from_slice(&[0; 24]); // pre_defined
    body.extend_from_slice(&(track_id + 1).to_be_bytes()); // next_track_ID
    full_bx(b"mvhd", 0, 0, &body)
}

fn trak(params: &VideoParams, timescale: u32, track_id: u32) -> Vec<u8> {
    let mut body = tkhd(params, track_id);
    body.extend_from_slice(&mdia(params, timescale));
    bx(b"trak", &body)
}

fn tkhd(params: &VideoParams, track_id: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // creation
    body.extend_from_slice(&0u32.to_be_bytes()); // modification
    body.extend_from_slice(&track_id.to_be_bytes());
    body.extend_from_slice(&[0; 4]); // reserved
    body.extend_from_slice(&0u32.to_be_bytes()); // duration: unknown
    body.extend_from_slice(&[0; 8]); // reserved
    body.extend_from_slice(&0u16.to_be_bytes()); // layer
    body.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
    body.extend_from_slice(&0u16.to_be_bytes()); // volume: video is silent
    body.extend_from_slice(&[0; 2]); // reserved
    body.extend_from_slice(&UNITY_MATRIX);
    // Dimensions here are 16.16 fixed point.
    body.extend_from_slice(&((params.width() as u32) << 16).to_be_bytes());
    body.extend_from_slice(&((params.height() as u32) << 16).to_be_bytes());
    // flags 3 = track_enabled | track_in_movie
    full_bx(b"tkhd", 0, 3, &body)
}

fn mdia(params: &VideoParams, timescale: u32) -> Vec<u8> {
    let mut body = mdhd(timescale);
    body.extend_from_slice(&hdlr());
    body.extend_from_slice(&minf(params));
    bx(b"mdia", &body)
}

fn mdhd(timescale: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // creation
    body.extend_from_slice(&0u32.to_be_bytes()); // modification
    body.extend_from_slice(&timescale.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes()); // duration: unknown
    body.extend_from_slice(&0x55c4u16.to_be_bytes()); // language 'und'
    body.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    full_bx(b"mdhd", 0, 0, &body)
}

fn hdlr() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&[0; 4]); // pre_defined
    body.extend_from_slice(b"vide");
    body.extend_from_slice(&[0; 12]); // reserved
    body.extend_from_slice(b"VideoHandler\0");
    full_bx(b"hdlr", 0, 0, &body)
}

fn minf(params: &VideoParams) -> Vec<u8> {
    let mut body = vmhd();
    body.extend_from_slice(&dinf());
    body.extend_from_slice(&stbl(params));
    bx(b"minf", &body)
}

fn vmhd() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u16.to_be_bytes()); // graphicsmode
    body.extend_from_slice(&[0; 6]); // opcolor
    full_bx(b"vmhd", 0, 1, &body)
}

fn dinf() -> Vec<u8> {
    // A 'url ' entry with flag 1 means the media lives in this same file.
    let url = full_bx(b"url ", 0, 1, &[]);
    let mut dref_body = 1u32.to_be_bytes().to_vec();
    dref_body.extend_from_slice(&url);
    bx(b"dinf", &full_bx(b"dref", 0, 0, &dref_body))
}

/// The sample tables are empty: a fragmented file carries timing in `trun`.
fn stbl(params: &VideoParams) -> Vec<u8> {
    let mut stsd_body = 1u32.to_be_bytes().to_vec();
    stsd_body.extend_from_slice(&sample_entry(params));

    let mut body = full_bx(b"stsd", 0, 0, &stsd_body);
    body.extend_from_slice(&full_bx(b"stts", 0, 0, &0u32.to_be_bytes()));
    body.extend_from_slice(&full_bx(b"stsc", 0, 0, &0u32.to_be_bytes()));

    let mut stsz_body = 0u32.to_be_bytes().to_vec(); // uniform sample size
    stsz_body.extend_from_slice(&0u32.to_be_bytes()); // sample count
    body.extend_from_slice(&full_bx(b"stsz", 0, 0, &stsz_body));
    body.extend_from_slice(&full_bx(b"stco", 0, 0, &0u32.to_be_bytes()));

    bx(b"stbl", &body)
}

fn sample_entry(params: &VideoParams) -> Vec<u8> {
    match params {
        VideoParams::H264(p) => visual_sample_entry(b"avc1", p.width, p.height, &avcc(p)),
        VideoParams::H265(p) => visual_sample_entry(b"hvc1", p.width, p.height, &hvcc(p)),
    }
}

/// The fixed VisualSampleEntry preamble every video codec shares, followed by
/// that codec's configuration box.
fn visual_sample_entry(kind: &[u8; 4], width: u16, height: u16, config: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&[0; 6]); // reserved
    body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    body.extend_from_slice(&[0; 2]); // pre_defined
    body.extend_from_slice(&[0; 2]); // reserved
    body.extend_from_slice(&[0; 12]); // pre_defined
    body.extend_from_slice(&width.to_be_bytes());
    body.extend_from_slice(&height.to_be_bytes());
    body.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi horizontal
    body.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi vertical
    body.extend_from_slice(&[0; 4]); // reserved
    body.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    body.extend_from_slice(&[0; 32]); // compressorname
    body.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
    body.extend_from_slice(&0xffffu16.to_be_bytes()); // pre_defined
    body.extend_from_slice(config);
    bx(kind, &body)
}

/// Rebuilds the AVCDecoderConfigurationRecord from the parameter sets.
fn avcc(params: &H264Params) -> Vec<u8> {
    let profile = params.sps.get(1).copied().unwrap_or(0x42);
    let compatibility = params.sps.get(2).copied().unwrap_or(0);
    let level = params.sps.get(3).copied().unwrap_or(0x1e);

    let mut body = vec![
        1, // configurationVersion
        profile,
        compatibility,
        level,
        // Six reserved bits then lengthSizeMinusOne.
        0xfc | ((params.nal_length_size as u8).saturating_sub(1) & 0x03),
        0xe1, // three reserved bits then one SPS
    ];
    body.extend_from_slice(&(params.sps.len() as u16).to_be_bytes());
    body.extend_from_slice(&params.sps);
    body.push(1); // one PPS
    body.extend_from_slice(&(params.pps.len() as u16).to_be_bytes());
    body.extend_from_slice(&params.pps);

    bx(b"avcC", &body)
}

/// Rebuilds the HEVCDecoderConfigurationRecord: the 22-byte fixed head is
/// copied from the source file's record (it holds profile/tier/level fields
/// only an SPS bitstream parser could re-derive), then the parameter-set
/// arrays are rebuilt from the sets we extracted, marked complete because the
/// stream repeats nothing the record lacks.
fn hvcc(params: &H265Params) -> Vec<u8> {
    let mut body: Vec<u8> = params.config.iter().copied().take(22).collect();
    body.resize(22, 0);

    body.push(3); // numOfArrays: one per parameter set kind
    for (nal_type, unit) in [(32u8, &params.vps), (33, &params.sps), (34, &params.pps)] {
        body.push(0x80 | nal_type); // array_completeness + NAL unit type
        body.extend_from_slice(&1u16.to_be_bytes()); // one NAL unit
        body.extend_from_slice(&(unit.len() as u16).to_be_bytes());
        body.extend_from_slice(unit);
    }
    bx(b"hvcC", &body)
}

fn mvex(track_id: u32) -> Vec<u8> {
    let mut trex_body = track_id.to_be_bytes().to_vec();
    trex_body.extend_from_slice(&1u32.to_be_bytes()); // sample description index
    trex_body.extend_from_slice(&0u32.to_be_bytes()); // default duration
    trex_body.extend_from_slice(&0u32.to_be_bytes()); // default size
    trex_body.extend_from_slice(&0u32.to_be_bytes()); // default flags
    bx(b"mvex", &full_bx(b"trex", 0, 0, &trex_body))
}

pub struct TrunSample {
    pub duration: u32,
    pub size: u32,
    pub flags: u32,
    pub composition_offset: i32,
}

/// Builds a movie fragment box. The `trun` data offset is patched once the
/// final size of the box is known.
pub fn moof(
    sequence: u32,
    track_id: u32,
    base_decode_time: u64,
    samples: &[TrunSample],
) -> Vec<u8> {
    let mfhd = full_bx(b"mfhd", 0, 0, &sequence.to_be_bytes());

    // 0x020000 = default-base-is-moof, so offsets are relative to this box.
    let tfhd = full_bx(b"tfhd", 0, 0x020000, &track_id.to_be_bytes());
    let tfdt = full_bx(b"tfdt", 1, 0, &base_decode_time.to_be_bytes());

    // Everything is carried per sample: duration, size, flags and the
    // composition offset.
    const TRUN_FLAGS: u32 = 0x0001 | 0x0100 | 0x0200 | 0x0400 | 0x0800;
    let mut trun_body = (samples.len() as u32).to_be_bytes().to_vec();
    let data_offset_at = trun_body.len();
    trun_body.extend_from_slice(&0i32.to_be_bytes()); // patched below
    for sample in samples {
        trun_body.extend_from_slice(&sample.duration.to_be_bytes());
        trun_body.extend_from_slice(&sample.size.to_be_bytes());
        trun_body.extend_from_slice(&sample.flags.to_be_bytes());
        trun_body.extend_from_slice(&sample.composition_offset.to_be_bytes());
    }
    let trun = full_bx(b"trun", 1, TRUN_FLAGS, &trun_body);

    let mut traf_body = tfhd;
    traf_body.extend_from_slice(&tfdt);
    let trun_at = traf_body.len();
    traf_body.extend_from_slice(&trun);
    let traf = bx(b"traf", &traf_body);

    let mut moof_body = mfhd;
    let traf_at = moof_body.len();
    moof_body.extend_from_slice(&traf);
    let mut moof = bx(b"moof", &moof_body);

    // Sample data starts right after the moof box and the mdat header.
    let data_offset = (moof.len() + 8) as i32;
    // Walk down to the field: the moof header (8), the traf header (8), the
    // trun box header plus its version and flags (12), and the offsets of each
    // box within its parent.
    let patch_at = 8 + traf_at + 8 + trun_at + 12 + data_offset_at;
    moof[patch_at..patch_at + 4].copy_from_slice(&data_offset.to_be_bytes());

    moof
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> H264Params {
        H264Params {
            sps: vec![0x67, 0x4d, 0x00, 0x1f, 0xaa],
            pps: vec![0x68, 0xce, 0x3c, 0x80],
            nal_length_size: 4,
            width: 1280,
            height: 720,
        }
    }

    /// Navigates with the project's own atom reader, so what these tests
    /// assert is that a real MP4 parser can find its way around the output.
    use crate::infrastructure::mp4::atom;

    /// Byte width of the fixed preamble in a VisualSampleEntry, before the
    /// child boxes begin.
    const VISUAL_SAMPLE_ENTRY_LEN: usize = 78;

    fn h265_params() -> H265Params {
        // A plausible fixed head: version 1, Main profile, level 150, then
        // filler up to lengthSizeMinusOne = 3 at byte 21.
        let mut config = vec![0u8; 23];
        config[0] = 1;
        config[1] = 0x01;
        config[12] = 150;
        config[21] = 0xf0 | 3;
        H265Params {
            config,
            vps: vec![0x40, 0x01, 0x0c],
            sps: vec![0x42, 0x01, 0x01, 0x22],
            pps: vec![0x44, 0x01, 0xc0],
            nal_length_size: 4,
            width: 2560,
            height: 1920,
        }
    }

    fn init_segment() -> Vec<u8> {
        let mut init = ftyp();
        init.extend_from_slice(&moov(&VideoParams::H264(params()), 90_000, 1));
        init
    }

    #[test]
    fn the_init_segment_is_ftyp_then_moov_and_nothing_else() {
        let init = init_segment();
        let kinds: Vec<String> = atom::children(&init)
            .map(|a| String::from_utf8_lossy(&a.kind).into_owned())
            .collect();

        assert_eq!(kinds, vec!["ftyp", "moov"]);
        // Every byte must be accounted for by a box, or a parser will choke.
        let covered: usize = atom::children(&init).map(|a| a.body.len() + 8).sum();
        assert_eq!(covered, init.len());
    }

    #[test]
    fn moov_declares_the_track_timescale() {
        let init = init_segment();
        let moov = atom::find(&init, b"moov").expect("moov");
        let trak = atom::find(moov, b"trak").expect("trak");
        let mdhd = atom::find_path(trak, &[b"mdia", b"mdhd"]).expect("mdhd");

        // Timescale follows version/flags and the two timestamps.
        assert_eq!(atom::u32_at(mdhd, 12).unwrap(), 90_000);
        // A fragmented file must announce itself with mvex/trex.
        assert!(atom::find_path(moov, &[b"mvex", b"trex"]).is_some());
    }

    #[test]
    fn the_parameter_sets_survive_a_round_trip_through_the_container() {
        let init = init_segment();
        let moov = atom::find(&init, b"moov").expect("moov");
        let trak = atom::find(moov, b"trak").expect("trak");
        let stbl = atom::find_path(trak, &[b"mdia", b"minf", b"stbl"]).expect("stbl");
        let stsd = atom::find(stbl, b"stsd").expect("stsd");

        // stsd is a FullBox holding an entry count, then the entries.
        assert_eq!(atom::u32_at(stsd, 4).unwrap(), 1, "one sample entry");
        let entry = atom::children(&stsd[8..]).next().expect("a sample entry");
        assert_eq!(&entry.kind, b"avc1");

        let avcc = atom::find(&entry.body[VISUAL_SAMPLE_ENTRY_LEN..], b"avcC").expect("avcC");
        assert_eq!(avcc[0], 1, "configurationVersion");
        assert_eq!(&avcc[1..4], &[0x4d, 0x00, 0x1f], "profile/compat/level");
        assert_eq!(avcc[4] & 0x03, 3, "four-byte NAL length prefixes");

        // The SPS and PPS must come back byte for byte, or no decoder starts.
        let original = params();
        let sps_len = atom::u16_at(avcc, 6).unwrap() as usize;
        assert_eq!(&avcc[8..8 + sps_len], &original.sps[..]);
        let pps_at = 8 + sps_len + 1;
        let pps_len = atom::u16_at(avcc, pps_at).unwrap() as usize;
        assert_eq!(&avcc[pps_at + 2..pps_at + 2 + pps_len], &original.pps[..]);

        // Dimensions are declared on the sample entry too.
        assert_eq!(atom::u16_at(entry.body, 24).unwrap(), 1280);
        assert_eq!(atom::u16_at(entry.body, 26).unwrap(), 720);
    }

    #[test]
    fn a_fragment_points_at_its_own_media_data() {
        let samples = vec![
            TrunSample {
                duration: 1000,
                size: 40,
                flags: 0x0200_0000,
                composition_offset: 0,
            },
            TrunSample {
                duration: 1000,
                size: 60,
                flags: 0x0101_0000,
                composition_offset: 500,
            },
        ];
        let fragment = moof(7, 1, 12_000, &samples);

        let body = atom::find(&fragment, b"moof").expect("moof");
        let traf = atom::find(body, b"traf").expect("traf");
        let trun = atom::find(traf, b"trun").expect("trun");

        assert_eq!(atom::u32_at(trun, 4).unwrap(), 2, "sample count");

        // The data offset must land exactly on the first payload byte, which
        // is the whole moof box followed by an 8-byte mdat header.
        assert_eq!(
            atom::i32_at(trun, 8).unwrap() as usize,
            fragment.len() + 8,
            "trun data offset must point at the first byte of mdat payload"
        );

        let tfdt = atom::find(traf, b"tfdt").expect("tfdt");
        assert_eq!(atom::u64_at(tfdt, 4).unwrap(), 12_000, "base decode time");

        let mfhd = atom::find(body, b"mfhd").expect("mfhd");
        assert_eq!(atom::u32_at(mfhd, 4).unwrap(), 7, "sequence number");
    }

    #[test]
    fn fragments_advance_their_decode_time_and_sequence() {
        use crate::domain::ports::{FragmentSample, MediaFragmenter};

        let mut fragmenter =
            super::super::Fmp4Fragmenter::new(VideoParams::H264(params()), 90_000);
        let mut decode_times = Vec::new();
        let mut sequences = Vec::new();

        for _ in 0..3 {
            for _ in 0..2 {
                fragmenter.push(FragmentSample {
                    data: &[0u8; 32],
                    duration: 1000,
                    composition_offset: 0,
                    keyframe: true,
                });
            }
            let fragment = fragmenter.flush().expect("a fragment");
            let body = atom::find(&fragment, b"moof").expect("moof");
            let traf = atom::find(body, b"traf").expect("traf");
            let tfdt = atom::find(traf, b"tfdt").expect("tfdt");
            let mfhd = atom::find(body, b"mfhd").expect("mfhd");

            decode_times.push(atom::u64_at(tfdt, 4).unwrap());
            sequences.push(atom::u32_at(mfhd, 4).unwrap());

            // mdat has to follow moof and hold exactly the pushed bytes.
            let mdat = atom::children(&fragment)
                .find(|a| &a.kind == b"mdat")
                .expect("mdat");
            assert_eq!(mdat.body.len(), 64);
        }

        // Two 1000-tick samples per fragment.
        assert_eq!(decode_times, vec![0, 2000, 4000]);
        assert_eq!(sequences, vec![1, 2, 3]);
        assert!(fragmenter.flush().is_none(), "nothing left to flush");
    }

    #[test]
    fn codec_strings_come_from_the_sps() {
        assert_eq!(params().codec_string(), "avc1.4d001f");
    }

    #[test]
    fn hevc_parameter_sets_survive_a_round_trip_through_the_container() {
        let original = h265_params();
        let mut init = ftyp();
        init.extend_from_slice(&moov(&VideoParams::H265(original.clone()), 90_000, 1));

        let moov = atom::find(&init, b"moov").expect("moov");
        let trak = atom::find(moov, b"trak").expect("trak");
        let stbl = atom::find_path(trak, &[b"mdia", b"minf", b"stbl"]).expect("stbl");
        let stsd = atom::find(stbl, b"stsd").expect("stsd");

        let entry = atom::children(&stsd[8..]).next().expect("a sample entry");
        assert_eq!(&entry.kind, b"hvc1");
        assert_eq!(atom::u16_at(entry.body, 24).unwrap(), 2560);
        assert_eq!(atom::u16_at(entry.body, 26).unwrap(), 1920);

        let hvcc = atom::find(&entry.body[VISUAL_SAMPLE_ENTRY_LEN..], b"hvcC").expect("hvcC");
        // The fixed head must come through from the source record.
        assert_eq!(&hvcc[..22], &original.config[..22]);
        assert_eq!(hvcc[21] & 0x03, 3, "four-byte NAL length prefixes");
        assert_eq!(hvcc[22], 3, "one array per parameter set kind");

        // Each array: completeness + type, a count of one, then the set.
        let mut pos = 23usize;
        for (nal_type, unit) in [(32u8, &original.vps), (33, &original.sps), (34, &original.pps)]
        {
            assert_eq!(hvcc[pos], 0x80 | nal_type);
            assert_eq!(atom::u16_at(hvcc, pos + 1).unwrap(), 1);
            let len = atom::u16_at(hvcc, pos + 3).unwrap() as usize;
            assert_eq!(&hvcc[pos + 5..pos + 5 + len], &unit[..]);
            pos += 5 + len;
        }
        assert_eq!(pos, hvcc.len(), "nothing after the last array");
    }

    #[test]
    fn hevc_codec_strings_come_from_the_configuration_record() {
        let mut params = h265_params();
        // Main profile: compatibility flags 0x60000000 reverse to 6; one
        // constraint byte set, the rest of the tail dropped.
        params.config[2] = 0x60;
        params.config[6] = 0xb0;
        assert_eq!(params.codec_string(), "hvc1.1.6.L150.B0");
    }
}
