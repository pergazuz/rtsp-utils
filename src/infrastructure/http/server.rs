//! The HTTP control API the web UI drives, plus optional hosting of the built
//! UI itself so a single binary serves both.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;

use super::files::FileBrowser;
use super::json::{self, Json};
use super::{dto, mime};
use crate::application::{preview, StreamControl};
use crate::domain::media::CodecParams;
use crate::domain::ports::{ByteSink, SampleReaderFactory};
use crate::domain::{Error, Result};
use crate::infrastructure::fmp4::Fmp4Fragmenter;

/// Refuse absurd request bodies rather than allocating whatever is asked for.
const MAX_BODY: usize = 1 << 20;

pub struct ApiServer {
    listener: TcpListener,
    control: Arc<StreamControl>,
    browser: Arc<FileBrowser>,
    readers: Arc<dyn SampleReaderFactory>,
    /// Directory holding the built web UI, if it should be served.
    ui_dir: Option<PathBuf>,
}

impl ApiServer {
    pub fn bind(
        addr: SocketAddr,
        control: Arc<StreamControl>,
        browser: Arc<FileBrowser>,
        readers: Arc<dyn SampleReaderFactory>,
        ui_dir: Option<PathBuf>,
    ) -> Result<Self> {
        Ok(Self {
            listener: TcpListener::bind(addr)?,
            control,
            browser,
            readers,
            ui_dir,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    pub fn serves_ui(&self) -> bool {
        self.ui_dir.is_some()
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
                        if let Err(e) = server.handle(stream) {
                            eprintln!("[api] {e}");
                        }
                    });
                }
                Err(e) => eprintln!("[api] accept failed: {e}"),
            }
        }
        Ok(())
    }

    fn handle(&self, stream: TcpStream) -> Result<()> {
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);

        let Some(request) = Request::read(&mut reader)? else {
            return Ok(());
        };

        if request.method == "GET" {
            match preview_target(&request.path) {
                // The video preview is an open-ended response, so it takes
                // over the socket instead of going through request/response.
                Some((name, PreviewKind::Video)) => return self.stream_preview(&name, writer),
                Some((name, PreviewKind::Image)) => {
                    let response = self.image_preview(&name);
                    writer.write_all(&response.to_bytes())?;
                    writer.flush()?;
                    return Ok(());
                }
                None => {}
            }
        }

        let response = self.route(&request);
        writer.write_all(&response.to_bytes())?;
        writer.flush()?;
        Ok(())
    }

    fn route(&self, request: &Request) -> Response {
        // The UI is served from a different origin during development.
        if request.method == "OPTIONS" {
            return Response::no_content();
        }

        let path = request.path.as_str();
        if let Some(rest) = path.strip_prefix("/api/") {
            return self.route_api(request, rest);
        }
        self.serve_ui(path)
    }

    fn route_api(&self, request: &Request, rest: &str) -> Response {
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();

        let result = match (request.method.as_str(), segments.as_slice()) {
            ("GET", ["health"]) => Ok(dto::health(
                self.control.config(),
                self.control.list().len(),
                &self.browser,
            )),

            ("GET", ["streams"]) => Ok(dto::streams(&self.control.list())),

            ("POST", ["streams"]) => self.add_stream(request),

            // The picker browses one directory at a time.
            ("GET", ["files"]) => {
                let path = request.query("path").unwrap_or_default();
                self.browser
                    .browse(&path)
                    .map(|listing| dto::listing(&listing, &self.browser))
            }

            ("POST", ["streams", name, "start"]) => decode(name)
                .and_then(|name| self.control.start(&name))
                .and_then(|s| self.control.status(s.name()))
                .map(|status| dto::stream(&status)),

            ("POST", ["streams", name, "stop"]) => decode(name)
                .and_then(|name| self.control.stop(&name))
                .and_then(|s| self.control.status(s.name()))
                .map(|status| dto::stream(&status)),

            ("GET", ["streams", name]) => decode(name)
                .and_then(|name| self.control.status(&name))
                .map(|status| dto::stream(&status)),

            ("DELETE", ["streams", name]) => decode(name)
                .and_then(|name| self.control.remove(&name))
                .map(|_| Json::object(vec![("ok", Json::Bool(true))])),

            _ => Err(Error::NotFound(format!(
                "no route for {} /api/{rest}",
                request.method
            ))),
        };

        match result {
            Ok(body) => Response::json(200, &body),
            Err(e) => {
                eprintln!("[api] {} /api/{rest}: {e}", request.method);
                Response::json(status_for(&e), &dto::error(&e.to_string()))
            }
        }
    }

    fn add_stream(&self, request: &Request) -> Result<Json> {
        let body = json::parse(&String::from_utf8_lossy(&request.body))?;
        let path = body
            .get("path")
            .and_then(Json::as_str)
            .ok_or_else(|| Error::Config("the request needs a 'path' field".into()))?;
        let name = body.get("name").and_then(Json::as_str).filter(|n| !n.is_empty());
        // Loading a file without putting it on air is a legitimate thing to
        // want, so `start` is opt-out rather than implied.
        let start = body.get("start").and_then(Json::as_bool).unwrap_or(true);

        // Resolved through the browser so confinement, when enabled, applies
        // to publishing as well as to listing.
        let resolved = self.browser.resolve(path)?;

        let stream = self.control.add(&resolved, name, start)?;
        let status = self.control.status(stream.name())?;
        Ok(dto::stream(&status))
    }

    /// Streams the video track as fragmented MP4 for as long as the browser
    /// keeps listening.
    fn stream_preview(&self, name: &str, mut writer: TcpStream) -> Result<()> {
        let Some(stream) = self.control.stream(name).filter(|s| s.is_active()) else {
            let response = Response::json(
                404,
                &dto::error("no stream by that name is currently running"),
            );
            writer.write_all(&response.to_bytes())?;
            return Ok(());
        };

        let Some((params, timescale)) = stream.source.tracks.iter().find_map(|t| match &t.codec {
            CodecParams::H264(p) => Some((p.clone(), t.timescale)),
            _ => None,
        }) else {
            let response = Response::json(
                415,
                &dto::error("this stream has no H.264 video to preview"),
            );
            writer.write_all(&response.to_bytes())?;
            return Ok(());
        };

        let reader = self.readers.open(&stream.source)?;
        writer.write_all(CHUNKED_MP4_HEADERS.as_bytes())?;
        writer.flush()?;

        let mut fragmenter = Fmp4Fragmenter::new(params, timescale);
        let mut sink = ChunkedSink { writer };
        let cancel = AtomicBool::new(false);
        let looping = self.control.config().looping;

        println!("[preview] {name}: browser attached");
        let outcome = preview::run(
            stream,
            reader,
            &mut fragmenter,
            &mut sink,
            looping,
            &cancel,
        );
        if let Err(e) = &outcome {
            eprintln!("[preview] {name}: {e}");
        }
        println!("[preview] {name}: browser detached");

        sink.finish();
        Ok(())
    }

    /// One frame of a JPEG stream, which is every frame: the browser gets the
    /// original file, since RTP resends the same image over and over.
    fn image_preview(&self, name: &str) -> Response {
        let Some(stream) = self.control.stream(name).filter(|s| s.is_active()) else {
            return Response::json(
                404,
                &dto::error("no stream by that name is currently running"),
            );
        };

        let is_jpeg = stream
            .source
            .tracks
            .iter()
            .any(|t| matches!(t.codec, CodecParams::Jpeg(_)));
        if !is_jpeg {
            return Response::json(415, &dto::error("this stream is not a still image"));
        }

        match std::fs::read(&stream.source.path) {
            Ok(bytes) => Response::bytes(200, "image/jpeg", bytes),
            Err(e) => Response::json(500, &dto::error(&format!("cannot read the image: {e}"))),
        }
    }

    /// Serves the built UI, falling back to `index.html` so client-side routes
    /// survive a page reload.
    fn serve_ui(&self, path: &str) -> Response {
        let Some(root) = &self.ui_dir else {
            return Response::json(
                404,
                &dto::error("no web UI is being served; build it with `bun run build`"),
            );
        };

        let relative = path.trim_start_matches('/');
        let relative = if relative.is_empty() { "index.html" } else { relative };

        if let Some(file) = safe_join(root, relative) {
            if let Ok(bytes) = std::fs::read(&file) {
                return Response::bytes(200, mime::for_path(&file), bytes);
            }
        }

        match std::fs::read(root.join("index.html")) {
            Ok(bytes) => Response::bytes(200, "text/html; charset=utf-8", bytes),
            Err(_) => Response::json(404, &dto::error("not found")),
        }
    }
}

