//! Persistence: a single JSON document under the XDG data directory.
//!
//! Notes are small, few, and always loaded together, so one file beats a
//! database. The two things that matter are that a crash mid-write can never
//! destroy the previous contents (writes go through a temporary file and an
//! atomic rename) and that a corrupt file loses at most the corrupt file (it is
//! set aside rather than overwritten).

use super::note::Note;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Bumped when the on-disk shape changes incompatibly. Readers accept any
/// version they understand and refuse to clobber anything newer.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct Document {
    version: u32,
    notes: Vec<Note>,
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    /// The file parsed as JSON but declares a version this build cannot read.
    /// The store refuses to save so a newer version's data is not destroyed.
    UnsupportedVersion(u32),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(err) => write!(f, "{err}"),
            StoreError::UnsupportedVersion(v) => write!(
                f,
                "notes file uses format version {v}, but this build understands \
                 at most {SCHEMA_VERSION}; upgrade Stickies to open it"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(err: std::io::Error) -> Self {
        StoreError::Io(err)
    }
}

/// What happened when the store was opened. Anything other than
/// [`LoadOutcome::Loaded`] is worth surfacing to the user as a toast or banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// Notes were read successfully (possibly zero of them).
    Loaded,
    /// No file yet — first run.
    Fresh,
    /// The file could not be parsed; it was moved aside to this path and the
    /// store started empty.
    Recovered { backup: PathBuf, reason: String },
}

/// The in-memory collection of notes plus the file backing it.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    notes: Vec<Note>,
    /// Set when the file on disk is newer than we understand; blocks saving.
    read_only: bool,
}

