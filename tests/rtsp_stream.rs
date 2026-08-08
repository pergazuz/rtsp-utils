//! End-to-end tests: they start the real server and drive it with a real RTSP
//! client over a socket.
//!
//! They need a sample file. By default they pick up any `.mov` or `.mp4` in
//! the crate root; set `RTSP_UTILS_TEST_FILE` to name one explicitly. The
//! tests skip themselves when no media is present, because it is deliberately
//! not committed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rtsp_utils::application::{ServerConfig, StreamControl, StreamRegistry};
use rtsp_utils::domain::media::{CodecParams, MediaSource};
use rtsp_utils::infrastructure::mp4::{FileSampleReaderFactory, Mp4Probe};
use rtsp_utils::infrastructure::rtsp::RtspServer;

/// How long to sit and collect media before judging the stream.
const OBSERVE: Duration = Duration::from_secs(4);

/// Locates sample media: an explicit path if one is given, otherwise the first
/// container sitting in the crate root.
///
/// The crate root is resolved from `CARGO_MANIFEST_DIR` rather than the working
/// directory, which is not guaranteed to be anywhere in particular.
fn test_file() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("RTSP_UTILS_TEST_FILE") {
        let path = PathBuf::from(configured);
        return path.is_file().then_some(path);
    }

    let mut candidates: Vec<PathBuf> = std::fs::read_dir(env!("CARGO_MANIFEST_DIR"))
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("mov") || e.eq_ignore_ascii_case("mp4"))
        })
        .collect();

    // Sort so a directory with several files still picks the same one twice.
    candidates.sort();
    candidates.into_iter().next()
}

struct Server {
    addr: SocketAddr,
    source: Arc<MediaSource>,
    control: Arc<StreamControl>,
    registry: Arc<StreamRegistry>,
}

/// Publishes `path` and starts the server on an ephemeral port.
fn start_server(path: &Path) -> Server {
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        advertised_host: "127.0.0.1".into(),
        looping: true,
    };

    let registry = Arc::new(StreamRegistry::new());
    let control = Arc::new(StreamControl::new(
        Arc::new(Mp4Probe),
        Arc::clone(&registry),
        config.clone(),
    ));
    let source = Arc::clone(
        &control
            .add(path, Some("test"), true)
            .expect("probe the file")
            .source,
    );

    let server = RtspServer::bind(
        Arc::clone(&registry),
        config,
        Arc::new(FileSampleReaderFactory),
    )
    .expect("bind an ephemeral port");
    let addr = server.local_addr().expect("read the bound address");
    thread::spawn(move || {
        let _ = server.run();
    });

    Server {
        addr,
        source,
        control,
        registry,
    }
}

// ---- a minimal RTSP client --------------------------------------------------

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

enum Incoming {
    Response(Response),
    /// An interleaved RTP/RTCP frame: channel and payload.
    Frame(u8, Vec<u8>),
}

struct Client {
    socket: TcpStream,
    reader: BufReader<TcpStream>,
    cseq: u32,
    session: Option<String>,
    base: String,
}

impl Client {
    fn connect(addr: SocketAddr, stream: &str) -> Self {
        let socket = TcpStream::connect(addr).expect("connect to the server");
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let reader = BufReader::new(socket.try_clone().unwrap());
        Self {
            socket,
            reader,
            cseq: 0,
            session: None,
            base: format!("rtsp://{addr}/{stream}"),
        }
    }

