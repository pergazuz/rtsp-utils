//! RTP / RTCP wire formatting (RFC 3550).

use std::time::{SystemTime, UNIX_EPOCH};

pub const RTP_HEADER_LEN: usize = 12;

/// Largest RTP payload we emit. 1400 keeps the IP datagram under the usual
/// 1500-byte Ethernet MTU once RTP/UDP/IP headers are added.
pub const MAX_PAYLOAD: usize = 1400 - RTP_HEADER_LEN;

/// Writes a 12-byte RTP header (version 2, no padding, no extension, no CSRCs).
pub fn write_header(out: &mut Vec<u8>, marker: bool, payload_type: u8, seq: u16, ts: u32, ssrc: u32) {
    out.push(0x80);
    out.push((payload_type & 0x7f) | if marker { 0x80 } else { 0 });
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ts.to_be_bytes());
    out.extend_from_slice(&ssrc.to_be_bytes());
}

/// Builds an RTCP Sender Report, which lets receivers align the RTP clocks of
/// the audio and video streams onto one wall clock.
pub fn sender_report(ssrc: u32, rtp_ts: u32, packets: u32, octets: u32) -> Vec<u8> {
    let (ntp_seconds, ntp_fraction) = ntp_now();
    let mut out = Vec::with_capacity(28);
    out.push(0x80); // V=2, P=0, RC=0
    out.push(200); // PT=SR
    out.extend_from_slice(&6u16.to_be_bytes()); // length in 32-bit words minus one
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(&ntp_seconds.to_be_bytes());
    out.extend_from_slice(&ntp_fraction.to_be_bytes());
    out.extend_from_slice(&rtp_ts.to_be_bytes());
    out.extend_from_slice(&packets.to_be_bytes());
    out.extend_from_slice(&octets.to_be_bytes());
    out
}

/// Current time as a 64-bit NTP timestamp (seconds since 1900 + fraction).
fn ntp_now() -> (u32, u32) {
    const UNIX_TO_NTP_SECONDS: u64 = 2_208_988_800;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() + UNIX_TO_NTP_SECONDS;
    let fraction = ((now.subsec_nanos() as u64) << 32) / 1_000_000_000;
    (seconds as u32, (fraction & 0xffff_ffff) as u32)
}

/// Cheap non-cryptographic entropy for SSRCs and initial sequence numbers.
/// RFC 3550 wants these unpredictable per session, not secret.
pub fn entropy(salt: u64) -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = nanos ^ salt.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    (x & 0xffff_ffff) as u32
}
