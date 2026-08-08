//! Transport negotiation and the RTP sink that feeds a playing session.

use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::domain::ports::RtpSink;
use crate::domain::{Error, Result};

/// How a client asked to receive one track.
#[derive(Debug, Clone, Copy)]
pub enum TransportSpec {
    /// RTP framed on the RTSP control connection itself.
    TcpInterleaved { rtp: u8, rtcp: u8 },
    /// Classic two-port UDP delivery.
    Udp { rtp: u16, rtcp: u16 },
}

/// Picks the first transport alternative we can honour from a Transport header.
pub fn parse_transport(header: &str, track: usize) -> Result<TransportSpec> {
    for alternative in header.split(',') {
        let params: Vec<&str> = alternative.split(';').map(str::trim).collect();
        let protocol = params.first().copied().unwrap_or("");
        let is_tcp = protocol.eq_ignore_ascii_case("RTP/AVP/TCP");

        if is_tcp {
            let channels = params
                .iter()
                .find_map(|p| p.strip_prefix("interleaved="))
                .and_then(parse_port_pair)
                // Clients may omit the channels and let the server choose.
                .unwrap_or(((track * 2) as u16, (track * 2 + 1) as u16));
            return Ok(TransportSpec::TcpInterleaved {
                rtp: channels.0 as u8,
                rtcp: channels.1 as u8,
            });
        }

        if protocol.eq_ignore_ascii_case("RTP/AVP") || protocol.eq_ignore_ascii_case("RTP/AVP/UDP")
        {
            // Multicast would need a different sink; we only serve unicast.
            if params.iter().any(|p| p.eq_ignore_ascii_case("multicast")) {
                continue;
            }
            if let Some((rtp, rtcp)) = params
                .iter()
                .find_map(|p| p.strip_prefix("client_port="))
                .and_then(parse_port_pair)
            {
                return Ok(TransportSpec::Udp { rtp, rtcp });
            }
        }
    }

    Err(Error::Protocol(format!(
        "no supported transport in '{header}' (offer RTP/AVP/TCP interleaved or RTP/AVP unicast)"
    )))
}

