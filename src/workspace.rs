//! Working copies: a view of one commit on disk.
//!
//! A workspace holds the files under a set of path prefixes and nothing
//! else. Because a directory's id says everything about what is below it,
//! checking out `packages/core` reads the directories on that path and the
//! files under it, and never looks at the rest of the tree. Because a commit
//! is a list of changes, a workspace can commit without the rest either.
//!
//! Files are not written per workspace. Each blob is materialised once, in
//! the store's `files/` directory, and a workspace gets a copy-on-write
//! clone of it (APFS `clonefile`; a plain copy where that is not available).
//! Ten workspaces of one commit cost the disk of one. `fork` clones a whole
//! workspace the same way, untracked build output included, so a new
//! workspace starts with the caches of the one it came from.
//!
//! Recording and publishing are two things. A `snapshot` records what is on
//! disk as a commit on top of the last one, and is meant to be taken after
//! every step of work; nobody else sees it. A `commit` folds the snapshots
//! since the last published commit into one commit with one message. The
//! snapshots stay in the store, listed in the workspace's operation log.
//!
//! Finding what changed does not read files. The workspace keeps an index of
//! each file's size, modification time and id (`index.rs`); a file whose
//! size and time are as recorded is taken as unchanged. A writer that goes
//! through `record` (an agent's file tool, say) names what it wrote, and a
//! snapshot can then look at those paths alone.

use crate::commit::{Commit, root_of};
pub use crate::index::{Index, Indexed};
use crate::manifest::{Change, Entry, MODE_GITLINK, MODE_LINK, in_view, walk};
use crate::store::{Kind, Store, object_id};
use crate::{Id, Result, hex, unhex};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const META: &str = ".nv";
/// Never tracked: this tool's and Git's own directories, and the usual build output.
const SKIPPED: &[&str] = &[META, ".git", "target", "node_modules"];

pub struct Workspace {
  pub root: PathBuf,
  pub store: PathBuf,
  /// The last published commit.
  pub base: Option<Id>,
  /// The commit the disk was last recorded as: `base`, or a snapshot on top of it.
  pub head: Option<Id>,
  pub prefixes: Vec<Vec<u8>>,
  /// The server and repository this workspace pushes to, as a URL, and the ref there.
  pub remote: Option<String>,
  pub remote_ref: Option<String>,
  /// The ref's tip when this workspace last looked, and its position in the ref's history.
  pub pushed: Option<Id>,
  pub seq: u64,
}

/// A modification time this close to now may be followed by another write
/// in the same tick, which would leave size and time looking unchanged.
const RACY_NANOS: u128 = 20_000_000;

fn mtime_of(meta: &std::fs::Metadata) -> u128 {
  meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
}

fn now_nanos() -> u128 {
  std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// The index entry for a file as it is on disk now, known to have this entry.
fn indexed(path: &Path, entry: Entry) -> Indexed {
  match std::fs::symlink_metadata(path) {
    Ok(meta) => {
      let mtime = mtime_of(&meta);
      Indexed { entry, size: meta.len(), mtime: if now_nanos().saturating_sub(mtime) < RACY_NANOS { 0 } else { mtime } }
    }
    Err(_) => Indexed { entry, size: 0, mtime: 0 },
  }
}

fn to_path(bytes: &[u8]) -> PathBuf {
  PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
}

/// A copy of `from` at `to` that shares its blocks until one of them is written.
fn clone_file(from: &Path, to: &Path) -> std::io::Result<()> {
  #[cfg(target_os = "macos")]
  {
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::Error::other("a path with a NUL byte"));
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    if unsafe { libc::clonefile(c(from)?.as_ptr(), c(to)?.as_ptr(), 0) } == 0 {
      return Ok(());
    }
  }
  std::fs::copy(from, to).map(|_| ())
}

