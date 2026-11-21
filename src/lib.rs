//! newvcs: an experiment in keeping Git's object ids without Git's storage.
//!
//! - `store`: content-addressed objects in an append-only segment, file
//!   versions as bounded delta chains.
//! - `manifest`: a directory tree as one small object per directory.
//! - `commit`: a commit as a list of path changes; its tree is derived.
//! - `git`: import from Git, and the Git ids every object maps back to.
//! - `gitcli`: the part of Git's command line that nv speaks.
//! - `index`: what a workspace knows of its files, changed by appending.
//! - `textdiff`: line differences, written as `git diff` writes them.
//! - `workspace`: checkouts of a set of path prefixes, shared copy-on-write.
//! - `remote`: pushing to and catching up with a server that owns the refs.

pub mod colocated;
pub mod commit;
pub mod eol;
pub mod git;
pub mod gitcli;
pub mod ignore;
pub mod index;
pub mod links;
pub mod manifest;
pub mod remote;
pub mod store;
pub mod textdiff;
pub mod watch;
pub mod workspace;

/// An object id: BLAKE3 over the object's kind and bytes.
pub type Id = [u8; 32];

pub fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
  if !text.len().is_multiple_of(2) {
    return None;
  }
  (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

pub type Result<T> = std::result::Result<T, String>;

/// Replace a file's contents so that whoever reads it, at any moment and
/// after a process is killed at any moment, finds the old contents or the
/// new, never a part of either. Every file that says where a workspace or
/// a ref stands is written this way.
pub fn write_whole(path: &std::path::Path, bytes: impl AsRef<[u8]>) -> Result<()> {
  let fail = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
  let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
  let temp = path.with_file_name(format!(".{name}.tmp{}", std::process::id()));
  std::fs::write(&temp, bytes).map_err(fail)?;
  std::fs::rename(&temp, path).map_err(|e| {
    let _ = std::fs::remove_file(&temp);
    fail(e)
  })
}
