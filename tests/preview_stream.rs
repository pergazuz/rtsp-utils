//! End-to-end tests for the browser preview: the control API is started for
//! real and the fragmented-MP4 response is parsed with the project's own atom
//! reader, which is the same thing a browser's demuxer has to do.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rtsp_utils::application::{ServerConfig, StreamControl, StreamRegistry};
use rtsp_utils::infrastructure::http::{ApiServer, FileBrowser};
use rtsp_utils::infrastructure::mp4::{atom, FileSampleReaderFactory, Mp4Probe};

/// Long enough for several fragments at the 250 ms cadence.
const OBSERVE: Duration = Duration::from_secs(3);

fn test_file() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("RTSP_UTILS_TEST_FILE") {
        let path = PathBuf::from(configured);
        return path.is_file().then_some(path);
    }

    let mut candidates: Vec<PathBuf> = std::fs::read_dir(env!("CARGO_MANIFEST_DIR"))
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("mov") || e.eq_ignore_ascii_case("mp4"))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

/// Starts the control API with one stream on air, and returns its address.
fn start_api(active: bool) -> Option<SocketAddr> {
    let path = test_file()?;
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        advertised_host: "127.0.0.1".into(),
        looping: true,
    };

    let registry = Arc::new(StreamRegistry::new());
    let control = Arc::new(StreamControl::new(
        Arc::new(Mp4Probe),
        registry,
        config,
    ));
    control.add(&path, Some("test"), active).expect("probe the file");

    let browser = Arc::new(
        FileBrowser::new(&PathBuf::from(env!("CARGO_MANIFEST_DIR")), false)
            .expect("starting folder"),
    );
    let api = ApiServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        control,
        browser,
        Arc::new(FileSampleReaderFactory),
        None,
    )
    .expect("bind the api");
    let addr = api.local_addr().expect("bound address");
    thread::spawn(move || {
        let _ = api.run();
    });
    Some(addr)
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    reader: BufReader<TcpStream>,
}

/// Sends a bare HTTP/1.1 GET and returns the response with the body unread.
fn get(addr: SocketAddr, path: &str) -> Reply {
    let socket = TcpStream::connect(addr).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut writer = socket.try_clone().unwrap();
    write!(writer, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    writer.flush().unwrap();

    let mut reader = BufReader::new(socket);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).expect("status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("header");
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
    }

    Reply {
        status,
        headers,
        reader,
    }
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Reads chunked-transfer frames until `window` elapses, returning the
    /// reassembled body.
    fn read_chunked_for(&mut self, window: Duration) -> Vec<u8> {
        let deadline = Instant::now() + window;
        let mut body = Vec::new();

        while Instant::now() < deadline {
            let mut size_line = String::new();
            if self.reader.read_line(&mut size_line).is_err() {
                break;
            }
            let Ok(size) = usize::from_str_radix(size_line.trim(), 16) else {
                break;
            };
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size];
            if self.reader.read_exact(&mut chunk).is_err() {
                break;
            }
            body.extend_from_slice(&chunk);
            // Consume the CRLF that terminates the chunk.
            let mut crlf = [0u8; 2];
            let _ = self.reader.read_exact(&mut crlf);
        }
        body
    }
}

/// Lists the top-level box types in order.
fn box_types(buf: &[u8]) -> Vec<String> {
    atom::children(buf)
        .map(|a| String::from_utf8_lossy(&a.kind).into_owned())
        .collect()
}

#[test]
fn the_preview_is_a_playable_fragmented_mp4() {
    let Some(addr) = start_api(true) else {
        eprintln!("skipping: no test media (set RTSP_UTILS_TEST_FILE)");
        return;
    };

    let started = Instant::now();
    let mut reply = get(addr, "/api/streams/test/preview.mp4");

    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-type"), Some("video/mp4"));
    assert_eq!(reply.header("transfer-encoding"), Some("chunked"));

    let body = reply.read_chunked_for(OBSERVE);
    let elapsed = started.elapsed();
    assert!(!body.is_empty(), "no media arrived");

    let types = box_types(&body);
    assert_eq!(types.first().map(String::as_str), Some("ftyp"));
    assert_eq!(types.get(1).map(String::as_str), Some("moov"));

    // Everything after the init segment must be moof/mdat pairs, in that
    // order; a browser rejects anything else.
    let fragments: Vec<&String> = types.iter().skip(2).collect();
    assert!(
        fragments.len() >= 4,
        "expected several fragments in {elapsed:?}, got {}",
        fragments.len()
    );
    for pair in fragments.chunks(2) {
        assert_eq!(pair[0], "moof");
        if pair.len() > 1 {
            assert_eq!(pair[1], "mdat");
        }
    }

    // The init segment has to carry the decoder setup.
    let moov = atom::find(&body, b"moov").expect("moov");
    assert!(
        atom::find_path(moov, &[b"mvex", b"trex"]).is_some(),
        "a fragmented file must declare mvex/trex"
    );
    let trak = atom::find(moov, b"trak").expect("trak");
    assert!(atom::find_path(trak, &[b"mdia", b"minf", b"stbl", b"stsd"]).is_some());

    // Decode times must advance monotonically across fragments, or playback
    // stalls.
    let mut decode_times = Vec::new();
    for fragment in atom::children(&body).filter(|a| &a.kind == b"moof") {
        let traf = atom::find(fragment.body, b"traf").expect("traf");
        let tfdt = atom::find(traf, b"tfdt").expect("tfdt");
        decode_times.push(atom::u64_at(tfdt, 4).unwrap());
    }
    assert!(
        decode_times.windows(2).all(|w| w[1] > w[0]),
        "decode times must increase: {decode_times:?}"
    );
    assert_eq!(decode_times[0], 0, "the first fragment starts at zero");
}

#[test]
fn the_preview_is_paced_rather_than_dumped() {
    let Some(addr) = start_api(true) else {
        eprintln!("skipping: no test media (set RTSP_UTILS_TEST_FILE)");
        return;
    };

    let mut reply = get(addr, "/api/streams/test/preview.mp4");
    let body = reply.read_chunked_for(OBSERVE);

    // Media time delivered is read from the last fragment's decode time, in
    // the track's timescale, which the init segment declares.
    let moov = atom::find(&body, b"moov").expect("moov");
    let trak = atom::find(moov, b"trak").expect("trak");
    let mdhd = atom::find_path(trak, &[b"mdia", b"mdhd"]).expect("mdhd");
    let timescale = atom::u32_at(mdhd, 12).unwrap() as f64;

    let last = atom::children(&body)
        .filter(|a| &a.kind == b"moof")
        .last()
        .expect("at least one fragment");
    let traf = atom::find(last.body, b"traf").expect("traf");
    let tfdt = atom::find(traf, b"tfdt").expect("tfdt");
    let media_seconds = atom::u64_at(tfdt, 4).unwrap() as f64 / timescale;

    // Roughly one second of media per second of wall clock: a dump would run
    // far ahead, a stall far behind.
    let window = OBSERVE.as_secs_f64();
    assert!(
        media_seconds > window * 0.5 && media_seconds < window * 1.5,
        "delivered {media_seconds:.1}s of media in {window:.1}s of wall clock"
    );
}

#[test]
fn a_stopped_stream_has_nothing_to_preview() {
    let Some(addr) = start_api(false) else {
        eprintln!("skipping: no test media (set RTSP_UTILS_TEST_FILE)");
        return;
    };

    let reply = get(addr, "/api/streams/test/preview.mp4");
    assert_eq!(reply.status, 404);

    let reply = get(addr, "/api/streams/nope/preview.mp4");
    assert_eq!(reply.status, 404);
}