impl Workspace {
  pub fn open(root: &Path) -> Result<Workspace> {
    let text = std::fs::read_to_string(root.join(META).join("workspace")).map_err(|_| format!("{} is not a workspace", root.display()))?;
    let mut ws = Workspace {
      root: root.to_path_buf(),
      store: PathBuf::new(),
      base: None,
      head: None,
      prefixes: Vec::new(),
      remote: None,
      remote_ref: None,
      pushed: None,
      seq: 0,
    };
    for line in text.lines() {
      match line.split_once(' ') {
        Some(("store", v)) => ws.store = PathBuf::from(v),
        Some(("base", v)) => ws.base = unhex(v).and_then(|b| b.try_into().ok()),
        Some(("head", v)) => ws.head = unhex(v).and_then(|b| b.try_into().ok()),
        Some(("remote", v)) => ws.remote = Some(v.to_string()),
        Some(("ref", v)) => ws.remote_ref = Some(v.to_string()),
        Some(("pushed", v)) => ws.pushed = unhex(v).and_then(|b| b.try_into().ok()),
        Some(("seq", v)) => ws.seq = v.parse().unwrap_or(0),
        Some(("prefix", v)) => ws.prefixes.push(unhex(v).ok_or("a damaged workspace file")?),
        _ => {}
      }
    }
    Ok(ws)
  }

  /// Point the workspace at a server's repository and ref.
  pub fn set_remote(&mut self, url: &str, name: &str) -> Result<()> {
    (self.remote, self.remote_ref, self.pushed, self.seq) = (Some(url.to_string()), Some(name.to_string()), None, 0);
    self.save()
  }

  /// Take a new state arrived at by `remote::sync`.
  pub(crate) fn adopt(&self, index: &mut Index) -> Result<()> {
    index.save()?;
    self.save()?;
    if let Some(head) = &self.head {
      self.log("sync", head)?;
    }
    Ok(())
  }

  fn save(&self) -> Result<()> {
    let mut text = format!("store {}\n", self.store.display());
    if let Some(base) = &self.base {
      text.push_str(&format!("base {}\n", hex(base)));
    }
    if let Some(head) = &self.head {
      text.push_str(&format!("head {}\n", hex(head)));
    }
    if let (Some(remote), Some(name)) = (&self.remote, &self.remote_ref) {
      text.push_str(&format!("remote {remote}\nref {name}\nseq {}\n", self.seq));
    }
    if let Some(pushed) = &self.pushed {
      text.push_str(&format!("pushed {}\n", hex(pushed)));
    }
    for p in &self.prefixes {
      text.push_str(&format!("prefix {}\n", hex(p)));
    }
    std::fs::create_dir_all(self.root.join(META)).map_err(|e| e.to_string())?;
    std::fs::write(self.root.join(META).join("workspace"), text).map_err(|e| e.to_string())
  }

  pub fn load_index(&self) -> Index {
    Index::open(&self.root.join(META))
  }

  fn log(&self, what: &str, id: &Id) -> Result<()> {
    use std::io::Write;
    let mut file =
      std::fs::File::options().append(true).create(true).open(self.root.join(META).join("oplog")).map_err(|e| e.to_string())?;
    writeln!(file, "{} {what} {}", now_nanos() / 1_000_000, hex(id)).map_err(|e| e.to_string())
  }

  /// The operations of this workspace, oldest first: `(milliseconds, what, commit)`.
  pub fn operations(&self) -> Vec<(u128, String, String)> {
    let text = std::fs::read_to_string(self.root.join(META).join("oplog")).unwrap_or_default();
    let parse = |l: &str| {
      let mut f = l.split(' ');
      Some((f.next()?.parse().ok()?, f.next()?.to_string(), f.next()?.to_string()))
    };
    text.lines().filter_map(parse).collect()
  }

  /// Say that `path` (relative to the workspace) was written or removed. A
  /// snapshot taken with `Look::Recorded` looks at recorded paths and nothing else.
  pub fn record(&self, path: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file =
      std::fs::File::options().append(true).create(true).open(self.root.join(META).join("journal")).map_err(|e| e.to_string())?;
    file.write_all(&[path, b"\0"].concat()).map_err(|e| e.to_string())
  }

