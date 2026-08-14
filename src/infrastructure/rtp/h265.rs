//! H.265 RTP payload format (RFC 7798): single NAL unit packets, with FU
//! fragmentation for NAL units that exceed the MTU.

use super::packet::{self, MAX_PAYLOAD};
use crate::domain::media::H265Params;
use crate::domain::ports::Packetizer;
use crate::domain::{Error, Result};

const FU: u8 = 49;

pub struct H265Packetizer {
    params: H265Params,
    payload_type: u8,
    ssrc: u32,
    seq: u16,
}

impl H265Packetizer {
    pub fn new(params: H265Params, payload_type: u8, ssrc: u32, initial_seq: u16) -> Self {
        Self {
            params,
            payload_type,
            ssrc,
            seq: initial_seq,
        }
    }
}

impl Packetizer for H265Packetizer {
    fn packetize(
        &mut self,
        timestamp: u32,
        keyframe: bool,
        data: &[u8],
        out: &mut Vec<Vec<u8>>,
    ) -> Result<()> {
        // Collect the access unit's NAL units first so we know which packet
        // carries the marker bit (the last one of the frame).
        let mut units: Vec<&[u8]> = Vec::with_capacity(8);

        // Repeat the parameter sets ahead of every IRAP frame: clients that
        // join mid-stream would otherwise have nothing to initialise with.
        if keyframe {
            units.push(&self.params.vps);
            units.push(&self.params.sps);
            units.push(&self.params.pps);
        }

        let length_size = self.params.nal_length_size;
        let mut pos = 0usize;
        while pos + length_size <= data.len() {
            let mut nal_len = 0usize;
            for i in 0..length_size {
                nal_len = (nal_len << 8) | data[pos + i] as usize;
            }
            pos += length_size;

            if nal_len == 0 {
                continue;
            }
            let Some(unit) = data.get(pos..pos + nal_len) else {
                return Err(Error::MalformedContainer(format!(
                    "H.265 sample declares a {nal_len}-byte NAL unit but only {} bytes remain",
                    data.len() - pos
                )));
            };
            units.push(unit);
            pos += nal_len;
        }

        let last = units.len().saturating_sub(1);
        for (i, unit) in units.iter().enumerate() {
            let marker = i == last;
            emit_nal(
                unit,
                marker,
                timestamp,
                self.payload_type,
                self.ssrc,
                &mut self.seq,
                out,
            );
        }
        Ok(())
    }

    fn ssrc(&self) -> u32 {
        self.ssrc
    }

    fn clock_rate(&self) -> u32 {
        90_000
    }

    fn next_sequence(&self) -> u16 {
        self.seq
    }
}