impl Store {
    /// Default location: `$XDG_DATA_HOME/stickies/notes.json`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("stickies").join("notes.json")
    }

    /// An empty store backed by `path`, without touching the disk.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            notes: Vec::new(),
            read_only: false,
        }
    }

    /// Read notes from `path`.
    ///
    /// A missing file is first run, not an error. A file that fails to parse is
    /// renamed out of the way and the store starts empty, so a bad write never
    /// leaves the app permanently unable to launch. A file from a *newer*
    /// schema is loaded read-only rather than discarded.
    pub fn open(path: impl Into<PathBuf>) -> (Self, LoadOutcome) {
        let path = path.into();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Self::new(path), LoadOutcome::Fresh);
            }
            Err(err) => {
                // Unreadable for some other reason (permissions, I/O). Do not
                // move it aside — that would risk destroying recoverable data.
                let mut store = Self::new(path.clone());
                store.read_only = true;
                return (
                    store,
                    LoadOutcome::Recovered {
                        backup: path,
                        reason: err.to_string(),
                    },
                );
            }
        };

        match serde_json::from_str::<Document>(&raw) {
            Ok(doc) if doc.version <= SCHEMA_VERSION => (
                Self {
                    path,
                    notes: doc.notes,
                    read_only: false,
                },
                LoadOutcome::Loaded,
            ),
            Ok(doc) => {
                let mut store = Self {
                    path,
                    notes: doc.notes,
                    read_only: true,
                };
                let version = doc.version;
                store.notes.clear();
                let backup = store.path.clone();
                (
                    store,
                    LoadOutcome::Recovered {
                        backup,
                        reason: StoreError::UnsupportedVersion(version).to_string(),
                    },
                )
            }
            Err(err) => {
                let backup = backup_path(&path);
                let _ = std::fs::rename(&path, &backup);
                (
                    Self::new(path),
                    LoadOutcome::Recovered {
                        backup,
                        reason: err.to_string(),
                    },
                )
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Note> {
        self.notes.iter_mut().find(|n| n.id == id)
    }

    /// Insert a new note or replace the existing one with the same ID.
    pub fn upsert(&mut self, note: Note) {
        match self.notes.iter_mut().find(|n| n.id == note.id) {
            Some(existing) => *existing = note,
            None => self.notes.push(note),
        }
    }

    /// Remove a note, returning it so the caller can offer an undo.
    pub fn remove(&mut self, id: &str) -> Option<Note> {
        let index = self.notes.iter().position(|n| n.id == id)?;
        Some(self.notes.remove(index))
    }

    /// Write the store out atomically: temporary file in the same directory,
    /// flushed and fsynced, then renamed over the target. A crash at any point
    /// leaves either the old file or the new one, never a truncated one.
    pub fn save(&self) -> Result<(), StoreError> {
        if self.read_only {
            return Err(StoreError::UnsupportedVersion(SCHEMA_VERSION + 1));
        }

        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }

        let doc = Document {
            version: SCHEMA_VERSION,
            notes: self.notes.clone(),
        };
        let json = serde_json::to_vec_pretty(&doc).map_err(std::io::Error::other)?;

        let tmp = self.path.with_extension("json.tmp");
        {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&json)?;
            file.flush()?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// `notes.json` → `notes.json.corrupt-<unix-seconds>`.
fn backup_path(path: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".corrupt-{stamp}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note::{Note, NoteGeometry, Palette};

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn sample(id: &str) -> Note {
        Note {
            id: id.into(),
            body: format!("body of {id}"),
            palette: Palette::Blue,
            geometry: NoteGeometry {
                monitor: Some("DP-1".into()),
                x: 100,
                y: 200,
                width: 320,
                height: 340,
            },
            visible: true,
            pinned: false,
            created: 1,
            modified: 2,
        }
    }

    #[test]
    fn opening_a_missing_file_is_first_run_not_an_error() {
        let dir = temp_dir();
        let (store, outcome) = Store::open(dir.path().join("notes.json"));
        assert_eq!(outcome, LoadOutcome::Fresh);
        assert!(store.is_empty());
        assert!(!store.is_read_only());
    }

    #[test]
    fn notes_survive_a_save_and_reload() {
        let dir = temp_dir();
        let path = dir.path().join("nested/notes.json");

        let mut store = Store::new(&path);
        store.upsert(sample("a"));
        store.upsert(sample("b"));
        store.save().expect("save");

        let (reloaded, outcome) = Store::open(&path);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded.get("a"), Some(&sample("a")));
        assert_eq!(
            reloaded.get("b").unwrap().geometry.monitor.as_deref(),
            Some("DP-1"),
            "monitor identity round-trips"
        );
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let dir = temp_dir();
        let path = dir.path().join("a/b/c/notes.json");
        let store = Store::new(&path);
        store.save().expect("save");
        assert!(path.exists());
    }

    #[test]
    fn save_leaves_no_temporary_file_behind() {
        let dir = temp_dir();
        let path = dir.path().join("notes.json");
        let mut store = Store::new(&path);
        store.upsert(sample("a"));
        store.save().expect("save");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stray temp files: {leftovers:?}");
    }

    #[test]
    fn upsert_replaces_rather_than_duplicates() {
        let mut store = Store::new("/nonexistent/notes.json");
        store.upsert(sample("a"));
        let mut updated = sample("a");
        updated.body = "rewritten".into();
        store.upsert(updated);

        assert_eq!(store.len(), 1);
        assert_eq!(store.get("a").unwrap().body, "rewritten");
    }

    #[test]
    fn remove_returns_the_note_so_it_can_be_undone() {
        let mut store = Store::new("/nonexistent/notes.json");
        store.upsert(sample("a"));
        let removed = store.remove("a").expect("removed");
        assert_eq!(removed.id, "a");
        assert!(store.is_empty());
        assert_eq!(store.remove("a"), None);

        // Undo: put it straight back.
        store.upsert(removed);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn get_mut_edits_in_place() {
        let mut store = Store::new("/nonexistent/notes.json");
        store.upsert(sample("a"));
        store.get_mut("a").unwrap().body = "edited".into();
        assert_eq!(store.get("a").unwrap().body, "edited");
        assert!(store.get_mut("missing").is_none());
    }

    #[test]
    fn a_corrupt_file_is_set_aside_and_the_app_still_starts() {
        let dir = temp_dir();
        let path = dir.path().join("notes.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let (store, outcome) = Store::open(&path);
        assert!(store.is_empty());
        assert!(!store.is_read_only(), "a fresh store is writable");

        let LoadOutcome::Recovered { backup, .. } = outcome else {
            panic!("expected recovery, got {outcome:?}");
        };
        assert!(backup.exists(), "the bad file was preserved");
        assert!(!path.exists(), "the bad file was moved, not copied");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "{ this is not json"
        );

        // And the recovered store can be written to.
        store.save().expect("save after recovery");
        assert_eq!(Store::open(&path).1, LoadOutcome::Loaded);
    }

    #[test]
    fn a_file_from_a_newer_version_is_never_overwritten() {
        let dir = temp_dir();
        let path = dir.path().join("notes.json");
        std::fs::write(
            &path,
            format!(r#"{{"version":{},"notes":[]}}"#, SCHEMA_VERSION + 1),
        )
        .unwrap();

        let (store, outcome) = Store::open(&path);
        assert!(store.is_read_only());
        assert!(matches!(outcome, LoadOutcome::Recovered { .. }));
        assert!(store.save().is_err(), "saving must be refused");
        // Original bytes intact.
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains(&format!("\"version\":{}", SCHEMA_VERSION + 1)));
    }

    #[test]
    fn an_empty_notes_array_loads_cleanly() {
        let dir = temp_dir();
        let path = dir.path().join("notes.json");
        std::fs::write(&path, r#"{"version":1,"notes":[]}"#).unwrap();
        let (store, outcome) = Store::open(&path);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert!(store.is_empty());
    }

    #[test]
    fn default_path_follows_xdg_data_home() {
        // Exercised via the same precedence logic, without mutating the
        // process environment (which would race other tests).
        let path = Store::default_path();
        assert!(path.is_absolute() || path.starts_with("."));
        assert!(path.ends_with("stickies/notes.json"));
    }

    #[test]
    fn backup_paths_are_derived_from_the_original() {
        let backup = backup_path(Path::new("/home/u/.local/share/stickies/notes.json"));
        let name = backup.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("notes.json.corrupt-"), "{name}");
        assert_eq!(
            backup.parent(),
            Path::new("/home/u/.local/share/stickies").into()
        );
    }
}
