//! Where a purse keeps its notes between runs.

use std::fmt;
use std::fs::{self, File, OpenOptions};
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

    /// Locks the store against other processes and reads what is saved in
    /// it, for a store more than one process writes. A purse calls it
    /// before every change: it makes the change to what was read here,
    /// saves, and only then drops the lock, so it never saves over a change
    /// another process made since it last looked.
    ///
    /// The default returns `None`, for a store only one purse writes: the
    /// purse's own holder is then always current.
    fn lock(&self) -> io::Result<Option<Locked>> {
        Ok(None)
    }
}

/// A store locked against other processes until this is dropped, and what
/// was saved in it when the lock was taken.
pub struct Locked {
    saved: Option<String>,
    _lock: Box<dyn Send>,
}

impl Locked {
    /// What [`Store::lock`] returns: `saved` is the JSON saved now, or
    /// `None` if nothing has been saved yet, and `lock` keeps other
    /// processes out until it is dropped, such as a [`FileLock`].
    pub fn new(saved: Option<String>, lock: impl Send + 'static) -> Self {
        Self {
            saved,
            _lock: Box::new(lock),
        }
    }

    /// The JSON saved when the lock was taken, if any.
    pub fn saved(&self) -> Option<&str> {
        self.saved.as_deref()
    }
}

impl fmt::Debug for Locked {
    // The JSON holds secret keys.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Locked").finish_non_exhaustive()
    }
}

/// A store in one file, replaced whole on every save.
///
/// A save writes a temporary file beside it, flushes it to disk and renames
/// it over the old one, so a crash leaves either the old store or the new,
/// never a mix. On Unix the file is readable by its owner only.
///
/// Several processes can share it: [`lock`](Store::lock) takes a lock on a
/// second file beside it, `.<name>.lock`, which stays there.
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

    /// Waits until no other process holds the store's lock, then takes it,
    /// until the returned lock is dropped. [`lock`](Store::lock) does this
    /// and reads the file; a store that wraps a `FileStore`, such as one
    /// that encrypts it, can lock it this way and read it itself.
    pub fn lock_file(&self) -> io::Result<FileLock> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(self.beside("lock"))?;
        file.lock()?;
        Ok(FileLock(file))
    }

    fn temporary(&self) -> PathBuf {
        self.beside("new")
    }

    /// `.<name>.<suffix>` in the store's directory.
    fn beside(&self, suffix: &str) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map_or_else(|| "store".into(), |name| name.to_string_lossy());
        self.path.with_file_name(format!(".{name}.{suffix}"))
    }
}

/// A [`FileStore`]'s lock, held until it is dropped.
#[derive(Debug)]
pub struct FileLock(File);

impl Drop for FileLock {
    fn drop(&mut self) {
        // A child process started meanwhile holds a copy of every open file
        // until it execs, and a lock lasts until every copy of its file is
        // closed. Unlocking ends it for all copies at once.
        let _ = self.0.unlock();
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

    fn lock(&self) -> io::Result<Option<Locked>> {
        let lock = self.lock_file()?;
        let saved = match fs::read_to_string(&self.path) {
            Ok(json) => Some(json),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Some(Locked::new(saved, lock)))
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

    fn lock(&self) -> io::Result<Option<Locked>> {
        (**self).lock()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_file_store_is_locked_by_one_writer_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("purse.json"));
        let first = store.lock().unwrap().unwrap();
        assert_eq!(first.saved(), None);

        let (sender, receiver) = mpsc::channel();
        let other = store.clone();
        let waiter = thread::spawn(move || {
            let locked = other.lock().unwrap().unwrap();
            sender.send(locked.saved().map(str::to_owned)).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
        store.save("{}").unwrap();
        drop(first);
        let saved = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(saved.as_deref(), Some("{}"));
        waiter.join().unwrap();
    }
}