/// Headers for the open-ended fMP4 response. `no-store` matters: a cached
/// live stream is a stale one.
const CHUNKED_MP4_HEADERS: &str = "HTTP/1.1 200 OK\r\n\
     Content-Type: video/mp4\r\n\
     Transfer-Encoding: chunked\r\n\
     Cache-Control: no-store\r\n\
     Access-Control-Allow-Origin: *\r\n\
     Connection: close\r\n\r\n";

/// Writes HTTP chunked-transfer frames to a socket.
struct ChunkedSink {
    writer: TcpStream,
}

impl ChunkedSink {
    fn finish(&mut self) {
        // The terminating zero-length chunk; a broken pipe here is expected
        // whenever the browser left first.
        let _ = self.writer.write_all(b"0\r\n\r\n");
        let _ = self.writer.flush();
    }
}

impl ByteSink for ChunkedSink {
    fn write_chunk(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.writer
            .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())?;
        self.writer.write_all(bytes)?;
        self.writer.write_all(b"\r\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

enum PreviewKind {
    Video,
    Image,
}

/// Matches `/api/streams/{name}/preview.mp4` or `.../preview.jpg` and returns
/// the decoded name with the kind of preview asked for.
fn preview_target(path: &str) -> Option<(String, PreviewKind)> {
    let rest = path.strip_prefix("/api/streams/")?;
    let (name, kind) = if let Some(name) = rest.strip_suffix("/preview.mp4") {
        (name, PreviewKind::Video)
    } else if let Some(name) = rest.strip_suffix("/preview.jpg") {
        (name, PreviewKind::Image)
    } else {
        return None;
    };
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some((percent_decode(name), kind))
}

/// Joins a request path onto the UI root, refusing anything that would escape
/// it via `..` or an absolute path.
fn safe_join(root: &Path, relative: &str) -> Option<PathBuf> {
    let candidate = Path::new(relative);
    if candidate
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let joined = root.join(candidate);
    joined.is_file().then_some(joined)
}

fn decode(value: &str) -> Result<String> {
    Ok(percent_decode(value))
}

/// Minimal percent-decoding for path segments; stream names are restricted to
/// URL-safe characters, but clients still escape them.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn status_for(error: &Error) -> u16 {
    match error {
        Error::NotFound(_) => 404,
        Error::Config(_) | Error::Protocol(_) => 400,
        Error::UnsupportedMedia(_) | Error::MalformedContainer(_) => 422,
        Error::Io(_) => 500,
    }
}

// ---- HTTP plumbing ----------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

impl Request {
    /// Value of a query parameter, percent-decoded.
    fn query(&self, key: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            // '+' means a space in a query string, unlike in a path.
            (name == key).then(|| percent_decode(&value.replace('+', " ")))
        })
    }

    fn read(reader: &mut BufReader<TcpStream>) -> Result<Option<Request>> {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(None);
        }

        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_ascii_uppercase();
        let target = parts.next().unwrap_or("/").to_string();
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path.to_string(), query.to_string()),
            None => (target, String::new()),
        };

        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("Content-Length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }

        if content_length > MAX_BODY {
            return Err(Error::Protocol(format!(
                "request body of {content_length} bytes is too large"
            )));
        }

        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body)?;
        }

        Ok(Some(Request {
            method,
            path,
            query,
            body,
        }))
    }
}