  fn recorded(&self) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(self.root.join(META).join("journal")).unwrap_or_default();
    let mut paths: Vec<Vec<u8>> = bytes.split(|&b| b == 0).filter(|p| !p.is_empty()).map(<[u8]>::to_vec).collect();
    paths.sort();
    paths.dedup();
    paths
  }
}

/// How a snapshot finds what changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
  /// Walk the view and compare each file's size and time with the index.
  Everywhere,
  /// Only at the paths `Workspace::record` was told of: for a writer that reports every write.
  Recorded,
}

fn blob(store: &mut Store, id: &Id) -> Result<std::sync::Arc<Vec<u8>>> {
  store.get(id)?.ok_or_else(|| format!("blob {} is not in the store", hex(id)))
}

/// Write one file of a commit into a workspace, from the store's shared
/// copy, and say what the index should hold for it.
pub(crate) fn place(store: &mut Store, root: &Path, path: &[u8], entry: Entry) -> Result<Indexed> {
  let dest = root.join(to_path(path));
  let fail = |what: &str, at: &Path, e: std::io::Error| format!("cannot {what} {}: {e}", at.display());
  std::fs::create_dir_all(dest.parent().expect("under the workspace")).map_err(|e| fail("create the directory of", &dest, e))?;
  if entry.mode == MODE_GITLINK {
    std::fs::create_dir_all(&dest).map_err(|e| fail("create", &dest, e))?;
    return Ok(Indexed { entry, size: 0, mtime: 0 });
  }
  // A directory in the way of a file is what the other changes of the same commit emptied.
  if std::fs::symlink_metadata(&dest).is_ok_and(|m| m.is_dir()) {
    let _ = std::fs::remove_dir_all(&dest);
  }
  let _ = std::fs::remove_file(&dest);
  if entry.mode == MODE_LINK {
    std::os::unix::fs::symlink(to_path(&blob(store, &entry.id)?), &dest).map_err(|e| fail("link", &dest, e))?;
    return Ok(indexed(&dest, entry));
  }
  let files_dir = store.dir.join("files");
  std::fs::create_dir_all(&files_dir).map_err(|e| fail("create", &files_dir, e))?;
  let cached = files_dir.join(hex(&entry.id));
  if !cached.exists() {
    // Written under another name first: a reader never sees half a file.
    let temp = files_dir.join(format!("{}.tmp{}", hex(&entry.id), std::process::id()));
    std::fs::write(&temp, blob(store, &entry.id)?.as_slice()).map_err(|e| fail("write", &temp, e))?;
    std::fs::rename(&temp, &cached).map_err(|e| fail("write", &cached, e))?;
  }
  clone_file(&cached, &dest).map_err(|e| fail("write", &dest, e))?;
  if entry.mode & 0o111 != 0 {
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| fail("mark executable", &dest, e))?;
  }
  Ok(indexed(&dest, entry))
}

/// Remove a file from a workspace, and the directories that leaves empty.
pub(crate) fn unplace(root: &Path, path: &[u8]) {
  let dest = root.join(to_path(path));
  let _ = std::fs::remove_file(&dest);
  let mut dir = dest.parent();
  while let Some(d) = dir.filter(|d| *d != root) {
    if std::fs::remove_dir(d).is_err() {
      break;
    }
    dir = d.parent();
  }
}

/// A workspace's own unpublished changes as one commit on `onto`.
pub(crate) fn carry(store: &mut Store, onto: Id, changes: Vec<Change>, author: &str) -> Result<Id> {
  let id = Commit { parents: vec![onto], rest: rest_of(author, "snapshot"), changes }.store(store)?;
  root_of(store, &id)?;
  Ok(id)
}

#[derive(Default, Debug)]
pub struct CheckedOut {
  pub files: usize,
  pub bytes: u64,
  /// Blobs that had to be read from the store; the rest were already materialised.
  pub materialised: usize,
}

