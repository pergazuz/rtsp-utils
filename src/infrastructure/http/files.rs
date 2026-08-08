//! Lets the UI browse video files on the server.
//!
//! By default the whole machine is reachable, which is what makes the picker
//! useful — media rarely lives next to the binary. `confined` restores a hard
//! boundary at the starting folder for deployments that want one.

use std::path::{Component, Path, PathBuf};

use crate::domain::{Error, Result};

/// Extensions offered in the picker.
const VIDEO_EXTENSIONS: [&str; 3] = ["mov", "mp4", "m4v"];

/// Ceiling on entries per listing, so a folder with tens of thousands of files
/// cannot wedge the UI. Truncation is reported rather than hidden.
const MAX_ENTRIES: usize = 1000;

pub struct Entry {
    pub name: String,
    /// Absolute path, which is what the client sends back to select it.
    pub path: String,
    pub directory: bool,
    pub size: u64,
}

/// One clickable step in a breadcrumb trail, or one drive.
pub struct Segment {
    pub label: String,
    pub path: String,
}

pub struct Listing {
    /// Absolute path of the directory being listed.
    pub path: String,
    /// Absolute path of the parent, or None at a filesystem root.
    pub parent: Option<String>,
    pub segments: Vec<Segment>,
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

pub struct FileBrowser {
    /// Where the picker opens.
    start: PathBuf,
    /// When set, nothing outside `start` may be listed or published.
    confined: bool,
    /// Filesystem roots, probed once at startup.
    roots: Vec<Segment>,
}

impl FileBrowser {
    pub fn new(start: &Path, confined: bool) -> Result<Self> {
        let start = normalize(start).map_err(|e| {
            Error::Config(format!(
                "cannot use {} as the starting folder: {e}",
                start.display()
            ))
        })?;
        if !start.is_dir() {
            return Err(Error::Config(format!(
                "{} is not a directory",
                start.display()
            )));
        }
        Ok(Self {
            start,
            confined,
            roots: filesystem_roots(),
        })
    }

    pub fn start_display(&self) -> String {
        display_path(&self.start)
    }

    pub fn is_confined(&self) -> bool {
        self.confined
    }

    /// Drives on Windows, `/` elsewhere. Empty while confined, since there is
    /// nowhere else to go.
    pub fn roots(&self) -> &[Segment] {
        if self.confined {
            &[]
        } else {
            &self.roots
        }
    }

    /// Lists the directories and video files inside `requested`. An empty path
    /// means the starting folder.
    pub fn browse(&self, requested: &str) -> Result<Listing> {
        let directory = self.resolve_directory(requested)?;

        let read = std::fs::read_dir(&directory).map_err(|e| {
            // Plenty of system folders are simply not readable; say so rather
            // than reporting an internal error.
            Error::Config(format!("cannot open {}: {e}", display_path(&directory)))
        })?;

        let mut entries: Vec<Entry> = Vec::new();
        let mut truncated = false;

        for entry in read {
            let Ok(entry) = entry else { continue };
            let Ok(metadata) = entry.metadata() else {
                // Symlinks to nowhere and locked files are skipped silently.
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }

            let is_dir = metadata.is_dir();
            if !is_dir && !is_video(&entry.path()) {
                continue;
            }

            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                break;
            }

            entries.push(Entry {
                path: display_path(&entry.path()),
                name,
                directory: is_dir,
                size: if is_dir { 0 } else { metadata.len() },
            });
        }

        // Directories first, then files, each alphabetically.
        entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        let parent = directory
            .parent()
            .map(|p| display_path(p))
            .filter(|p| !p.is_empty())
            // Confinement stops the trail at the starting folder.
            .filter(|_| !self.confined || directory != self.start);

        Ok(Listing {
            path: display_path(&directory),
            parent,
            segments: breadcrumbs(&directory),
            entries,
            truncated,
        })
    }

    /// Turns a client-supplied path into a real one, or fails.
    pub fn resolve(&self, requested: &str) -> Result<PathBuf> {
        let resolved = normalize(&self.join(requested)).map_err(|_| {
            Error::NotFound(format!("{requested} does not exist on the server"))
        })?;
        self.ensure_allowed(&resolved)?;
        Ok(resolved)
    }

