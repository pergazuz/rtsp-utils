//! H.264 RTP payload format (RFC 6184): single NAL unit packets, with FU-A
//! fragmentation for NAL units that exceed the MTU.

use super::packet::{self, MAX_PAYLOAD};
use crate::domain::media::H264Params;
use crate::domain::ports::Packetizer;
use crate::domain::{Error, Result};

const FU_A: u8 = 28;

pub struct H264Packetizer {
    params: H264Params,
    payload_type: u8,
    ssrc: u32,
    seq: u16,
}

impl H264Packetizer {
    pub fn new(params: H264Params, payload_type: u8, ssrc: u32, initial_seq: u16) -> Self {
        Self {
            params,
            payload_type,
            ssrc,
            seq: initial_seq,
        }
    }
}

impl Packetizer for H264Packetizer {
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

        // Repeat the parameter sets ahead of every IDR: clients that join
        // mid-stream would otherwise have nothing to initialise a decoder with.
        if keyframe {
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
                    "H.264 sample declares a {nal_len}-byte NAL unit but only {} bytes remain",
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

/// Emits one NAL unit as either a single-NAL packet or a run of FU-A fragments.
fn emit_nal(
    unit: &[u8],
    marker: bool,
    timestamp: u32,
    payload_type: u8,
    ssrc: u32,
    seq: &mut u16,
    out: &mut Vec<Vec<u8>>,
) {
    if unit.is_empty() {
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

    // FU-A: the original NAL header is split into an indicator and an FU header.
    let nri = unit[0] & 0x60;
    let nal_type = unit[0] & 0x1f;
    let indicator = nri | FU_A;

    let payload = &unit[1..];
    // Two extra payload bytes per fragment for the indicator and FU header.
    let chunk = MAX_PAYLOAD - 2;
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

        let mut pkt = Vec::with_capacity(packet::RTP_HEADER_LEN + 2 + (end - offset));
        packet::write_header(
            &mut pkt,
            marker && is_last,
            payload_type,
            *seq,
            timestamp,
            ssrc,
        );
        pkt.push(indicator);
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

    fn params() -> H264Params {
        H264Params {
            // 0x67 = NAL type 7 (SPS), 0x68 = type 8 (PPS).
            sps: vec![0x67, 0x42, 0x00, 0x1e, 0xaa],
            pps: vec![0x68, 0xce, 0x3c, 0x80],
            nal_length_size: 4,
            width: 1280,
            height: 720,
        }
    }

    /// Wraps NAL units in the AVCC length-prefixed form a sample uses.
    fn avcc(units: &[&[u8]]) -> Vec<u8> {
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

    fn sequence(packet: &[u8]) -> u16 {
        u16::from_be_bytes([packet[2], packet[3]])
    }

    #[test]
    fn small_nal_units_become_one_packet_each() {
        let mut p = H264Packetizer::new(params(), 96, 0xdead_beef, 100);
        let mut out = Vec::new();
        p.packetize(9000, false, &avcc(&[&[0x41, 0x01, 0x02], &[0x41, 0x03]]), &mut out)
            .unwrap();

        assert_eq!(out.len(), 2);
        assert_eq!(payload(&out[0]), &[0x41, 0x01, 0x02]);
        assert_eq!(payload(&out[1]), &[0x41, 0x03]);
        // Only the final packet of an access unit carries the marker bit.
        assert!(!marker(&out[0]));
        assert!(marker(&out[1]));
        assert_eq!(sequence(&out[0]), 100);
        assert_eq!(sequence(&out[1]), 101);
        assert_eq!(p.next_sequence(), 102);
    }

    #[test]
    fn keyframes_are_preceded_by_the_parameter_sets() {
        let mut p = H264Packetizer::new(params(), 96, 1, 0);
        let mut out = Vec::new();
        p.packetize(0, true, &avcc(&[&[0x65, 0xaa]]), &mut out).unwrap();

        assert_eq!(out.len(), 3);
        assert_eq!(payload(&out[0])[0] & 0x1f, 7, "SPS first");
        assert_eq!(payload(&out[1])[0] & 0x1f, 8, "PPS second");
        assert_eq!(payload(&out[2])[0] & 0x1f, 5, "then the IDR itself");
    }

    #[test]
    fn oversized_nal_units_are_fragmented_as_fu_a() {
        let mut unit = vec![0x65]; // IDR NAL header, NRI = 3
        unit.extend(std::iter::repeat(0xab).take(MAX_PAYLOAD * 2 + 10));

        let mut p = H264Packetizer::new(params(), 96, 1, 0);
        let mut out = Vec::new();
        p.packetize(0, false, &avcc(&[&unit]), &mut out).unwrap();

        assert!(out.len() >= 3, "expected several fragments, got {}", out.len());

        let mut reassembled = vec![0x65];
        for (i, packet) in out.iter().enumerate() {
            let body = payload(packet);
            assert_eq!(body[0], 0x60 | FU_A, "FU indicator keeps NRI, sets type 28");
            assert_eq!(body[1] & 0x1f, 5, "FU header keeps the original NAL type");

            let is_first = i == 0;
            let is_last = i == out.len() - 1;
            assert_eq!(body[1] & 0x80 != 0, is_first, "start bit only on the first");
            assert_eq!(body[1] & 0x40 != 0, is_last, "end bit only on the last");
            assert!(packet.len() <= RTP_HEADER_LEN + MAX_PAYLOAD);

            reassembled.extend_from_slice(&body[2..]);
        }
        assert_eq!(reassembled, unit, "fragments rebuild the original NAL unit");
    }

    #[test]
    fn a_truncated_sample_is_reported_rather_than_silently_dropped() {
        let mut p = H264Packetizer::new(params(), 96, 1, 0);
        let mut out = Vec::new();
        // Declares 100 bytes but supplies 2.
        let sample = [0, 0, 0, 100, 0x41, 0x01];
        assert!(p.packetize(0, false, &sample, &mut out).is_err());
    }
}