    fn request(&mut self, method: &str, uri: &str, extra: &[(&str, String)]) -> Response {
        self.cseq += 1;
        let mut out = format!("{method} {uri} RTSP/1.0\r\nCSeq: {}\r\n", self.cseq);
        if let Some(session) = &self.session {
            out.push_str(&format!("Session: {session}\r\n"));
        }
        for (name, value) in extra {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        self.socket.write_all(out.as_bytes()).expect("send request");

        loop {
            match self.read_incoming() {
                Incoming::Response(response) => return response,
                // Media may already be flowing; it is not what we are after here.
                Incoming::Frame(..) => continue,
            }
        }
    }

    fn read_incoming(&mut self) -> Incoming {
        let mut first = [0u8; 1];
        self.reader.read_exact(&mut first).expect("read from server");

        if first[0] == b'$' {
            let mut header = [0u8; 3];
            self.reader.read_exact(&mut header).expect("frame header");
            let len = u16::from_be_bytes([header[1], header[2]]) as usize;
            let mut data = vec![0u8; len];
            self.reader.read_exact(&mut data).expect("frame payload");
            return Incoming::Frame(header[0], data);
        }

        let mut status_line = String::new();
        status_line.push(first[0] as char);
        self.reader.read_line(&mut status_line).expect("status line");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("header line");
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }

        let length: usize = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        if length > 0 {
            self.reader.read_exact(&mut body).expect("response body");
        }

        Incoming::Response(Response {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }

    /// Reads and discards whatever arrives for `window`, so packets already in
    /// flight do not count against a later quiet check.
    fn drain_for(&mut self, window: Duration) {
        self.socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let deadline = Instant::now() + window;
        let mut scratch = [0u8; 4096];
        while Instant::now() < deadline {
            match self.reader.read(&mut scratch) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }

    /// True when nothing at all arrives within `window`.
    fn is_quiet_for(&mut self, window: Duration) -> bool {
        self.socket.set_read_timeout(Some(window)).unwrap();
        let mut scratch = [0u8; 4096];
        matches!(self.reader.read(&mut scratch), Err(_) | Ok(0))
    }

    fn setup(&mut self, track: usize, transport: &str) -> Response {
        let uri = format!("{}/trackID={track}", self.base);
        let response = self.request("SETUP", &uri, &[("Transport", transport.to_string())]);
        if let Some(session) = response.header("Session") {
            // Strip the `;timeout=` parameter before echoing it back.
            self.session = Some(session.split(';').next().unwrap_or(session).to_string());
        }
        response
    }
}

// ---- RTP helpers ------------------------------------------------------------

struct Rtp {
    payload_type: u8,
    marker: bool,
    sequence: u16,
    timestamp: u32,
}

fn parse_rtp(packet: &[u8]) -> Rtp {
    assert!(packet.len() >= 12, "RTP packets carry a 12-byte header");
    assert_eq!(packet[0] >> 6, 2, "RTP version must be 2");
    Rtp {
        payload_type: packet[1] & 0x7f,
        marker: packet[1] & 0x80 != 0,
        sequence: u16::from_be_bytes([packet[2], packet[3]]),
        timestamp: u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
    }
}

#[derive(Default)]
struct TrackStats {
    packets: u32,
    frames: u32,
    first_timestamp: Option<u32>,
    last_timestamp: u32,
    sequence_gaps: u32,
    last_sequence: Option<u16>,
    saw_sps: bool,
    saw_pps: bool,
}

impl TrackStats {
    fn record(&mut self, rtp: &Rtp) {
        self.packets += 1;
        if rtp.marker {
            self.frames += 1;
        }
        if self.first_timestamp.is_none() {
            self.first_timestamp = Some(rtp.timestamp);
        }
        self.last_timestamp = rtp.timestamp;

        if let Some(previous) = self.last_sequence {
            if rtp.sequence != previous.wrapping_add(1) {
                self.sequence_gaps += 1;
            }
        }
        self.last_sequence = Some(rtp.sequence);
    }
}

// ---- tests ------------------------------------------------------------------

#[test]
fn describes_and_streams_over_tcp_interleaved() {
    let Some(path) = test_file() else {
        eprintln!("skipping: no test media (set RTSP_UTILS_TEST_FILE)");
        return;
    };
    let server = start_server(&path);
    let mut client = Client::connect(server.addr, "test");

    let response = client.request("OPTIONS", &client.base.clone(), &[]);
    assert_eq!(response.status, 200);
    assert!(response.header("Public").unwrap().contains("DESCRIBE"));

    let response = client.request(
        "DESCRIBE",
        &client.base.clone(),
        &[("Accept", "application/sdp".into())],
    );
    assert_eq!(response.status, 200, "DESCRIBE should succeed");
    assert_eq!(
        response.header("Content-Type"),
        Some("application/sdp"),
        "DESCRIBE must return SDP"
    );

    let sdp = &response.body;
    assert!(sdp.contains("m=video 0 RTP/AVP 96"), "SDP video line:\n{sdp}");
    assert!(sdp.contains("a=rtpmap:96 H264/90000"), "SDP rtpmap:\n{sdp}");
    assert!(
        sdp.contains("packetization-mode=1") && sdp.contains("sprop-parameter-sets="),
        "SDP must carry H.264 setup:\n{sdp}"
    );
    assert!(sdp.contains("a=control:trackID=0"), "SDP control URL:\n{sdp}");

    let has_audio = server
        .source
        .tracks
        .iter()
        .any(|t| matches!(t.codec, CodecParams::Aac(_)));
    if has_audio {
        assert!(sdp.contains("m=audio 0 RTP/AVP 97"), "SDP audio line:\n{sdp}");
        assert!(sdp.contains("mode=AAC-hbr"), "SDP AAC mode:\n{sdp}");
    }

    // Set up video on channels 0/1 and audio, if present, on 2/3.
    let response = client.setup(0, "RTP/AVP/TCP;unicast;interleaved=0-1");
    assert_eq!(response.status, 200, "SETUP video");
    assert!(response
        .header("Transport")
        .unwrap()
        .contains("interleaved=0-1"));
    assert!(response.header("Session").is_some(), "SETUP must open a session");

    if has_audio {
        let response = client.setup(1, "RTP/AVP/TCP;unicast;interleaved=2-3");
        assert_eq!(response.status, 200, "SETUP audio");
    }

    let response = client.request("PLAY", &client.base.clone(), &[]);
    assert_eq!(response.status, 200, "PLAY should succeed");
    let rtp_info = response.header("RTP-Info").expect("PLAY returns RTP-Info");
    assert!(rtp_info.contains("trackID=0"), "RTP-Info: {rtp_info}");
    assert!(rtp_info.contains("seq=") && rtp_info.contains("rtptime="));

    // Collect media for a few seconds.
    let started = Instant::now();
    let mut video = TrackStats::default();
    let mut audio = TrackStats::default();
    let mut rtcp_reports = 0u32;

    while started.elapsed() < OBSERVE {
        match client.read_incoming() {
            Incoming::Frame(channel, data) => match channel {
                0 => {
                    let rtp = parse_rtp(&data);
                    assert_eq!(rtp.payload_type, 96, "video payload type");
                    let nal_type = data[12] & 0x1f;
                    video.saw_sps |= nal_type == 7;
                    video.saw_pps |= nal_type == 8;
                    video.record(&rtp);
                }
                2 => {
                    let rtp = parse_rtp(&data);
                    assert_eq!(rtp.payload_type, 97, "audio payload type");
                    audio.record(&rtp);
                }
                // Odd channels carry RTCP.
                1 | 3 => {
                    assert_eq!(data[1], 200, "expected a Sender Report");
                    rtcp_reports += 1;
                }
                other => panic!("unexpected interleaved channel {other}"),
            },
            Incoming::Response(_) => panic!("unsolicited response while playing"),
        }
    }
    let elapsed = started.elapsed().as_secs_f64();

    eprintln!(
        "observed {:.1}s: video {} packets / {} frames, audio {} packets, {rtcp_reports} reports",
        elapsed, video.packets, video.frames, audio.packets
    );

    assert!(video.packets > 0, "no video arrived");
    assert_eq!(video.sequence_gaps, 0, "video sequence numbers must be contiguous");
    assert!(
        video.saw_sps && video.saw_pps,
        "parameter sets must be repeated in-band so late joiners can decode"
    );

    // Delivery must be paced against the clock, not dumped as fast as the disk
    // can serve it.
    let track = &server.source.tracks[0];
    let expected_fps = track.samples.len() as f64 / track.duration_secs();
    let actual_fps = video.frames as f64 / elapsed;
    assert!(
        actual_fps > expected_fps * 0.7 && actual_fps < expected_fps * 1.3,
        "expected roughly {expected_fps:.1} fps, got {actual_fps:.1} \
         ({} frames in {elapsed:.1}s)",
        video.frames
    );

    // The RTP clock must advance at 90 kHz in step with the wall clock.
    let ticks = video.last_timestamp.wrapping_sub(video.first_timestamp.unwrap()) as f64;
    let expected_ticks = 90_000.0 * elapsed;
    assert!(
        ticks > expected_ticks * 0.7 && ticks < expected_ticks * 1.3,
        "video RTP clock drifted: {ticks:.0} ticks over {elapsed:.1}s"
    );

    if has_audio {
        assert!(audio.packets > 0, "no audio arrived");
        assert_eq!(audio.sequence_gaps, 0, "audio sequence numbers must be contiguous");
    }
    assert!(rtcp_reports > 0, "expected periodic RTCP sender reports");

    let response = client.request("TEARDOWN", &client.base.clone(), &[]);
    assert_eq!(response.status, 200);
}

#[test]
fn streams_over_udp() {
    let Some(path) = test_file() else {
        eprintln!("skipping: no test media (set RTSP_UTILS_TEST_FILE)");
        return;
    };
    let server = start_server(&path);
    let mut client = Client::connect(server.addr, "test");

    // Bind the client's own even/odd RTP/RTCP pair.
    let (rtp_socket, rtcp_socket) = loop {
        let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = rtp.local_addr().unwrap().port();
        if port % 2 != 0 {
            continue;
        }
        if let Ok(rtcp) = UdpSocket::bind(format!("127.0.0.1:{}", port + 1)) {
            break (rtp, rtcp);
        }
    };
    let rtp_port = rtp_socket.local_addr().unwrap().port();
    rtp_socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    drop(rtcp_socket);

    client.request("DESCRIBE", &client.base.clone(), &[]);
    let response = client.setup(
        0,
        &format!("RTP/AVP;unicast;client_port={rtp_port}-{}", rtp_port + 1),
    );
    assert_eq!(response.status, 200, "SETUP over UDP");
    let transport = response.header("Transport").expect("Transport header");
    assert!(
        transport.contains("server_port="),
        "the server must report its own ports: {transport}"
    );

    let response = client.request("PLAY", &client.base.clone(), &[]);
    assert_eq!(response.status, 200);

    let mut buf = [0u8; 2048];
    let mut stats = TrackStats::default();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        match rtp_socket.recv(&mut buf) {
            Ok(len) => {
                let rtp = parse_rtp(&buf[..len]);
                assert_eq!(rtp.payload_type, 96);
                stats.record(&rtp);
            }
            Err(e) => panic!("no RTP over UDP: {e}"),
        }
    }

    assert!(stats.packets > 0, "no RTP datagrams arrived");
    assert_eq!(stats.sequence_gaps, 0, "loopback UDP should not reorder or drop");
}

/// The behaviour the web UI's Start/Stop button relies on: stopping a stream
/// disconnects whoever is watching and hides it from new clients, and starting
/// it again brings it back.
#[test]
fn stopping_a_stream_cuts_off_playback_and_hides_it() {
    let Some(path) = test_file() else {
        eprintln!("skipping: no test media (set RTSP_UTILS_TEST_FILE)");
        return;
    };
    let server = start_server(&path);
    let mut client = Client::connect(server.addr, "test");

    client.request("DESCRIBE", &client.base.clone(), &[]);
    client.setup(0, "RTP/AVP/TCP;unicast;interleaved=0-1");
    assert_eq!(
        client.request("PLAY", &client.base.clone(), &[]).status,
        200
    );

    // Confirm media really is flowing before we try to stop it.
    let mut media = 0;
    while media < 5 {
        if let Incoming::Frame(..) = client.read_incoming() {
            media += 1;
        }
    }

    let stream = server.registry.get("test").expect("the stream is registered");
    assert_eq!(stream.viewers(), 1, "the playing client is counted");

    server.control.stop("test").expect("stop the stream");

    // Let the session notice the flag and any in-flight packets drain.
    client.drain_for(Duration::from_millis(800));
    assert!(
        client.is_quiet_for(Duration::from_secs(1)),
        "RTP kept arriving after the stream was stopped"
    );

    // The viewer count releases once the playback thread winds down.
    let deadline = Instant::now() + Duration::from_secs(3);
    while stream.viewers() > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(stream.viewers(), 0, "the viewer count releases on stop");

    // A stopped stream is invisible to a fresh client.
    let mut newcomer = Client::connect(server.addr, "test");
    assert_eq!(
        newcomer.request("DESCRIBE", &newcomer.base.clone(), &[]).status,
        404,
        "a stopped stream should not be describable"
    );

    // Starting it again puts it back on air.
    server.control.start("test").expect("start the stream");
    let mut returning = Client::connect(server.addr, "test");
    assert_eq!(
        returning.request("DESCRIBE", &returning.base.clone(), &[]).status,
        200,
        "restarting should publish the stream again"
    );
}

#[test]
fn unknown_streams_are_rejected() {
    let Some(path) = test_file() else {
        eprintln!("skipping: no test media (set RTSP_UTILS_TEST_FILE)");
        return;
    };
    let server = start_server(&path);
    let mut client = Client::connect(server.addr, "test");

    let uri = format!("rtsp://{}/does-not-exist", server.addr);
    let response = client.request("DESCRIBE", &uri, &[]);
    assert_eq!(response.status, 404);

    // PLAY before SETUP has nothing to play.
    let response = client.request("PLAY", &client.base.clone(), &[]);
    assert_eq!(response.status, 455);
}
