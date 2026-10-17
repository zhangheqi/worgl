//! Reading and writing the small files beside each store: secrets, tokens
//! and settings.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Creates `path` holding `text` and a newline, readable by its owner only.
/// Refuses to replace an existing file.
pub fn write_secret(path: &Path, text: &str) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    writeln!(file, "{text}")?;
    file.sync_all()?;
    Ok(())
}

/// Reads a file written by [`write_secret`], without its trailing newline.
pub fn read_secret(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text.trim().to_owned())
}

/// Reads a 32-byte secret key written as hex.
pub fn read_key(path: &Path) -> Result<[u8; 32]> {
    let mut secret = [0u8; 32];
    hex::decode_to_slice(read_secret(path)?, &mut secret)
        .with_context(|| format!("{} must hold 64 hex digits", path.display()))?;
    Ok(secret)
}

/// 32 random bytes as hex, for a bearer token.
pub(crate) fn random_token() -> Result<String> {
    let mut token = [0u8; 32];
    getrandom::fill(&mut token).map_err(|e| anyhow::anyhow!("system RNG failed: {e}"))?;
    Ok(hex::encode(token))
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Replaces `dir/name` with `value` as pretty JSON, so that a crash leaves
/// either the old file or the new one.
pub(crate) fn write_json(dir: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)? + "\n";
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file = File::create(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir)
}

/// Flushes a directory's entries, so a file created or renamed in it stays.
#[cfg(unix)]
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}
