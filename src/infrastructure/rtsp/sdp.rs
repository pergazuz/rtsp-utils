//! Session description generation for DESCRIBE (RFC 4566, plus the payload
//! specific attributes from RFC 6184 and RFC 3640).

use crate::domain::media::{AacParams, CodecParams, H264Params, MediaSource};
use crate::infrastructure::rtp::payload_type_for;

/// Builds the SDP body advertised for a source.
///
/// `looping` sources are described with an open-ended range so clients treat
/// them as live rather than showing a seek bar that will never be honoured.
pub fn describe(source: &MediaSource, looping: bool) -> String {
    let mut sdp = String::new();
    sdp.push_str("v=0\r\n");
    sdp.push_str("o=- 0 0 IN IP4 127.0.0.1\r\n");
    sdp.push_str(&format!("s={}\r\n", source.name));
    sdp.push_str("c=IN IP4 0.0.0.0\r\n");
    sdp.push_str("t=0 0\r\n");
    sdp.push_str("a=tool:rtsp-utils\r\n");
    if looping {
        sdp.push_str("a=range:npt=0-\r\n");
    } else {
        sdp.push_str(&format!("a=range:npt=0-{:.3}\r\n", source.duration_secs));
    }
    sdp.push_str("a=control:*\r\n");

    for track in &source.tracks {
        let pt = payload_type_for(track);
        sdp.push_str(&format!(
            "m={} 0 RTP/AVP {pt}\r\n",
            track.kind.sdp_media()
        ));
        sdp.push_str("c=IN IP4 0.0.0.0\r\n");

        // rtpmap is `encoding/clock` for video and `encoding/clock/channels`
        // for audio.
        match &track.codec {
            CodecParams::H264(_) => sdp.push_str(&format!(
                "a=rtpmap:{pt} {}/{}\r\n",
                track.codec.encoding_name(),
                track.codec.clock_rate()
            )),
            CodecParams::Aac(params) => sdp.push_str(&format!(
                "a=rtpmap:{pt} {}/{}/{}\r\n",
                track.codec.encoding_name(),
                track.codec.clock_rate(),
                params.channels
            )),
        }

        match &track.codec {
            CodecParams::H264(params) => describe_h264(&mut sdp, pt, params),
            CodecParams::Aac(params) => describe_aac(&mut sdp, pt, params),
        }

        sdp.push_str(&format!("a=control:{}\r\n", track.control()));
    }

    sdp
}

fn describe_h264(sdp: &mut String, pt: u8, params: &H264Params) {
    // profile-level-id is the profile_idc / constraint flags / level_idc
    // triplet taken straight from the start of the SPS.
    let profile_level_id = if params.sps.len() >= 4 {
        hex(&params.sps[1..4])
    } else {
        "42001e".to_string()
    };

    sdp.push_str(&format!(
        "a=fmtp:{pt} packetization-mode=1;profile-level-id={};sprop-parameter-sets={},{}\r\n",
        profile_level_id,
        base64(&params.sps),
        base64(&params.pps),
    ));
    sdp.push_str(&format!(
        "a=x-dimensions:{},{}\r\n",
        params.width, params.height
    ));
}

fn describe_aac(sdp: &mut String, pt: u8, params: &AacParams) {
    sdp.push_str(&format!(
        "a=fmtp:{pt} streamtype=5;profile-level-id=1;mode=AAC-hbr;\
config={};sizelength=13;indexlength=3;indexdeltalength=3\r\n",
        hex(&params.config)
    ));
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(BASE64_ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(BASE64_ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