    fn resolve_directory(&self, requested: &str) -> Result<PathBuf> {
        let resolved = normalize(&self.join(requested))
            .map_err(|_| Error::NotFound(format!("no directory at {requested}")))?;
        self.ensure_allowed(&resolved)?;
        if !resolved.is_dir() {
            return Err(Error::Config(format!("{requested} is not a directory")));
        }
        Ok(resolved)
    }

    fn join(&self, requested: &str) -> PathBuf {
        let trimmed = requested.trim();
        if trimmed.is_empty() {
            return self.start.clone();
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.start.join(path)
        }
    }

    /// `resolved` must already be normalised, or this comparison is meaningless.
    fn ensure_allowed(&self, resolved: &Path) -> Result<()> {
        if !self.confined || resolved.starts_with(&self.start) {
            Ok(())
        } else {
            Err(Error::Config(format!(
                "{} is outside {}",
                display_path(resolved),
                self.start_display()
            )))
        }
    }
}

/// Probes the drive letters that are actually mounted.
fn filesystem_roots() -> Vec<Segment> {
    #[cfg(windows)]
    {
        ('A'..='Z')
            .filter_map(|letter| {
                let path = PathBuf::from(format!("{letter}:\\"));
                path.is_dir().then(|| Segment {
                    label: format!("{letter}:"),
                    path: display_path(&path),
                })
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![Segment {
            label: "/".to_string(),
            path: "/".to_string(),
        }]
    }
}

/// Splits an absolute path into clickable steps, each carrying the path it
/// navigates to.
fn breadcrumbs(path: &Path) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut accumulated = PathBuf::new();
    let mut components = path.components().peekable();

    // A Windows path opens with a prefix and a root separator; together they
    // make one segment, `C:`.
    if let Some(Component::Prefix(prefix)) = components.peek().copied() {
        accumulated.push(prefix.as_os_str());
        components.next();
        if matches!(components.peek(), Some(Component::RootDir)) {
            accumulated.push(Component::RootDir.as_os_str());
            components.next();
        }
        out.push(Segment {
            label: prefix.as_os_str().to_string_lossy().into_owned(),
            path: display_path(&accumulated),
        });
    } else if matches!(components.peek(), Some(Component::RootDir)) {
        accumulated.push(Component::RootDir.as_os_str());
        components.next();
        out.push(Segment {
            label: "/".to_string(),
            path: display_path(&accumulated),
        });
    }

    for component in components {
        if let Component::Normal(name) = component {
            accumulated.push(name);
            out.push(Segment {
                label: name.to_string_lossy().into_owned(),
                path: display_path(&accumulated),
            });
        }
    }
    out
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Resolves a path and strips the Windows verbatim prefix.
///
/// Every path in this module goes through here, so containment checks compare
/// like with like and breadcrumbs never read `\\?\C:`.
fn normalize(path: &Path) -> std::io::Result<PathBuf> {
    Ok(PathBuf::from(display_path(&path.canonicalize()?)))
}

/// Windows canonicalisation yields a `\\?\` prefix that means nothing to a
/// person reading it.
fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crate_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn open() -> FileBrowser {
        FileBrowser::new(&crate_root(), false).unwrap()
    }

    fn confined() -> FileBrowser {
        FileBrowser::new(&crate_root(), true).unwrap()
    }

    #[test]
    fn only_video_extensions_are_offered() {
        assert!(is_video(Path::new("clip.mov")));
        assert!(is_video(Path::new("clip.MP4")));
        assert!(is_video(Path::new("clip.m4v")));
        assert!(!is_video(Path::new("notes.txt")));
        assert!(!is_video(Path::new("noextension")));
    }

    #[test]
    fn browsing_starts_in_the_configured_folder() {
        let listing = open().browse("").unwrap();
        assert_eq!(listing.path, display_path(&normalize(&crate_root()).unwrap()));
        assert!(listing.entries.iter().any(|e| e.name == "src"));
    }

    #[test]
    fn listings_put_directories_before_files() {
        let listing = open().browse("").unwrap();
        let first_file = listing.entries.iter().position(|e| !e.directory);
        let last_dir = listing.entries.iter().rposition(|e| e.directory);
        if let (Some(file), Some(dir)) = (first_file, last_dir) {
            assert!(dir < file, "directories should sort ahead of files");
        }
        assert!(listing
            .entries
            .iter()
            .all(|e| e.directory || is_video(Path::new(&e.name))));
    }

    #[test]
    fn entries_carry_absolute_paths_that_resolve() {
        let browser = open();
        let listing = browser.browse("").unwrap();
        let src = listing
            .entries
            .iter()
            .find(|e| e.name == "src")
            .expect("src");

        assert!(Path::new(&src.path).is_absolute());
        // Feeding an entry's path straight back must work; that is how the
        // picker navigates.
        assert!(browser.browse(&src.path).is_ok());
    }

    #[test]
    fn the_whole_machine_is_reachable_by_default() {
        let browser = open();

        // Up and out of the crate entirely.
        let parent = browser.browse("..").expect("the parent must be listable");
        assert!(!parent.path.ends_with("rtsp-utils"));

        // And an absolute path elsewhere on the machine.
        for candidate in [r"C:\Windows", "/usr", "/etc"] {
            if Path::new(candidate).is_dir() {
                assert!(
                    browser.browse(candidate).is_ok(),
                    "{candidate} should be browsable"
                );
                return;
            }
        }
    }

    #[test]
    fn a_filesystem_root_has_no_parent() {
        let browser = open();
        let Some(root) = browser.roots().first() else {
            return;
        };
        let listing = browser.browse(&root.path).unwrap();
        assert!(listing.parent.is_none(), "nothing sits above a drive");
        assert_eq!(listing.segments.len(), 1);
    }

    #[test]
    fn confinement_still_refuses_paths_outside_the_start() {
        let browser = confined();

        assert!(browser.browse("..").is_err());
        assert!(browser.resolve("../../../..").is_err());
        for outside in [r"C:\Windows", "/etc"] {
            if Path::new(outside).is_dir() {
                assert!(browser.resolve(outside).is_err());
            }
        }
        // Inside is still fine.
        assert!(browser.resolve("Cargo.toml").is_ok());
        assert!(browser.browse("src").is_ok());
        // With nowhere else to go, no drive switcher is offered.
        assert!(browser.roots().is_empty());
    }

    #[test]
    fn the_confined_start_folder_is_the_top_of_the_trail() {
        let listing = confined().browse("").unwrap();
        assert!(listing.parent.is_none(), "confinement caps the trail");
    }

    #[test]
    fn the_first_breadcrumb_names_the_drive_cleanly() {
        // Canonicalised Windows paths carry a `\\?\` prefix that must not
        // reach the UI as a breadcrumb label.
        let trail = open().browse("").unwrap().segments;
        let first = trail.first().expect("at least one segment");

        assert!(
            !first.label.contains('?') && !first.label.contains('\\'),
            "the root segment should read like a drive, got {:?}",
            first.label
        );
        assert!(!first.path.starts_with(r"\\?\"));
    }

    #[test]
    fn breadcrumbs_walk_from_the_root_down() {
        let trail = breadcrumbs(&normalize(&crate_root()).unwrap());

        assert!(trail.len() >= 2);
        assert_eq!(trail.last().unwrap().label, "rtsp-utils");
        // Each step must name a real, listable directory.
        for step in &trail {
            assert!(
                Path::new(&step.path).is_dir(),
                "{} should be a directory",
                step.path
            );
        }
        // …and they must nest, shortest first.
        for pair in trail.windows(2) {
            assert!(pair[1].path.starts_with(&pair[0].path));
        }
    }

    #[test]
    fn resolved_paths_are_readable_and_free_of_the_windows_prefix() {
        let resolved = open().resolve("Cargo.toml").unwrap();
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "the extended-length prefix would leak into the UI: {}",
            resolved.display()
        );
        assert!(resolved.is_file());
    }

    #[test]
    fn a_missing_path_is_reported_as_not_found() {
        assert!(open().resolve("no-such-file.mov").is_err());
    }
}
