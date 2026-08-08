//! AAC RTP payload format (RFC 3640, `mode=AAC-hbr`).
//!
//! Each packet carries an AU-headers section — a 16-bit bit-length field
//! followed by one 13-bit size / 3-bit index pair per access unit — and then
//! the raw AAC frames. We put exactly one access unit in each packet.

use super::packet::{self, MAX_PAYLOAD};
use crate::domain::media::AacParams;
use crate::domain::ports::Packetizer;
use crate::domain::Result;

/// AU-headers-length (2 bytes) + one 2-byte AU header.
const AU_SECTION_LEN: usize = 4;

pub struct AacPacketizer {
    clock_rate: u32,
    payload_type: u8,
    ssrc: u32,
    seq: u16,
}

impl AacPacketizer {
    pub fn new(params: &AacParams, payload_type: u8, ssrc: u32, initial_seq: u16) -> Self {
        Self {
            clock_rate: params.sample_rate,
            payload_type,
            ssrc,
            seq: initial_seq,
        }
    }
}

impl Packetizer for AacPacketizer {
    fn packetize(
        &mut self,
        timestamp: u32,
        _keyframe: bool,
        data: &[u8],
        out: &mut Vec<Vec<u8>>,
    ) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        // An access unit larger than the MTU has to be split; RFC 3640 calls
        // these fragments, and only the final one gets the marker bit.
        let max_chunk = MAX_PAYLOAD - AU_SECTION_LEN;
        if data.len() <= max_chunk {
            out.push(self.build(timestamp, data, data.len(), true));
            return Ok(());
        }

        let total = data.len();
        let mut offset = 0usize;
        while offset < total {
            let end = (offset + max_chunk).min(total);
            let is_last = end == total;
            // The AU header always advertises the size of the whole access
            // unit, not of the fragment.
            out.push(self.build(timestamp, &data[offset..end], total, is_last));
            offset = end;
        }
        Ok(())
    }

    fn ssrc(&self) -> u32 {
        self.ssrc
    }

    fn clock_rate(&self) -> u32 {
        self.clock_rate
    }

    fn next_sequence(&self) -> u16 {
        self.seq
    }
}

impl AacPacketizer {
    fn build(&mut self, timestamp: u32, payload: &[u8], au_size: usize, marker: bool) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(packet::RTP_HEADER_LEN + AU_SECTION_LEN + payload.len());
        packet::write_header(&mut pkt, marker, self.payload_type, self.seq, timestamp, self.ssrc);
        self.seq = self.seq.wrapping_add(1);

        // AU-headers-length, in bits: one 16-bit header.
        pkt.extend_from_slice(&16u16.to_be_bytes());
        // 13-bit AU size followed by a 3-bit AU index (0 for the first AU).
        let header = ((au_size as u16) & 0x1fff) << 3;
        pkt.extend_from_slice(&header.to_be_bytes());
        pkt.extend_from_slice(payload);
        pkt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::rtp::packet::RTP_HEADER_LEN;

    fn params() -> AacParams {
        AacParams {
            config: vec![0x11, 0x90],
            sample_rate: 48_000,
            channels: 1,
        }
    }

    #[test]
    fn one_access_unit_becomes_one_packet_with_an_au_header() {
        let mut p = AacPacketizer::new(&params(), 97, 7, 0);
        let mut out = Vec::new();
        let frame = vec![0x21, 0x00, 0x49, 0x90];
        p.packetize(4800, false, &frame, &mut out).unwrap();

        assert_eq!(out.len(), 1);
        let body = &out[0][RTP_HEADER_LEN..];
        // AU-headers-length is expressed in bits: one 16-bit header.
        assert_eq!(u16::from_be_bytes([body[0], body[1]]), 16);
        let au_header = u16::from_be_bytes([body[2], body[3]]);
        assert_eq!(au_header >> 3, frame.len() as u16, "13-bit AU size");
        assert_eq!(au_header & 0x07, 0, "3-bit AU index");
        assert_eq!(&body[4..], &frame[..]);
        assert!(out[0][1] & 0x80 != 0, "a complete AU sets the marker bit");
    }

    #[test]
    fn an_oversized_access_unit_is_fragmented_with_the_marker_on_the_last() {
        let mut p = AacPacketizer::new(&params(), 97, 7, 0);
        let mut out = Vec::new();
        let frame = vec![0x5a; MAX_PAYLOAD * 2];
        p.packetize(0, false, &frame, &mut out).unwrap();

        assert!(out.len() > 1);
        for (i, packet) in out.iter().enumerate() {
            let body = &packet[RTP_HEADER_LEN..];
            let au_header = u16::from_be_bytes([body[2], body[3]]);
            assert_eq!(
                au_header >> 3,
                (frame.len() & 0x1fff) as u16,
                "every fragment advertises the full AU size"
            );
            let is_last = i == out.len() - 1;
            assert_eq!(packet[1] & 0x80 != 0, is_last);
        }
    }

    #[test]
    fn an_empty_sample_produces_nothing() {
        let mut p = AacPacketizer::new(&params(), 97, 7, 0);
        let mut out = Vec::new();
        p.packetize(0, false, &[], &mut out).unwrap();
        assert!(out.is_empty());
    }
}
