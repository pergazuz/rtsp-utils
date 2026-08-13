//! End-to-end test for JPEG publishing: a baseline JPEG is built in the test,
//! probed through the same `AutoProbe` the binary wires up, served by the real
//! RTSP server, and pulled with a real client over a socket. Unlike the video
//! tests this one needs no sample media, so it never skips itself.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rtsp_utils::application::{ServerConfig, StreamControl, StreamRegistry};
use rtsp_utils::infrastructure::mp4::FileSampleReaderFactory;
use rtsp_utils::infrastructure::probe::AutoProbe;
use rtsp_utils::infrastructure::rtsp::RtspServer;

/// Long enough to see several frames at 5 fps.
const OBSERVE: Duration = Duration::from_secs(2);

// ---- a synthetic baseline JPEG ----------------------------------------------

const WIDTH: u16 = 64;
const HEIGHT: u16 = 48;

fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0xff, marker];
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// The entropy-coded scan: arbitrary bytes that never contain 0xff, so the
/// EOI search cannot trip over them. Long enough to force fragmentation.
fn scan_bytes() -> Vec<u8> {
    (0..4000u32).map(|i| (i % 251) as u8).collect()
}

/// A structurally valid 4:2:0 baseline JPEG with two quantization tables and
/// no DHT segments (an abbreviated stream implies the standard tables).
fn build_jpeg() -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    out.extend(segment(0xe0, b"JFIF\0"));

    for (id, fill) in [(0u8, 16u8), (1, 17)] {
        let mut body = vec![id];
        body.extend_from_slice(&[fill; 64]);
        out.extend(segment(0xdb, &body));
    }

    let mut sof = vec![8];
    sof.extend_from_slice(&HEIGHT.to_be_bytes());
    sof.extend_from_slice(&WIDTH.to_be_bytes());
    sof.push(3);
    sof.extend_from_slice(&[1, 0x22, 0]);
    sof.extend_from_slice(&[2, 0x11, 1]);
    sof.extend_from_slice(&[3, 0x11, 1]);
    out.extend(segment(0xc0, &sof));

    out.extend(segment(0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]));
    out.extend(scan_bytes());
    out.extend_from_slice(&[0xff, 0xd9]);
    out
}

/// Writes the JPEG to a temp path and removes it again on drop.
struct TempJpeg(PathBuf);

impl TempJpeg {
    fn create() -> Self {
        let path = std::env::temp_dir().join(format!(
            "rtsp-utils-jpeg-stream-{}.jpg",
            std::process::id()
        ));
        std::fs::write(&path, build_jpeg()).expect("write the test image");
        Self(path)
    }
}

impl Drop for TempJpeg {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn start_server(path: &std::path::Path) -> SocketAddr {
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        advertised_host: "127.0.0.1".into(),
        looping: true,
    };

    let registry = Arc::new(StreamRegistry::new());
    let control = Arc::new(StreamControl::new(
        Arc::new(AutoProbe),
        Arc::clone(&registry),
        config.clone(),
    ));
    control.add(path, Some("photo"), true).expect("probe the image");

    let server = RtspServer::bind(registry, config, Arc::new(FileSampleReaderFactory))
        .expect("bind an ephemeral port");
    let addr = server.local_addr().expect("read the bound address");
    thread::spawn(move || {
        let _ = server.run();
    });
    addr
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
}

// ---- the test ---------------------------------------------------------------

#[test]
fn a_jpeg_publishes_as_a_repeating_rtp_jpeg_stream() {
    let image = TempJpeg::create();
    let addr = start_server(&image.0);
    let mut client = Client::connect(addr, "photo");

    // DESCRIBE advertises static payload type 26 and the image dimensions.
    let response = client.request(
        "DESCRIBE",
        &client.base.clone(),
        &[("Accept", "application/sdp".into())],
    );
    assert_eq!(response.status, 200, "DESCRIBE should succeed");
    let sdp = &response.body;
    assert!(sdp.contains("m=video 0 RTP/AVP 26"), "SDP video line:\n{sdp}");
    assert!(sdp.contains("a=rtpmap:26 JPEG/90000"), "SDP rtpmap:\n{sdp}");
    assert!(
        sdp.contains(&format!("a=x-dimensions:{WIDTH},{HEIGHT}")),
        "SDP dimensions:\n{sdp}"
    );
    assert!(sdp.contains("a=control:trackID=0"), "SDP control:\n{sdp}");

    let response = client.request(
        "SETUP",
        &format!("{}/trackID=0", client.base),
        &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1".into())],
    );
    assert_eq!(response.status, 200, "SETUP should succeed");
    let session = response.header("Session").expect("SETUP opens a session");
    client.session = Some(session.split(';').next().unwrap_or(session).to_string());

    let response = client.request("PLAY", &client.base.clone(), &[]);
    assert_eq!(response.status, 200, "PLAY should succeed");

    // Collect frames: each is a run of packets with contiguous fragment
    // offsets, closed by the marker bit.
    let scan = scan_bytes();
    let started = Instant::now();
    let mut complete_frames = 0u32;
    let mut current: Vec<u8> = Vec::new();
    let mut first_packet_checked = false;

    while started.elapsed() < OBSERVE {
        let Incoming::Frame(channel, data) = client.read_incoming() else {
            panic!("unsolicited response while playing");
        };
        if channel != 0 {
            continue; // RTCP
        }

        assert_eq!(data[0] >> 6, 2, "RTP version");
        assert_eq!(data[1] & 0x7f, 26, "JPEG rides payload type 26");
        let marker = data[1] & 0x80 != 0;
        let payload = &data[12..];

        // RFC 2435 main header.
        let offset =
            (payload[1] as usize) << 16 | (payload[2] as usize) << 8 | payload[3] as usize;
        assert_eq!(payload[0], 0, "type-specific must be zero");
        assert_eq!(payload[4], 1, "type 1: 4:2:0 without restart markers");
        assert_eq!(payload[5], 255, "Q=255: tables travel in-band");
        assert_eq!(payload[6] as u16 * 8, WIDTH, "width in 8-pixel units");
        assert_eq!(payload[7] as u16 * 8, HEIGHT, "height in 8-pixel units");
        assert_eq!(offset, current.len(), "fragment offsets must be contiguous");

        let data_at = if offset == 0 {
            // The first fragment carries the quantization table header.
            assert_eq!(&payload[8..10], &[0, 0], "MBZ and 8-bit precision");
            let len = u16::from_be_bytes([payload[10], payload[11]]) as usize;
            assert_eq!(len, 128, "two 64-byte tables");
            assert!(payload[12..76].iter().all(|&b| b == 16), "the luma table");
            assert!(payload[76..140].iter().all(|&b| b == 17), "the chroma table");
            first_packet_checked = true;
            140
        } else {
            8
        };
        current.extend_from_slice(&payload[data_at..]);

        if marker {
            assert_eq!(current, scan, "a frame must rebuild the original scan");
            complete_frames += 1;
            current.clear();
        }
    }
    let elapsed = started.elapsed().as_secs_f64();

    assert!(first_packet_checked, "no first fragment was seen");
    assert!(complete_frames > 0, "no complete frame arrived");

    // The still must repeat at the advertised pace, not as fast as the disk
    // can serve it.
    let fps = complete_frames as f64 / elapsed;
    assert!(
        fps > 3.5 && fps < 6.5,
        "expected roughly 5 fps, got {fps:.1} ({complete_frames} frames in {elapsed:.1}s)"
    );

    let response = client.request("TEARDOWN", &client.base.clone(), &[]);
    assert_eq!(response.status, 200);
}
