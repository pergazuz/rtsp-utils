use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use crate::domain::media::MediaSource;

/// A source that has been loaded, together with the runtime state the control
/// API and the RTSP server share: whether it is currently on air, and how many
/// clients are pulling it.
pub struct PublishedStream {
    pub source: Arc<MediaSource>,
    active: AtomicBool,
    viewers: AtomicUsize,
    started_at: RwLock<Option<SystemTime>>,
}

impl PublishedStream {
    pub fn new(source: MediaSource, active: bool) -> Self {
        let stream = Self {
            source: Arc::new(source),
            active: AtomicBool::new(false),
            viewers: AtomicUsize::new(0),
            started_at: RwLock::new(None),
        };
        if active {
            stream.start();
        }
        stream
    }

    pub fn name(&self) -> &str {
        &self.source.name
    }

    /// While inactive the stream stays loaded but is invisible to RTSP clients.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn start(&self) {
        self.active.store(true, Ordering::Relaxed);
        if let Ok(mut at) = self.started_at.write() {
            *at = Some(SystemTime::now());
        }
    }

    /// Playing sessions poll `is_active`, so they wind down on their own.
    pub fn stop(&self) {
        self.active.store(false, Ordering::Relaxed);
        if let Ok(mut at) = self.started_at.write() {
            *at = None;
        }
    }

    pub fn started_at(&self) -> Option<SystemTime> {
        self.started_at.read().ok().and_then(|at| *at)
    }

    pub fn viewers(&self) -> usize {
        self.viewers.load(Ordering::Relaxed)
    }

    /// Counts one client for as long as the returned guard lives.
    pub fn attach_viewer(self: &Arc<Self>) -> ViewerGuard {
        self.viewers.fetch_add(1, Ordering::Relaxed);
        ViewerGuard {
            stream: Arc::clone(self),
        }
    }
}

/// Decrements the viewer count when a session ends, however it ends.
pub struct ViewerGuard {
    stream: Arc<PublishedStream>,
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        // `fetch_update` so a double drop can never wrap the counter around.
        let _ = self
            .stream
            .viewers
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }
}

/// The set of streams the server knows about, keyed by URL path.
#[derive(Default)]
pub struct StreamRegistry {
    streams: RwLock<HashMap<String, Arc<PublishedStream>>>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a stream, replacing any earlier one with the same name.
    pub fn publish(&self, source: MediaSource, active: bool) -> Arc<PublishedStream> {
        let stream = Arc::new(PublishedStream::new(source, active));
        if let Ok(mut streams) = self.streams.write() {
            if let Some(previous) = streams.insert(stream.name().to_string(), Arc::clone(&stream)) {
                // Let anyone still playing the old source finish and detach.
                previous.stop();
            }
        }
        stream
    }

    pub fn get(&self, name: &str) -> Option<Arc<PublishedStream>> {
        self.streams.read().ok()?.get(name).cloned()
    }

    pub fn remove(&self, name: &str) -> Option<Arc<PublishedStream>> {
        let removed = self.streams.write().ok()?.remove(name);
        if let Some(stream) = &removed {
            stream.stop();
        }
        removed
    }

    /// All streams, ordered by name so the UI does not reshuffle on refresh.
    pub fn list(&self) -> Vec<Arc<PublishedStream>> {
        let Ok(streams) = self.streams.read() else {
            return Vec::new();
        };
        let mut all: Vec<Arc<PublishedStream>> = streams.values().cloned().collect();
        all.sort_by(|a, b| a.name().cmp(b.name()));
        all
    }

    pub fn names(&self) -> Vec<String> {
        self.list().iter().map(|s| s.name().to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn source(name: &str) -> MediaSource {
        MediaSource {
            name: name.to_string(),
            path: PathBuf::from("test.mov"),
            duration_secs: 1.0,
            tracks: Vec::new(),
        }
    }

    #[test]
    fn a_stream_can_be_published_stopped_and_restarted() {
        let registry = StreamRegistry::new();
        let stream = registry.publish(source("a"), true);

        assert!(stream.is_active());
        assert!(stream.started_at().is_some());

        stream.stop();
        assert!(!stream.is_active());
        assert!(stream.started_at().is_none());

        stream.start();
        assert!(stream.is_active());
    }

    #[test]
    fn publishing_inactive_leaves_the_stream_off_air() {
        let registry = StreamRegistry::new();
        assert!(!registry.publish(source("a"), false).is_active());
    }

    #[test]
    fn viewer_guards_count_up_and_release_on_drop() {
        let registry = StreamRegistry::new();
        let stream = registry.publish(source("a"), true);

        let first = stream.attach_viewer();
        let second = stream.attach_viewer();
        assert_eq!(stream.viewers(), 2);

        drop(first);
        assert_eq!(stream.viewers(), 1);
        drop(second);
        assert_eq!(stream.viewers(), 0);
    }

    #[test]
    fn republishing_a_name_takes_the_previous_stream_off_air() {
        let registry = StreamRegistry::new();
        let first = registry.publish(source("a"), true);
        let second = registry.publish(source("a"), true);

        assert!(!first.is_active(), "the replaced stream stops");
        assert!(second.is_active());
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn removing_a_stream_stops_it_and_drops_it_from_the_listing() {
        let registry = StreamRegistry::new();
        let stream = registry.publish(source("a"), true);

        assert!(registry.remove("a").is_some());
        assert!(!stream.is_active());
        assert!(registry.get("a").is_none());
        assert!(registry.remove("a").is_none());
    }

    #[test]
    fn listings_are_ordered_by_name() {
        let registry = StreamRegistry::new();
        registry.publish(source("charlie"), true);
        registry.publish(source("alpha"), true);
        registry.publish(source("bravo"), true);

        assert_eq!(registry.names(), vec!["alpha", "bravo", "charlie"]);
    }
}
