use std::fmt;

/// The address a client uses to pull a published stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtspUrl {
    pub host: String,
    pub port: u16,
    pub stream: String,
}

impl RtspUrl {
    pub fn new(host: impl Into<String>, port: u16, stream: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            stream: stream.into(),
        }
    }

}

impl fmt::Display for RtspUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Bracket IPv6 literals so the port stays parseable.
        if self.host.contains(':') {
            write!(f, "rtsp://[{}]:{}/{}", self.host, self.port, self.stream)
        } else {
            write!(f, "rtsp://{}:{}/{}", self.host, self.port, self.stream)
        }
    }
}
