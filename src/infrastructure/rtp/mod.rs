pub mod aac;
pub mod h264;
pub mod packet;

use crate::domain::media::{CodecParams, Track};
use crate::domain::ports::{Packetizer, RtcpReporter};

/// Dynamic payload types, assigned per track in SDP order.
pub const FIRST_DYNAMIC_PAYLOAD_TYPE: u8 = 96;

pub fn payload_type_for(track: &Track) -> u8 {
    FIRST_DYNAMIC_PAYLOAD_TYPE + track.index as u8
}

/// Builds the packetizer that matches a track's codec.
pub fn packetizer_for(track: &Track) -> Box<dyn Packetizer> {
    let payload_type = payload_type_for(track);
    let ssrc = packet::entropy(track.index as u64 + 1);
    let initial_seq = (packet::entropy(track.index as u64 + 0x5eed) & 0x7fff) as u16;

    match &track.codec {
        CodecParams::H264(params) => Box::new(h264::H264Packetizer::new(
            params.clone(),
            payload_type,
            ssrc,
            initial_seq,
        )),
        CodecParams::Aac(params) => Box::new(aac::AacPacketizer::new(
            params,
            payload_type,
            ssrc,
            initial_seq,
        )),
    }
}

/// RFC 3550 Sender Reports.
pub struct StandardRtcpReporter;

impl RtcpReporter for StandardRtcpReporter {
    fn sender_report(&self, ssrc: u32, rtp_timestamp: u32, packets: u32, octets: u32) -> Vec<u8> {
        packet::sender_report(ssrc, rtp_timestamp, packets, octets)
    }
}
