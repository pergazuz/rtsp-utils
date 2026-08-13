//! RTSP/1.0 request parsing and response building (RFC 2326).

use std::io::BufRead;

use crate::domain::{Error, Result};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn cseq(&self) -> &str {
        self.header("CSeq").unwrap_or("0")
    }

    /// Path component of the request URI, without the leading slash.
    pub fn path(&self) -> String {
        let without_scheme = match self.uri.find("://") {
            Some(i) => &self.uri[i + 3..],
            None => self.uri.as_str(),
        };
        let path = match without_scheme.find('/') {
            Some(i) => &without_scheme[i + 1..],
            None => "",
        };
        path.trim_end_matches('/').to_string()
    }

    /// Splits the path into the stream name and, if present, the track control
    /// suffix a client appends on SETUP (`trackID=0` / `streamid=0`).
    pub fn stream_and_track(&self) -> (String, Option<usize>) {
        let path = self.path();
        for marker in ["/trackID=", "/streamid=", "/track="] {
            if let Some(i) = path.rfind(marker) {
                let stream = path[..i].to_string();
                let track = path[i + marker.len()..].parse::<usize>().ok();
                return (stream, track);
            }
        }
        (path, None)
    }
}

/// Reads one request, skipping any interleaved binary frames the client sends
/// back on the control connection. `Ok(None)` means the peer hung up.
pub fn read_request<R: BufRead>(reader: &mut R) -> Result<Option<Request>> {
    let first = loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte)? {
            0 => return Ok(None),
            _ => {}
        }
        if byte[0] != b'$' {
            break byte[0];
        }
        // '$' introduces an interleaved RTP/RTCP frame: channel + 16-bit length.
        let mut header = [0u8; 3];
        reader.read_exact(&mut header)?;
        let len = u16::from_be_bytes([header[1], header[2]]) as usize;
        let mut discard = vec![0u8; len];
        reader.read_exact(&mut discard)?;
    };

    let mut request_line = String::new();
    request_line.push(first as char);
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }

    let mut parts = request_line.trim_end().split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| Error::Protocol("empty request line".into()))?
        .to_ascii_uppercase();
    let uri = parts.next().unwrap_or("*").to_string();

    let mut headers = Vec::new();
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
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);

    // No method we implement carries a meaningful body, but it still has to be
    // drained so the next request starts at the right offset.
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Request {
        method,
        uri,
        headers,
    }))
}

pub struct Response {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, reason: &str, cseq: &str) -> Self {
        Self {
            status,
            reason: reason.to_string(),
            headers: vec![
                ("CSeq".into(), cseq.to_string()),
                ("Server".into(), "rtsp-utils/0.1".into()),
            ],
            body: Vec::new(),
        }
    }

    pub fn ok(cseq: &str) -> Self {
        Self::new(200, "OK", cseq)
    }

    pub fn error(status: u16, reason: &str, cseq: &str) -> Self {
        Self::new(status, reason, cseq)
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn body(mut self, content_type: &str, body: Vec<u8>) -> Self {
        self.headers
            .push(("Content-Type".into(), content_type.to_string()));
        self.headers
            .push(("Content-Length".into(), body.len().to_string()));
        self.body = body;
        self
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("RTSP/1.0 {} {}\r\n", self.status, self.reason).into_bytes();
        for (name, value) in &self.headers {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(bytes: &[u8]) -> Option<Request> {
        read_request(&mut Cursor::new(bytes.to_vec())).expect("parses")
    }

    #[test]
    fn parses_a_request_with_headers() {
        let request = parse(
            b"DESCRIBE rtsp://127.0.0.1:8555/91 RTSP/1.0\r\n\
              CSeq: 2\r\n\
              Accept: application/sdp\r\n\r\n",
        )
        .expect("a request");

        assert_eq!(request.method, "DESCRIBE");
        assert_eq!(request.cseq(), "2");
        assert_eq!(request.header("accept"), Some("application/sdp"));
        assert_eq!(request.path(), "91");
    }

    #[test]
    fn skips_interleaved_frames_that_precede_a_request() {
        // '$', channel 0, four payload bytes, then a real request.
        let mut bytes = vec![b'$', 0x00, 0x00, 0x04, 0xde, 0xad, 0xbe, 0xef];
        bytes.extend_from_slice(b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n");

        let request = parse(&bytes).expect("a request");
        assert_eq!(request.method, "OPTIONS");
        assert_eq!(request.cseq(), "1");
    }

    #[test]
    fn splits_the_track_control_suffix_off_the_stream_name() {
        let request = parse(b"SETUP rtsp://h/91/trackID=1 RTSP/1.0\r\nCSeq: 3\r\n\r\n").unwrap();
        assert_eq!(request.stream_and_track(), ("91".to_string(), Some(1)));

        let request = parse(b"SETUP rtsp://h/91/streamid=0 RTSP/1.0\r\nCSeq: 3\r\n\r\n").unwrap();
        assert_eq!(request.stream_and_track(), ("91".to_string(), Some(0)));

        let request = parse(b"DESCRIBE rtsp://h/91 RTSP/1.0\r\nCSeq: 3\r\n\r\n").unwrap();
        assert_eq!(request.stream_and_track(), ("91".to_string(), None));
    }

    #[test]
    fn a_closed_connection_yields_no_request() {
        assert!(parse(b"").is_none());
    }

    #[test]
    fn responses_serialise_with_crlf_and_content_length() {
        let bytes = Response::ok("7")
            .body("application/sdp", b"v=0\r\n".to_vec())
            .to_bytes();
        let text = String::from_utf8(bytes).unwrap();

        assert!(text.starts_with("RTSP/1.0 200 OK\r\n"));
        assert!(text.contains("CSeq: 7\r\n"));
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.ends_with("\r\n\r\nv=0\r\n"));
    }
}
