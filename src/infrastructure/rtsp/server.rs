//! The RTSP control server: one thread per connection, one playback thread per
//! playing session.

use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use super::message::{read_request, Request, Response};
use super::sdp;
use super::transport::{bind_udp_pair, parse_transport, SessionSink, TransportSpec, UdpPair};
use crate::application::{PlaybackSession, ServerConfig, StreamRegistry, TrackStream};
use crate::domain::media::MediaSource;
use crate::domain::ports::{Packetizer, RtcpReporter, SampleReaderFactory};
use crate::domain::{Error, Result};
use crate::infrastructure::rtp::{packet, packetizer_for, StandardRtcpReporter};

const SESSION_TIMEOUT_SECS: u32 = 60;
const SUPPORTED_METHODS: &str =
    "OPTIONS, DESCRIBE, SETUP, PLAY, PAUSE, TEARDOWN, GET_PARAMETER, SET_PARAMETER";

pub struct RtspServer {
    registry: Arc<StreamRegistry>,
    config: ServerConfig,
    readers: Arc<dyn SampleReaderFactory>,
    reporter: Arc<dyn RtcpReporter>,
    listener: TcpListener,
}

impl RtspServer {
    /// Claims the control port up front, so a port clash is reported before we
    /// tell anyone the stream is ready.
    pub fn bind(
        registry: Arc<StreamRegistry>,
        config: ServerConfig,
        readers: Arc<dyn SampleReaderFactory>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(config.bind)?;
        Ok(Self {
            registry,
            config,
            readers,
            reporter: Arc::new(StandardRtcpReporter),
            listener,
        })
    }

    /// The address actually bound; differs from the configured one when the
    /// caller asked for port 0.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    /// Serves until the process is killed.
    pub fn run(self) -> Result<()> {
        let listener = self.listener.try_clone()?;
        let server = Arc::new(self);

        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    let server = Arc::clone(&server);
                    thread::spawn(move || {
                        let peer = stream
                            .peer_addr()
                            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
                        if let Err(e) = Connection::new(server, stream, peer).serve() {
                            eprintln!("[rtsp] {peer}: {e}");
                        }
                        println!("[rtsp] {peer} disconnected");
                    });
                }
                Err(e) => eprintln!("[rtsp] accept failed: {e}"),
            }
        }
        Ok(())
    }
}

/// A track the client has SETUP but that is not streaming yet.
struct PendingSetup {
    track_index: usize,
    packetizer: Box<dyn Packetizer>,
    rtp_timestamp_offset: u32,
    transport: TransportSpec,
    udp: Option<UdpPair>,
}

struct Connection {
    server: Arc<RtspServer>,
    peer: SocketAddr,
    /// Shared with the RTP sink: responses and interleaved media must not
    /// interleave mid-write.
    control: Arc<Mutex<TcpStream>>,
    reader: BufReader<TcpStream>,
    session_id: Option<String>,
    source: Option<Arc<MediaSource>>,
    setups: Vec<PendingSetup>,
    playback: Option<PlaybackSession>,
}

impl Connection {
    fn new(server: Arc<RtspServer>, stream: TcpStream, peer: SocketAddr) -> Self {
        let reader = BufReader::new(stream.try_clone().expect("clone control socket"));
        Self {
            server,
            peer,
            control: Arc::new(Mutex::new(stream)),
            reader,
            session_id: None,
            source: None,
            setups: Vec::new(),
            playback: None,
        }
    }

    fn serve(mut self) -> Result<()> {
        println!("[rtsp] {} connected", self.peer);

        while let Some(request) = read_request(&mut self.reader)? {
            let cseq = request.cseq().to_string();
            println!("[rtsp] {} -> {} {}", self.peer, request.method, request.uri);

            let response = match request.method.as_str() {
                "OPTIONS" => Response::ok(&cseq).header("Public", SUPPORTED_METHODS),
                "DESCRIBE" => self.describe(&request, &cseq),
                "SETUP" => self.setup(&request, &cseq),
                "PLAY" => self.play(&request, &cseq),
                "PAUSE" => self.pause(&cseq),
                "TEARDOWN" => {
                    let response = self.teardown(&cseq);
                    self.write(&response)?;
                    break;
                }
                "GET_PARAMETER" | "SET_PARAMETER" => self.keepalive(&cseq),
                other => {
                    eprintln!("[rtsp] {} asked for unsupported method {other}", self.peer);
                    Response::error(501, "Not Implemented", &cseq)
                }
            };

            self.write(&response)?;
        }
        Ok(())
    }

