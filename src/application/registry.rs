use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::domain::media::MediaSource;

/// The set of streams the server currently serves, keyed by URL path.
#[derive(Default)]
pub struct StreamRegistry {
    streams: RwLock<HashMap<String, Arc<MediaSource>>>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&self, source: MediaSource) -> Arc<MediaSource> {
        let source = Arc::new(source);
        if let Ok(mut streams) = self.streams.write() {
            streams.insert(source.name.clone(), Arc::clone(&source));
        }
        source
    }

    pub fn get(&self, name: &str) -> Option<Arc<MediaSource>> {
        self.streams.read().ok()?.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.streams
            .read()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }
}
