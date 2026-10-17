//! Where a purse keeps its notes between runs.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use worgl_sdk::Holder;

use crate::error::Error;

/// Where a [`Purse`](crate::Purse) saves its [`Holder`] after every change.
///
/// The JSON holds the secret key of every note, and notes are bearer:
/// whoever reads it can spend them. Keep it somewhere private, encrypted at
/// rest.
pub trait Store: Send + Sync {
    /// Replaces what was saved with `json`. Returns only once the data
    /// would survive a crash: the purse hands out keys and transfers only
    /// after this succeeds.
    fn save(&self, json: &str) -> io::Result<()>;
}

/// A store in one file, replaced whole on every save.
///
/// A save writes a temporary file beside it, flushes it to disk and renames
/// it over the old one, so a crash leaves either the old store or the new,
/// never a mix. On Unix the file is readable by its owner only.
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// A store at `path`. Nothing is read or written yet.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the saved holder, or a new empty one if the file does not
    /// exist yet.
    pub fn load(&self) -> Result<Holder, Error> {
        match fs::read_to_string(&self.path) {
            Ok(json) => Ok(Holder::from_json(&json)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Holder::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn temporary(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map_or_else(|| "store".into(), |name| name.to_string_lossy());
        self.path.with_file_name(format!(".{name}.new"))
    }
}

impl Store for FileStore {
    fn save(&self, json: &str) -> io::Result<()> {
        let temporary = self.temporary();
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &self.path)?;
        // The rename is durable only once the directory is flushed too.
        #[cfg(unix)]
        {
            let dir = match self.path.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => dir,
                _ => Path::new("."),
            };
            fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }
}

/// A store in memory, lost when the process ends. For tests, and for
/// applications that persist the JSON themselves.
#[derive(Debug, Default)]
pub struct MemoryStore {
    saved: Mutex<Option<String>>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The JSON saved last, if any.
    pub fn saved(&self) -> Option<String> {
        self.saved.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Store for MemoryStore {
    fn save(&self, json: &str) -> io::Result<()> {
        *self.saved.lock().unwrap_or_else(|e| e.into_inner()) = Some(json.to_owned());
        Ok(())
    }
}

impl<S: Store + ?Sized> Store for std::sync::Arc<S> {
    fn save(&self, json: &str) -> io::Result<()> {
        (**self).save(json)
    }
}
