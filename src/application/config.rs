use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address the RTSP control port listens on.
    pub bind: SocketAddr,
    /// Host to put in the advertised URL; usually differs from `bind` when
    /// listening on 0.0.0.0.
    pub advertised_host: String,
    /// Restart the file from the top when it ends, so the stream never dies.
    pub looping: bool,
}

impl ServerConfig {
    pub fn port(&self) -> u16 {
        self.bind.port()
    }
}
