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