/// Write the files of `commit` under `prefixes` (all of it when empty) to `target`.
pub fn checkout(store: &mut Store, commit: Id, target: &Path, prefixes: Vec<Vec<u8>>) -> Result<CheckedOut> {
  let fail = |what: &str, path: &Path, e: std::io::Error| format!("cannot {what} {}: {e}", path.display());
  let files_dir = store.dir.join("files");
  std::fs::create_dir_all(&files_dir).map_err(|e| fail("create", &files_dir, e))?;
  std::fs::create_dir_all(target).map_err(|e| fail("create", target, e))?;
  let mut found: Vec<(Vec<u8>, Entry)> = Vec::new();
  if let Some(root) = root_of(store, &commit)? {
    walk(store, root, &prefixes, &mut |path, entry| {
      found.push((path.to_vec(), entry));
      Ok(())
    })?;
  }
  let mut done = CheckedOut::default();
  let mut index: Vec<(Vec<u8>, Indexed)> = Vec::new();
  for (path, entry) in found {
    let dest = target.join(to_path(&path));
    std::fs::create_dir_all(dest.parent().expect("under the target")).map_err(|e| fail("create the directory of", &dest, e))?;
    if entry.mode == MODE_GITLINK {
      std::fs::create_dir_all(&dest).map_err(|e| fail("create", &dest, e))?;
      index.push((path, Indexed { entry, size: 0, mtime: 0 }));
      continue;
    }
    let _ = std::fs::remove_file(&dest);
    if entry.mode == MODE_LINK {
      let bytes = store.get(&entry.id)?.ok_or_else(|| format!("blob {} is not in the store", hex(&entry.id)))?;
      std::os::unix::fs::symlink(to_path(&bytes), &dest).map_err(|e| fail("link", &dest, e))?;
      index.push((path, indexed(&dest, entry)));
      done.files += 1;
      continue;
    }
    let cached = files_dir.join(hex(&entry.id));
    let size = match std::fs::metadata(&cached) {
      Ok(meta) => meta.len(),
      Err(_) => {
        let bytes = store.get(&entry.id)?.ok_or_else(|| format!("blob {} is not in the store", hex(&entry.id)))?;
        // Written under another name first: a reader never sees half a file.
        let temp = files_dir.join(format!("{}.tmp{}", hex(&entry.id), std::process::id()));
        std::fs::write(&temp, bytes.as_slice()).map_err(|e| fail("write", &temp, e))?;
        std::fs::rename(&temp, &cached).map_err(|e| fail("write", &cached, e))?;
        done.materialised += 1;
        bytes.len() as u64
      }
    };
    clone_file(&cached, &dest).map_err(|e| fail("write", &dest, e))?;
    if entry.mode & 0o111 != 0 {
      std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| fail("mark executable", &dest, e))?;
    }
    index.push((path, indexed(&dest, entry)));
    done.files += 1;
    done.bytes += size;
  }
  let store_dir = std::fs::canonicalize(&store.dir).map_err(|e| e.to_string())?;
  let ws = Workspace {
    root: target.to_path_buf(),
    store: store_dir,
    base: Some(commit),
    head: Some(commit),
    prefixes,
    remote: None,
    remote_ref: None,
    pushed: None,
    seq: 0,
  };
  ws.save()?;
  Index::create(&target.join(META), index)?;
  let _ = std::fs::remove_file(target.join(META).join("journal"));
  Ok(done)
}

/// A file on disk: its mode and bytes.
pub type Content = (u32, Vec<u8>);
/// `(path, what is on disk now, the entry last recorded)`.
pub type Difference = (Vec<u8>, Option<Content>, Option<Entry>);

#[derive(Default, Debug, Clone, Copy)]
pub struct Looked {
  /// Files whose size and time were looked at, and files that had to be read.
  pub stats: usize,
  pub reads: usize,
}

fn read_content(path: &Path, meta: &std::fs::Metadata) -> Result<Content> {
  if meta.file_type().is_symlink() {
    let to = std::fs::read_link(path).map_err(|e| e.to_string())?;
    return Ok((MODE_LINK, to.into_os_string().into_vec()));
  }
  let mode = if meta.permissions().mode() & 0o111 != 0 { 0o100755 } else { 0o100644 };
  Ok((mode, std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?))
}