fn parse_port_pair(value: &str) -> Option<(u16, u16)> {
    let (a, b) = value.split_once('-')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// A bound UDP port pair waiting to deliver one track.
pub struct UdpPair {
    pub rtp: UdpSocket,
    pub rtcp: UdpSocket,
    pub rtp_port: u16,
    pub rtcp_port: u16,
}

/// Binds consecutive even/odd ports, as RFC 3550 §11 expects.
pub fn bind_udp_pair(local: IpAddr) -> Result<UdpPair> {
    for _ in 0..32 {
        let rtp = UdpSocket::bind(SocketAddr::new(local, 0))?;
        let rtp_port = rtp.local_addr()?.port();
        if rtp_port % 2 != 0 || rtp_port == u16::MAX {
            continue;
        }
        let Ok(rtcp) = UdpSocket::bind(SocketAddr::new(local, rtp_port + 1)) else {
            continue;
        };
        return Ok(UdpPair {
            rtp,
            rtcp,
            rtp_port,
            rtcp_port: rtp_port + 1,
        });
    }
    Err(Error::Config(
        "could not bind an even/odd UDP port pair for RTP".into(),
    ))
}

enum TrackTransport {
    Tcp {
        rtp_channel: u8,
        rtcp_channel: u8,
    },
    Udp {
        sockets: UdpPair,
        peer_rtp: SocketAddr,
        peer_rtcp: SocketAddr,
    },
}

/// Routes each track's packets to whichever transport that track negotiated.
pub struct SessionSink {
    /// Shared with the request handler so interleaved data and RTSP responses
    /// never overlap on the control socket.
    control: Arc<Mutex<TcpStream>>,
    tracks: HashMap<usize, TrackTransport>,
    closed: AtomicBool,
}

impl SessionSink {
    pub fn new(control: Arc<Mutex<TcpStream>>) -> Self {
        Self {
            control,
            tracks: HashMap::new(),
            closed: AtomicBool::new(false),
        }
    }

    pub fn add_tcp(&mut self, track: usize, rtp_channel: u8, rtcp_channel: u8) {
        self.tracks.insert(
            track,
            TrackTransport::Tcp {
                rtp_channel,
                rtcp_channel,
            },
        );
    }

    pub fn add_udp(
        &mut self,
        track: usize,
        sockets: UdpPair,
        peer_rtp: SocketAddr,
        peer_rtcp: SocketAddr,
    ) {
        self.tracks.insert(
            track,
            TrackTransport::Udp {
                sockets,
                peer_rtp,
                peer_rtcp,
            },
        );
    }

    fn send(&self, track: usize, packet: &[u8], rtcp: bool) -> Result<()> {
        let Some(transport) = self.tracks.get(&track) else {
            // A track that was never SETUP simply isn't delivered.
            return Ok(());
        };

        let result = match transport {
            TrackTransport::Tcp {
                rtp_channel,
                rtcp_channel,
            } => {
                let channel = if rtcp { *rtcp_channel } else { *rtp_channel };
                self.write_interleaved(channel, packet)
            }
            TrackTransport::Udp {
                sockets,
                peer_rtp,
                peer_rtcp,
            } => {
                let (socket, peer) = if rtcp {
                    (&sockets.rtcp, peer_rtcp)
                } else {
                    (&sockets.rtp, peer_rtp)
                };
                socket.send_to(packet, peer).map(|_| ()).map_err(Error::Io)
            }
        };

        if result.is_err() {
            self.closed.store(true, Ordering::Relaxed);
        }
        result
    }

    /// `$ <channel> <length:16> <packet>` framing from RFC 2326 §10.12.
    fn write_interleaved(&self, channel: u8, packet: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(4 + packet.len());
        frame.push(b'$');
        frame.push(channel);
        frame.extend_from_slice(&(packet.len() as u16).to_be_bytes());
        frame.extend_from_slice(packet);

        let mut control = self
            .control
            .lock()
            .map_err(|_| Error::Protocol("control connection lock poisoned".into()))?;
        control.write_all(&frame)?;
        Ok(())
    }
}

impl RtpSink for SessionSink {
    fn send_rtp(&self, track: usize, packet: &[u8]) -> Result<()> {
        self.send(track, packet, false)
    }

    fn send_rtcp(&self, track: usize, packet: &[u8]) -> Result<()> {
        self.send(track, packet, true)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_interleaved_channels() {
        let spec = parse_transport("RTP/AVP/TCP;unicast;interleaved=2-3", 1).unwrap();
        match spec {
            TransportSpec::TcpInterleaved { rtp, rtcp } => {
                assert_eq!((rtp, rtcp), (2, 3));
            }
            other => panic!("expected interleaved TCP, got {other:?}"),
        }
    }

    #[test]
    fn defaults_interleaved_channels_when_the_client_omits_them() {
        let spec = parse_transport("RTP/AVP/TCP;unicast", 1).unwrap();
        match spec {
            TransportSpec::TcpInterleaved { rtp, rtcp } => assert_eq!((rtp, rtcp), (2, 3)),
            other => panic!("expected interleaved TCP, got {other:?}"),
        }
    }

    #[test]
    fn parses_udp_client_ports() {
        let spec = parse_transport("RTP/AVP;unicast;client_port=5000-5001", 0).unwrap();
        match spec {
            TransportSpec::Udp { rtp, rtcp } => assert_eq!((rtp, rtcp), (5000, 5001)),
            other => panic!("expected UDP, got {other:?}"),
        }
    }

    #[test]
    fn skips_multicast_and_takes_the_next_alternative() {
        let spec = parse_transport(
            "RTP/AVP;multicast;port=6000-6001,RTP/AVP/TCP;unicast;interleaved=0-1",
            0,
        )
        .unwrap();
        assert!(matches!(spec, TransportSpec::TcpInterleaved { rtp: 0, rtcp: 1 }));
    }

    #[test]
    fn rejects_a_transport_we_cannot_serve() {
        assert!(parse_transport("RTP/SAVP;unicast;client_port=5000-5001", 0).is_err());
        // Unicast UDP without client ports leaves us nowhere to send.
        assert!(parse_transport("RTP/AVP;unicast", 0).is_err());
    }
}
