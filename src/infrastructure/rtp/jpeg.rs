//! JPEG RTP payload format (RFC 2435): the entropy-coded scan is fragmented
//! across packets, each led by a main header carrying the byte offset into
//! the frame. The receiver rebuilds the JPEG headers from those fields plus
//! the quantization tables, which ride in the first packet of every frame.

use super::packet::{self, MAX_PAYLOAD};
use crate::domain::media::JpegParams;
use crate::domain::ports::Packetizer;
use crate::domain::{Error, Result};

/// The static payload type RFC 3551 assigns to JPEG.
pub const PAYLOAD_TYPE: u8 = 26;

pub struct JpegPacketizer {
    params: JpegParams,
    payload_type: u8,
    ssrc: u32,
    seq: u16,
}

impl JpegPacketizer {
    pub fn new(params: JpegParams, payload_type: u8, ssrc: u32, initial_seq: u16) -> Self {
        Self {
            params,
            payload_type,
            ssrc,
            seq: initial_seq,
        }
    }
}

impl Packetizer for JpegPacketizer {
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
        if data.len() >= 1 << 24 {
            return Err(Error::MalformedContainer(format!(
                "a {}-byte JPEG scan exceeds the 24-bit RTP/JPEG fragment offset",
                data.len()
            )));
        }

        let restarts = self.params.restart_interval > 0;
        // Types 64-127 mean "restart markers present"; the restart header
        // then follows the main header in every packet.
        let type_code = self.params.type_code + if restarts { 64 } else { 0 };
        let width8 = self.params.width.div_ceil(8) as u8;
        let height8 = self.params.height.div_ceil(8) as u8;