/// Does the file's size, time and kind still match what the index recorded?
fn unchanged(known: &Indexed, meta: &std::fs::Metadata) -> bool {
  let link = known.entry.mode == MODE_LINK;
  known.mtime != 0
    && known.size == meta.len()
    && known.mtime == mtime_of(meta)
    && link == meta.file_type().is_symlink()
    && (link || (known.entry.mode & 0o111 != 0) == (meta.permissions().mode() & 0o111 != 0))
}

/// Files that were read and found to be what the index says, with the size
/// and time they have now: the index can vouch for them from here on.
type Refreshed = Vec<(Vec<u8>, Indexed)>;

/// Compare one path on disk with what the index has for it.
fn compare(
  ws: &Workspace,
  path: &[u8],
  known: Option<&Indexed>,
  looked: &mut Looked,
  out: &mut Vec<Difference>,
  refreshed: &mut Refreshed,
) -> Result<()> {
  if known.is_some_and(|k| k.entry.mode == MODE_GITLINK) {
    // A submodule's directory is not ours to track.
    return Ok(());
  }
  let full = ws.root.join(to_path(path));
  looked.stats += 1;
  let meta = match std::fs::symlink_metadata(&full) {
    Ok(meta) if !meta.is_dir() => meta,
    _ => {
      if let Some(k) = known {
        out.push((path.to_vec(), None, Some(k.entry)));
      }
      return Ok(());
    }
  };
  if known.is_some_and(|k| unchanged(k, &meta)) {
    return Ok(());
  }
  looked.reads += 1;
  let (mode, bytes) = read_content(&full, &meta)?;
  if let Some(k) = known.filter(|k| k.entry.mode == mode && k.entry.id == object_id(Kind::Blob, &bytes)) {
    let now = indexed(&full, k.entry);
    if now.mtime != 0 {
      refreshed.push((path.to_vec(), now));
    }
    return Ok(());
  }
  out.push((path.to_vec(), Some((mode, bytes)), known.map(|k| k.entry)));
  Ok(())
}

struct Walk<'a> {
  ws: &'a Workspace,
  index: &'a Index,
  seen: crate::index::Seen,
  looked: Looked,
  out: Vec<Difference>,
  refreshed: Refreshed,
}

impl Walk<'_> {
  fn dir(&mut self, dir: &Path, rel: &mut Vec<u8>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
      let name = entry.file_name();
      if SKIPPED.iter().any(|s| name == *s) {
        continue;
      }
      let len = rel.len();
      if len > 0 {
        rel.push(b'/');
      }
      rel.extend_from_slice(name.as_bytes());
      let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
      if in_view(&self.ws.prefixes, rel, is_dir) {
        if !is_dir {
          let known = self.index.get_noting(&mut self.seen, rel);
          compare(self.ws, rel, known.as_ref(), &mut self.looked, &mut self.out, &mut self.refreshed)?;
        } else if self.index.get(rel).is_none_or(|k| k.entry.mode != MODE_GITLINK) {
          self.dir(&entry.path(), rel)?;
        }
      }
      rel.truncate(len);
    }
    Ok(())
  }
}

