use std::path::Path;
use std::sync::Arc;

use super::config::ServerConfig;
use super::registry::StreamRegistry;
use crate::domain::media::MediaSource;
use crate::domain::ports::MediaProbe;
use crate::domain::url::RtspUrl;
use crate::domain::{Error, Result};

/// Use case: take a file on disk and make it reachable at an RTSP URL.
pub struct PublishMedia<'a> {
    probe: &'a dyn MediaProbe,
    registry: &'a StreamRegistry,
    config: &'a ServerConfig,
}

impl<'a> PublishMedia<'a> {
    pub fn new(
        probe: &'a dyn MediaProbe,
        registry: &'a StreamRegistry,
        config: &'a ServerConfig,
    ) -> Self {
        Self {
            probe,
            registry,
            config,
        }
    }

    /// Probes `path`, registers it under `name` (defaulting to the file stem)
    /// and returns the URL clients should open.
    pub fn execute(&self, path: &Path, name: Option<&str>) -> Result<(Arc<MediaSource>, RtspUrl)> {
        if !path.is_file() {
            return Err(Error::NotFound(format!("{} is not a file", path.display())));
        }

        let name = match name {
            Some(n) => sanitize(n),
            None => sanitize(&path.file_stem().unwrap_or_default().to_string_lossy()),
        };
        if name.is_empty() {
            return Err(Error::Config(
                "the stream name is empty; pass --name explicitly".into(),
            ));
        }

        let source = self.probe.probe(path, &name)?;
        let url = RtspUrl::new(
            self.config.advertised_host.clone(),
            self.config.port(),
            name,
        );
        Ok((self.registry.publish(source), url))
    }
}

/// Keeps stream names to characters that survive a URL path unescaped.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn sanitize_keeps_safe_characters_and_replaces_the_rest() {
        assert_eq!(sanitize("91"), "91");
        assert_eq!(sanitize("my clip (final)"), "my_clip__final_");
        assert_eq!(sanitize("cam-1_front.h264"), "cam-1_front.h264");
    }
}