struct Response {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

impl Response {
    fn json(status: u16, body: &Json) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8".into(),
            body: body.to_json_string().into_bytes(),
        }
    }

    fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: content_type.to_string(),
            body,
        }
    }

    fn no_content() -> Self {
        Self {
            status: 204,
            content_type: "text/plain".into(),
            body: Vec::new(),
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            404 => "Not Found",
            415 => "Unsupported Media Type",
            422 => "Unprocessable Entity",
            _ => "Internal Server Error",
        };

        let mut out = format!(
            "HTTP/1.1 {} {reason}\r\n\
             Content-Type: {}\r\n\
             Content-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Methods: GET, POST, DELETE, OPTIONS\r\n\
             Access-Control-Allow-Headers: Content-Type\r\n\
             Cache-Control: no-store\r\n\
             Connection: close\r\n\r\n",
            self.status,
            self.content_type,
            self.body.len()
        )
        .into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(percent_decode("cam%201"), "cam 1");
        assert_eq!(percent_decode("plain"), "plain");
        // A stray '%' is left alone rather than swallowing the next characters.
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn path_traversal_is_refused() {
        let root = Path::new("web/dist");
        assert!(safe_join(root, "../../Cargo.toml").is_none());
        assert!(safe_join(root, "/etc/passwd").is_none());
        assert!(safe_join(root, "assets/../../secret").is_none());
    }

    #[test]
    fn errors_map_onto_sensible_status_codes() {
        assert_eq!(status_for(&Error::NotFound("x".into())), 404);
        assert_eq!(status_for(&Error::Config("x".into())), 400);
        assert_eq!(status_for(&Error::UnsupportedMedia("x".into())), 422);
    }
}