    fn write(&self, response: &Response) -> Result<()> {
        let bytes = response.to_bytes();
        let mut control = self
            .control
            .lock()
            .map_err(|_| Error::Protocol("control connection lock poisoned".into()))?;
        control.write_all(&bytes)?;
        control.flush()?;
        Ok(())
    }

    fn describe(&mut self, request: &Request, cseq: &str) -> Response {
        let (name, _) = request.stream_and_track();
        let Some(source) = self.server.registry.get(&name) else {
            return not_found(&name, &self.server.registry.names(), cseq);
        };

        let body = sdp::describe(&source, self.server.config.looping).into_bytes();
        self.source = Some(source);

        Response::ok(cseq)
            .header("Content-Base", format!("{}/", request.uri.trim_end_matches('/')))
            .body("application/sdp", body)
    }

    fn setup(&mut self, request: &Request, cseq: &str) -> Response {
        // A SETUP arriving mid-playback would need us to renegotiate; the
        // simplest correct answer is to make the client start over.
        if self.playback.is_some() {
            return Response::error(455, "Method Not Valid In This State", cseq);
        }

        let (name, track_index) = request.stream_and_track();
        let Some(source) = self.server.registry.get(&name) else {
            return not_found(&name, &self.server.registry.names(), cseq);
        };
        // Without a control suffix there is nothing to bind a transport to,
        // except when the file has exactly one track.
        let track_index = match track_index.or_else(|| source.tracks.first().map(|t| t.index)) {
            Some(i) => i,
            None => return Response::error(400, "Bad Request", cseq),
        };
        let Some(track) = source.track(track_index) else {
            return Response::error(404, "Not Found", cseq);
        };

        let Some(header) = request.header("Transport") else {
            return Response::error(400, "Bad Request", cseq);
        };
        let spec = match parse_transport(header, track_index) {
            Ok(spec) => spec,
            Err(e) => {
                eprintln!("[rtsp] {}: {e}", self.peer);
                return Response::error(461, "Unsupported Transport", cseq);
            }
        };

        let packetizer = packetizer_for(track);
        let ssrc = packetizer.ssrc();
        let mut setup = PendingSetup {
            track_index,
            rtp_timestamp_offset: packet::entropy(track_index as u64 + 0xa11ce),
            packetizer,
            transport: spec,
            udp: None,
        };

        let transport_header = match spec {
            TransportSpec::TcpInterleaved { rtp, rtcp } => {
                format!("RTP/AVP/TCP;unicast;interleaved={rtp}-{rtcp};ssrc={ssrc:08x}")
            }
            TransportSpec::Udp { rtp, rtcp } => {
                let pair = match bind_udp_pair(local_bind_address(&self.server.config)) {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("[rtsp] {}: {e}", self.peer);
                        return Response::error(500, "Internal Server Error", cseq);
                    }
                };
                let header = format!(
                    "RTP/AVP;unicast;client_port={rtp}-{rtcp};server_port={}-{};ssrc={ssrc:08x}",
                    pair.rtp_port, pair.rtcp_port
                );
                setup.udp = Some(pair);
                header
            }
        };

        // Replacing an existing SETUP for the same track is legal and is how
        // clients switch transports.
        self.setups.retain(|s| s.track_index != track_index);
        self.setups.push(setup);
        self.source = Some(source);

        let session_id = self
            .session_id
            .get_or_insert_with(|| format!("{:08X}", packet::entropy(0xf00d)))
            .clone();