        let mut offset = 0usize;
        while offset < data.len() {
            let first = offset == 0;
            let mut header_len = 8;
            if restarts {
                header_len += 4;
            }
            if first {
                header_len += 4 + self.params.quant_tables.len();
            }

            let chunk = (MAX_PAYLOAD - header_len).min(data.len() - offset);
            let last = offset + chunk == data.len();

            let mut pkt = Vec::with_capacity(packet::RTP_HEADER_LEN + header_len + chunk);
            packet::write_header(&mut pkt, last, self.payload_type, self.seq, timestamp, self.ssrc);

            // Main header: type-specific, 24-bit fragment offset, type, Q,
            // then the dimensions in units of 8 pixels (rounded up, so an
            // odd-sized image decodes with a few padding columns).
            pkt.push(0);
            pkt.extend_from_slice(&[(offset >> 16) as u8, (offset >> 8) as u8, offset as u8]);
            pkt.push(type_code);
            // Q >= 128: the tables travel in-band in the first fragment.
            pkt.push(255);
            pkt.push(width8);
            pkt.push(height8);

            if restarts {
                pkt.extend_from_slice(&self.params.restart_interval.to_be_bytes());
                // F=1, L=1, count=0x3fff: fragments are cut without regard to
                // restart boundaries, which RFC 2435 spells as all-ones.
                pkt.extend_from_slice(&[0xff, 0xff]);
            }

            if first {
                // Quantization table header: MBZ, precision (0 = every table
                // is 8-bit), byte length, then the tables themselves.
                pkt.push(0);
                pkt.push(0);
                pkt.extend_from_slice(&(self.params.quant_tables.len() as u16).to_be_bytes());
                pkt.extend_from_slice(&self.params.quant_tables);
            }

            pkt.extend_from_slice(&data[offset..offset + chunk]);
            self.seq = self.seq.wrapping_add(1);
            out.push(pkt);
            offset += chunk;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::rtp::packet::RTP_HEADER_LEN;

    fn params() -> JpegParams {
        JpegParams {
            type_code: 1,
            width: 640,
            height: 480,
            restart_interval: 0,
            quant_tables: (0..128).collect(),
        }
    }

    fn payload(packet: &[u8]) -> &[u8] {
        &packet[RTP_HEADER_LEN..]
    }

    fn marker(packet: &[u8]) -> bool {
        packet[1] & 0x80 != 0
    }

    fn fragment_offset(payload: &[u8]) -> usize {
        (payload[1] as usize) << 16 | (payload[2] as usize) << 8 | payload[3] as usize
    }

    #[test]
    fn a_small_frame_is_one_packet_with_tables_in_front() {
        let mut p = JpegPacketizer::new(params(), PAYLOAD_TYPE, 1, 500);
        let mut out = Vec::new();
        let scan = [0xaa; 100];
        p.packetize(90_000, true, &scan, &mut out).unwrap();

        assert_eq!(out.len(), 1);
        let body = payload(&out[0]);
        assert!(marker(&out[0]), "the frame's last packet carries the marker");

        assert_eq!(body[0], 0, "type-specific");
        assert_eq!(fragment_offset(body), 0);
        assert_eq!(body[4], 1, "type: 4:2:0 without restarts");
        assert_eq!(body[5], 255, "Q says the tables are in-band");
        assert_eq!(body[6], 80, "640 / 8");
        assert_eq!(body[7], 60, "480 / 8");

        // Quantization table header, then the tables, then the scan.
        assert_eq!(&body[8..10], &[0, 0], "MBZ and 8-bit precision");
        assert_eq!(u16::from_be_bytes([body[10], body[11]]), 128);
        assert_eq!(&body[12..140], &params().quant_tables[..]);
        assert_eq!(&body[140..], &scan);
        assert_eq!(p.next_sequence(), 501);
    }

    #[test]
    fn a_large_frame_fragments_and_reassembles() {
        let mut p = JpegPacketizer::new(params(), PAYLOAD_TYPE, 1, 0);
        let mut out = Vec::new();
        let scan: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        p.packetize(0, true, &scan, &mut out).unwrap();

        assert!(out.len() >= 3, "expected several fragments, got {}", out.len());

        let mut reassembled = Vec::new();
        for (i, pkt) in out.iter().enumerate() {
            assert!(pkt.len() <= RTP_HEADER_LEN + MAX_PAYLOAD);
            let body = payload(pkt);
            assert_eq!(
                fragment_offset(body),
                reassembled.len(),
                "offsets must be contiguous"
            );
            let data_at = if i == 0 { 12 + 128 } else { 8 };
            reassembled.extend_from_slice(&body[data_at..]);

            let is_last = i == out.len() - 1;
            assert_eq!(marker(pkt), is_last, "marker only on the last fragment");
        }
        assert_eq!(reassembled, scan, "fragments rebuild the original scan");
    }

    #[test]
    fn restart_intervals_switch_the_type_and_add_a_header() {
        let mut with_restarts = params();
        with_restarts.restart_interval = 8;
        let mut p = JpegPacketizer::new(with_restarts, PAYLOAD_TYPE, 1, 0);
        let mut out = Vec::new();
        let scan: Vec<u8> = (0..4000u32).map(|i| (i % 251) as u8).collect();
        p.packetize(0, true, &scan, &mut out).unwrap();

        assert!(out.len() >= 2);
        for pkt in &out {
            let body = payload(pkt);
            assert_eq!(body[4], 1 + 64, "types 64-127 mean restarts are present");
            assert_eq!(u16::from_be_bytes([body[8], body[9]]), 8, "the interval");
            assert_eq!(&body[10..12], &[0xff, 0xff], "F=1, L=1, count=0x3fff");
        }
    }

    #[test]
    fn odd_dimensions_round_up_to_the_next_block() {
        let mut odd = params();
        odd.width = 1706;
        odd.height = 960;
        let mut p = JpegPacketizer::new(odd, PAYLOAD_TYPE, 1, 0);
        let mut out = Vec::new();
        p.packetize(0, true, &[1, 2, 3], &mut out).unwrap();

        let body = payload(&out[0]);
        assert_eq!(body[6], 214, "ceil(1706 / 8)");
        assert_eq!(body[7], 120, "960 / 8");
    }
}
