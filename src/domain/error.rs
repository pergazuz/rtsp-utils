use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The container parsed, but holds something we cannot packetise.
    UnsupportedMedia(String),
    /// The container itself is broken or truncated.
    MalformedContainer(String),
    /// An RTSP peer sent something we cannot honour.
    Protocol(String),
    NotFound(String),
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::UnsupportedMedia(m) => write!(f, "unsupported media: {m}"),
            Error::MalformedContainer(m) => write!(f, "malformed container: {m}"),
            Error::Protocol(m) => write!(f, "rtsp protocol error: {m}"),
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::Config(m) => write!(f, "configuration error: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