        Response::ok(cseq)
            .header("Transport", transport_header)
            .header(
                "Session",
                format!("{session_id};timeout={SESSION_TIMEOUT_SECS}"),
            )
    }

    fn play(&mut self, request: &Request, cseq: &str) -> Response {
        let Some(source) = self.source.clone() else {
            return Response::error(455, "Method Not Valid In This State", cseq);
        };
        if self.setups.is_empty() {
            return Response::error(455, "Method Not Valid In This State", cseq);
        }
        // Already playing: treat a repeat PLAY as a no-op keepalive.
        if self.playback.is_some() {
            return self.keepalive(cseq);
        }

        let reader = match self.server.readers.open(&source) {
            Ok(reader) => reader,
            Err(e) => {
                eprintln!("[rtsp] {}: cannot open media: {e}", self.peer);
                return Response::error(500, "Internal Server Error", cseq);
            }
        };

        let base = request.uri.trim_end_matches('/').to_string();
        let mut sink = SessionSink::new(Arc::clone(&self.control));
        let mut streams = Vec::new();
        let mut rtp_info = Vec::new();

        for setup in self.setups.drain(..) {
            let PendingSetup {
                track_index,
                packetizer,
                rtp_timestamp_offset,
                transport,
                udp,
            } = setup;

            match transport {
                TransportSpec::TcpInterleaved { rtp, rtcp } => sink.add_tcp(track_index, rtp, rtcp),
                TransportSpec::Udp { rtp, rtcp } => {
                    // Media goes back to the address the control connection
                    // came from, on the ports the client asked for.
                    let Some(pair) = udp else { continue };
                    let peer_rtp = SocketAddr::new(self.peer.ip(), rtp);
                    let peer_rtcp = SocketAddr::new(self.peer.ip(), rtcp);
                    sink.add_udp(track_index, pair, peer_rtp, peer_rtcp);
                }
            }

            let control = source
                .track(track_index)
                .map(|t| t.control())
                .unwrap_or_else(|| format!("trackID={track_index}"));
            rtp_info.push(format!(
                "url={base}/{control};seq={};rtptime={rtp_timestamp_offset}",
                packetizer.next_sequence()
            ));

            streams.push(TrackStream {
                track_index,
                packetizer,
                rtp_timestamp_offset,
            });
        }

        if streams.is_empty() {
            return Response::error(455, "Method Not Valid In This State", cseq);
        }

        println!(
            "[rtsp] {} playing '{}' ({} track(s))",
            self.peer,
            source.name,
            streams.len()
        );

        self.playback = Some(PlaybackSession::start(
            Arc::clone(&source),
            reader,
            streams,
            Arc::new(sink),
            Arc::clone(&self.server.reporter),
            self.server.config.looping,
        ));

        let mut response = Response::ok(cseq).header("Range", "npt=0.000-");
        if let Some(session) = &self.session_id {
            response = response.header("Session", session.clone());
        }
        response.header("RTP-Info", rtp_info.join(","))
    }

    /// We have no seek support, so PAUSE stops delivery and the client must
    /// re-SETUP to resume from the top.
    fn pause(&mut self, cseq: &str) -> Response {
        if let Some(mut playback) = self.playback.take() {
            playback.stop();
        }
        self.session_response(Response::ok(cseq))
    }

    fn teardown(&mut self, cseq: &str) -> Response {
        if let Some(mut playback) = self.playback.take() {
            playback.stop();
        }
        self.setups.clear();
        self.session_response(Response::ok(cseq))
    }

    fn keepalive(&self, cseq: &str) -> Response {
        self.session_response(Response::ok(cseq))
    }

    fn session_response(&self, response: Response) -> Response {
        match &self.session_id {
            Some(id) => response.header("Session", id.clone()),
            None => response,
        }
    }
}

/// UDP sockets bind on the same interface as the control port so replies leave
/// from an address the client expects.
fn local_bind_address(config: &ServerConfig) -> std::net::IpAddr {
    config.bind.ip()
}

fn not_found(name: &str, available: &[String], cseq: &str) -> Response {
    eprintln!(
        "[rtsp] no stream named '{name}' (published: {})",
        if available.is_empty() {
            "none".to_string()
        } else {
            available.join(", ")
        }
    );
    Response::error(404, "Not Found", cseq)
}