/// Emits one NAL unit as either a single-NAL packet or a run of FU fragments.
fn emit_nal(
    unit: &[u8],
    marker: bool,
    timestamp: u32,
    payload_type: u8,
    ssrc: u32,
    seq: &mut u16,
    out: &mut Vec<Vec<u8>>,
) {
    // The HEVC NAL header is two bytes; anything shorter is not a NAL unit.
    if unit.len() < 2 {
        return;
    }

    if unit.len() <= MAX_PAYLOAD {
        let mut pkt = Vec::with_capacity(packet::RTP_HEADER_LEN + unit.len());
        packet::write_header(&mut pkt, marker, payload_type, *seq, timestamp, ssrc);
        pkt.extend_from_slice(unit);
        *seq = seq.wrapping_add(1);
        out.push(pkt);
        return;
    }

    // FU: the payload header keeps the F bit, layer id and TID of the original
    // NAL unit but replaces its type; the type moves into the FU header.
    let nal_type = (unit[0] >> 1) & 0x3f;
    let payload_header = [(unit[0] & 0x81) | (FU << 1), unit[1]];

    let payload = &unit[2..];
    // Three extra bytes per fragment: the payload header and the FU header.
    let chunk = MAX_PAYLOAD - 3;
    let total = payload.len();
    let mut offset = 0usize;

    while offset < total {
        let end = (offset + chunk).min(total);
        let is_first = offset == 0;
        let is_last = end == total;

        let mut fu_header = nal_type;
        if is_first {
            fu_header |= 0x80; // S
        }
        if is_last {
            fu_header |= 0x40; // E
        }

        let mut pkt = Vec::with_capacity(packet::RTP_HEADER_LEN + 3 + (end - offset));
        packet::write_header(
            &mut pkt,
            marker && is_last,
            payload_type,
            *seq,
            timestamp,
            ssrc,
        );
        pkt.extend_from_slice(&payload_header);
        pkt.push(fu_header);
        pkt.extend_from_slice(&payload[offset..end]);
        *seq = seq.wrapping_add(1);
        out.push(pkt);

        offset = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::rtp::packet::RTP_HEADER_LEN;

    /// An HEVC NAL header: type in bits 6..1 of the first byte, layer id
    /// spanning the bytes, TID in the low three bits of the second.
    fn nal_header(nal_type: u8) -> [u8; 2] {
        [nal_type << 1, 0x01]
    }

    fn params() -> H265Params {
        H265Params {
            config: vec![1; 23],
            vps: [nal_header(32).as_slice(), &[0xaa]].concat(),
            sps: [nal_header(33).as_slice(), &[0xbb]].concat(),
            pps: [nal_header(34).as_slice(), &[0xcc]].concat(),
            nal_length_size: 4,
            width: 2560,
            height: 1920,
        }
    }

    /// Wraps NAL units in the length-prefixed form a sample uses.
    fn prefixed(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in units {
            out.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            out.extend_from_slice(unit);
        }
        out
    }

    fn payload(packet: &[u8]) -> &[u8] {
        &packet[RTP_HEADER_LEN..]
    }

    fn marker(packet: &[u8]) -> bool {
        packet[1] & 0x80 != 0
    }

    fn nal_type(payload: &[u8]) -> u8 {
        (payload[0] >> 1) & 0x3f
    }

    #[test]
    fn small_nal_units_become_one_packet_each() {
        let mut p = H265Packetizer::new(params(), 96, 0xdead_beef, 100);
        let mut out = Vec::new();
        let trail = [nal_header(1).as_slice(), &[0x99]].concat();
        p.packetize(
            9000,
            false,
            &prefixed(&[&[0x02, 0x01, 0xaa], &trail]),
            &mut out,
        )
        .unwrap();

        assert_eq!(out.len(), 2);
        assert_eq!(payload(&out[0]), &[0x02, 0x01, 0xaa]);
        assert_eq!(payload(&out[1]), &trail[..]);
        // Only the final packet of an access unit carries the marker bit.
        assert!(!marker(&out[0]));
        assert!(marker(&out[1]));
        assert_eq!(p.next_sequence(), 102);
    }

    #[test]
    fn keyframes_are_preceded_by_the_parameter_sets() {
        let mut p = H265Packetizer::new(params(), 96, 1, 0);
        let mut out = Vec::new();
        // Type 19 = IDR_W_RADL.
        let idr = [nal_header(19).as_slice(), &[0xdd]].concat();
        p.packetize(0, true, &prefixed(&[&idr]), &mut out).unwrap();

        assert_eq!(out.len(), 4);
        assert_eq!(nal_type(payload(&out[0])), 32, "VPS first");
        assert_eq!(nal_type(payload(&out[1])), 33, "SPS second");
        assert_eq!(nal_type(payload(&out[2])), 34, "PPS third");
        assert_eq!(nal_type(payload(&out[3])), 19, "then the IDR itself");
        assert!(marker(&out[3]));
    }

    #[test]
    fn oversized_nal_units_are_fragmented_as_fu() {
        // Type 19 IDR with layer id 0 and TID 1.
        let mut unit = nal_header(19).to_vec();
        unit.extend(std::iter::repeat(0xab).take(MAX_PAYLOAD * 2 + 10));

        let mut p = H265Packetizer::new(params(), 96, 1, 0);
        let mut out = Vec::new();
        p.packetize(0, false, &prefixed(&[&unit]), &mut out).unwrap();

        assert!(out.len() >= 3, "expected several fragments, got {}", out.len());

        let mut reassembled = unit[..2].to_vec();
        for (i, packet) in out.iter().enumerate() {
            let body = payload(packet);
            assert_eq!(nal_type(body), FU, "payload header type is FU");
            assert_eq!(body[1], unit[1], "layer id and TID survive");
            assert_eq!(body[2] & 0x3f, 19, "FU header keeps the original type");

            let is_first = i == 0;
            let is_last = i == out.len() - 1;
            assert_eq!(body[2] & 0x80 != 0, is_first, "start bit only on the first");
            assert_eq!(body[2] & 0x40 != 0, is_last, "end bit only on the last");
            assert!(packet.len() <= RTP_HEADER_LEN + MAX_PAYLOAD);

            reassembled.extend_from_slice(&body[3..]);
        }
        assert_eq!(reassembled, unit, "fragments rebuild the original NAL unit");
    }

    #[test]
    fn a_truncated_sample_is_reported_rather_than_silently_dropped() {
        let mut p = H265Packetizer::new(params(), 96, 1, 0);
        let mut out = Vec::new();
        // Declares 100 bytes but supplies 2.
        let sample = [0, 0, 0, 100, 0x02, 0x01];
        assert!(p.packetize(0, false, &sample, &mut out).is_err());
    }
}