/// How the workspace differs from the commit it was last recorded as.
///
/// A file whose time the index could not trust is read once; if it is what
/// the index says, the index is brought up to date so that it is not read
/// again. That is the one thing this writes.
pub fn status(ws: &Workspace, look: Look) -> Result<(Vec<Difference>, Looked)> {
  let mut index = ws.load_index();
  let (out, looked, refreshed) = if look == Look::Recorded {
    let (mut looked, mut out, mut refreshed) = (Looked::default(), Vec::new(), Refreshed::new());
    for path in ws.recorded() {
      if in_view(&ws.prefixes, &path, false) {
        compare(ws, &path, index.get(&path).as_ref(), &mut looked, &mut out, &mut refreshed)?;
      }
    }
    (out, looked, refreshed)
  } else {
    let mut walk = Walk { ws, index: &index, seen: index.seen(), looked: Looked::default(), out: Vec::new(), refreshed: Vec::new() };
    walk.dir(&ws.root, &mut Vec::new())?;
    let Walk { seen, looked, mut out, refreshed, .. } = walk;
    // What the index has and the walk did not come across is gone from the disk.
    for (path, known) in index.unseen(&seen) {
      if known.entry.mode != MODE_GITLINK {
        out.push((path, None, Some(known.entry)));
      }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    (out, looked, refreshed)
  };
  if !refreshed.is_empty() {
    for (path, now) in refreshed {
      index.insert(path, now);
    }
    index.save()?;
  }
  Ok((out, looked))
}

fn rest_of(author: &str, message: &str) -> Vec<u8> {
  let now = now_nanos() / 1_000_000_000;
  format!("author {author} {now} +0000\ncommitter {author} {now} +0000\n\n{message}\n").into_bytes()
}

/// Record what is on disk as a commit on top of the last recorded one.
/// Nothing but the changed files is read, and nothing of the tree outside
/// the changed paths. `None` when nothing changed.
pub fn snapshot(store: &mut Store, ws: &mut Workspace, author: &str, look: Look) -> Result<Option<Id>> {
  let (found, _) = status(ws, look)?;
  let _ = std::fs::remove_file(ws.root.join(META).join("journal"));
  if found.is_empty() {
    return Ok(None);
  }
  let mut index = ws.load_index();
  let mut changes: Vec<Change> = Vec::with_capacity(found.len());
  for (path, new, old) in found {
    let entry = match new {
      Some((mode, bytes)) => Some(Entry { mode, id: store.put(Kind::Blob, &bytes, old.map(|o| o.id))? }),
      None => None,
    };
    match entry {
      Some(e) => index.insert(path.clone(), indexed(&ws.root.join(to_path(&path)), e)),
      None => index.remove(&path),
    }
    changes.push((path, entry));
  }
  let id = Commit { parents: ws.head.into_iter().collect(), rest: rest_of(author, "snapshot"), changes }.store(store)?;
  store.flush()?;
  ws.head = Some(id);
  index.save()?;
  ws.save()?;
  ws.log("snapshot", &id)?;
  Ok(Some(id))
}

/// Publish: one commit on the last published one, holding everything the
/// snapshots since then changed. Takes a snapshot first if the disk has
/// moved on. `None` when there is nothing to publish.
pub fn commit(store: &mut Store, ws: &mut Workspace, author: &str, message: &str) -> Result<Option<Id>> {
  snapshot(store, ws, author, Look::Everywhere)?;
  if ws.head == ws.base {
    return Ok(None);
  }
  let from = match ws.base {
    Some(base) => root_of(store, &base)?,
    None => None,
  };
  let to = match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  };
  let changes = crate::manifest::diff(store, from, to)?;
  if changes.is_empty() {
    // The snapshots undid each other.
    ws.head = ws.base;
    ws.save()?;
    return Ok(None);
  }
  let id = Commit { parents: ws.base.into_iter().collect(), rest: rest_of(author, message), changes }.store(store)?;
  root_of(store, &id)?;
  store.flush()?;
  (ws.base, ws.head) = (Some(id), Some(id));
  ws.save()?;
  ws.log("commit", &id)?;
  Ok(Some(id))
}

/// A second workspace with everything the first has on disk, tracked or not,
/// sharing its blocks until either side writes.
pub fn fork(from: &Path, to: &Path) -> Result<()> {
  Workspace::open(from)?;
  if to.exists() {
    return Err(format!("{} already exists", to.display()));
  }
  #[cfg(target_os = "macos")]
  {
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| "a path with a NUL byte".to_string());
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    // On APFS this clones a directory and all below it in one call.
    if unsafe { libc::clonefile(c(from)?.as_ptr(), c(to)?.as_ptr(), 0) } == 0 {
      return Ok(());
    }
  }
  let status = std::process::Command::new("cp").arg("-R").arg(from).arg(to).status().map_err(|e| e.to_string())?;
  if status.success() { Ok(()) } else { Err(format!("cannot copy {} to {}", from.display(), to.display())) }
}
