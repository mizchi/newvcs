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
use crate::eol::Eols;
pub use crate::index::{Index, Indexed};
use crate::manifest::{Change, Entry, MODE_GITLINK, MODE_LINK, in_view, walk};
use crate::store::{Kind, Store, object_id};
use crate::{Id, Result, hex, unhex};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub(crate) const META: &str = ".nv";
/// Never tracked: this tool's and Git's own directories, and the usual build output.
/// Never part of a workspace, whatever the ignore rules say.
const SKIPPED: &[&str] = &[META, ".git"];

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
  /// The commit the workspace was made at: what it has in common with every
  /// other workspace made from the same history. A ref it starts on a
  /// server holds the commits made since.
  pub start: Option<Id>,
}

/// A modification time this close to now may be followed by another write
/// in the same tick, which would leave size and time looking unchanged.
const RACY_NANOS: u128 = 20_000_000;

/// When a file's bytes or attributes last changed, as the file system keeps it and nobody can set it.
fn ctime_of(meta: &std::fs::Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  (meta.ctime().max(0) as u64).saturating_mul(1_000_000_000).saturating_add(meta.ctime_nsec().max(0) as u64)
}

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
      Indexed {
        entry,
        size: meta.len(),
        mtime: if now_nanos().saturating_sub(mtime) < RACY_NANOS { 0 } else { mtime },
        ctime: ctime_of(&meta),
      }
    }
    Err(_) => Indexed { entry, size: 0, mtime: 0, ctime: 0 },
  }
}

/// A name with its characters composed (NFC): how Git spells a name on a
/// file system that hands names back decomposed.
pub(crate) fn composed(name: &str) -> String {
  use unicode_normalization::UnicodeNormalization;
  name.nfc().collect()
}

pub(crate) fn to_path(bytes: &[u8]) -> PathBuf {
  PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
}

/// Remove a directory that holds nothing but directories that hold nothing.
fn remove_empty(dir: &Path) -> std::io::Result<()> {
  for entry in std::fs::read_dir(dir)? {
    let entry = entry?;
    if !entry.file_type()?.is_dir() {
      return Err(std::io::Error::other(format!("{} is in the way and is not nv's to remove", entry.path().display())));
    }
    remove_empty(&entry.path())?;
  }
  std::fs::remove_dir(dir)
}

/// What stands, in a directory where a file is about to be written, that is not among the files
/// about to be removed: files nv does not track, which no recorded state could give back.
fn in_the_way(root: &Path, changes: &[(Vec<u8>, Option<Entry>)]) -> Vec<String> {
  let leaving: std::collections::HashSet<&[u8]> =
    changes.iter().filter(|(_, entry)| entry.is_none()).map(|(path, _)| path.as_slice()).collect();
  let mut found = Vec::new();
  for (path, _) in changes.iter().filter(|(_, entry)| entry.is_some()) {
    let mut todo = vec![root.join(to_path(path))];
    while let Some(dir) = todo.pop() {
      if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        continue;
      }
      let Ok(listing) = std::fs::read_dir(&dir) else {
        // What cannot be listed cannot be shown to hold nothing.
        found.push(dir.strip_prefix(root).unwrap_or(&dir).display().to_string());
        continue;
      };
      for entry in listing.flatten() {
        let full = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
          todo.push(full);
        } else if let Ok(relative) = full.strip_prefix(root)
          && !leaving.contains(relative.as_os_str().as_bytes())
        {
          found.push(relative.display().to_string());
        }
      }
    }
  }
  found.sort();
  found
}

/// Whether the file system `dir` is on takes names that differ in case for one name. Asked of
/// the file system itself, with a file made and looked for under another case.
pub fn folds_names(dir: &Path) -> bool {
  let probe = dir.join(format!(".nv-case-probe-{}", std::process::id()));
  if std::fs::write(&probe, b"").is_err() {
    return cfg!(target_os = "macos");
  }
  let other = dir.join(format!(".NV-CASE-PROBE-{}", std::process::id()));
  let folds = std::fs::symlink_metadata(&other).is_ok();
  let _ = std::fs::remove_file(&probe);
  folds
}

/// A copy of `from` at `to` that shares its blocks until one of them is
/// written. Whatever is at `to` is replaced.
fn clone_file(from: &Path, to: &Path) -> std::io::Result<()> {
  #[cfg(target_os = "macos")]
  {
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::Error::other("a path with a NUL byte"));
    let (from_c, to_c) = (c(from)?, c(to)?);
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    let clone = || unsafe { libc::clonefile(from_c.as_ptr(), to_c.as_ptr(), 0) } == 0;
    if clone() {
      return Ok(());
    }
    // In a fresh workspace nothing is in the way, so nothing is removed first:
    // only a clone that found something there pays for removing it.
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
      if std::fs::symlink_metadata(to).is_ok_and(|m| m.is_dir()) {
        // A directory with something in it is somebody's files, and not this function's to remove.
        remove_empty(to)?;
      } else {
        std::fs::remove_file(to)?;
      }
      if clone() {
        return Ok(());
      }
    }
  }
  std::fs::copy(from, to).map(|_| ())
}

/// A copy of a directory and all below it, sharing blocks until either side
/// writes. On APFS this is one call whatever the size of the tree, and far
/// cheaper per file than cloning files one at a time: those calls take
/// turns inside the kernel, about 75 microseconds each.
fn clone_tree(from: &Path, to: &Path) -> std::io::Result<()> {
  #[cfg(target_os = "macos")]
  {
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::Error::other("a path with a NUL byte"));
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    if unsafe { libc::clonefile(c(from)?.as_ptr(), c(to)?.as_ptr(), 0) } == 0 {
      return Ok(());
    }
  }
  // Elsewhere a copy, keeping the times the files were written: the index that comes with the
  // tree knows each file by them, and a copy that changed them would have every file read again.
  // Sharing blocks where the file system can (Btrfs, XFS), as APFS does.
  let mut cp = std::process::Command::new("cp");
  if cfg!(target_os = "linux") {
    cp.args(["-R", "--preserve=mode,timestamps", "--reflink=auto"]);
  } else {
    cp.arg("-Rp");
  }
  let status = cp.arg(from).arg(to).status()?;
  if status.success() { Ok(()) } else { Err(std::io::Error::other("cp failed")) }
}

/// The top of the workspace a directory is in: itself, as it was given, if
/// it is one; else the nearest directory above that is.
pub fn top_of(dir: &Path) -> Option<PathBuf> {
  if dir.join(META).join("workspace").is_file() {
    return Some(dir.to_path_buf());
  }
  let whole = std::fs::canonicalize(dir).ok()?;
  whole.ancestors().skip(1).find(|above| above.join(META).join("workspace").is_file()).map(Path::to_path_buf)
}

impl Workspace {
  /// The workspace `dir` is the top of, or is somewhere inside.
  pub fn open(dir: &Path) -> Result<Workspace> {
    let Some(found) = top_of(dir) else { return Err(format!("{} is not a workspace, nor inside one", dir.display())) };
    let root = found.as_path();
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
      start: None,
      seq: 0,
    };
    for line in text.lines() {
      match line.split_once(' ') {
        // A store inside the workspace is named from the workspace, so that the directory can be moved.
        Some(("store", v)) if Path::new(v).is_relative() => ws.store = std::fs::canonicalize(root).map_err(|e| e.to_string())?.join(v),
        Some(("store", v)) => ws.store = PathBuf::from(v),
        Some(("base", v)) => ws.base = unhex(v).and_then(|b| b.try_into().ok()),
        Some(("head", v)) => ws.head = unhex(v).and_then(|b| b.try_into().ok()),
        Some(("remote", v)) => ws.remote = Some(v.to_string()),
        Some(("ref", v)) => ws.remote_ref = Some(v.to_string()),
        Some(("pushed", v)) => ws.pushed = unhex(v).and_then(|b| b.try_into().ok()),
        Some(("seq", v)) => ws.seq = v.parse().unwrap_or(0),
        Some(("start", v)) => ws.start = unhex(v).and_then(|b| b.try_into().ok()),
        Some(("prefix", v)) => ws.prefixes.push(unhex(v).ok_or("a damaged workspace file")?),
        _ => {}
      }
    }
    if ws.store.as_os_str().is_empty() {
      return Err(format!("{} is damaged: it does not say where the store is", root.join(META).join("workspace").display()));
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
    if let Some(head) = &self.head {
      self.log("sync", head)?;
    }
    self.save()?;
    index.save()
  }

  /// Write where the workspace stands. The file is replaced whole, so a
  /// process killed at any moment leaves the old state or the new.
  ///
  /// A command also saves the index, and can be killed between the two. The
  /// order is chosen so that the index is never ahead of the head: it may
  /// fail to know that a file is what the head says, which costs a read and
  /// at worst records a change twice, but it never says a file matches when
  /// the head has something else. Where the files move with the head (a
  /// snapshot, a restore, a sync) the head is saved first. Where they do not
  /// (`move_head`) the index is.
  pub(crate) fn save(&self) -> Result<()> {
    let inside = std::fs::canonicalize(&self.root).ok().and_then(|root| self.store.strip_prefix(root).ok().map(Path::to_path_buf));
    let mut text = format!("store {}\n", inside.as_deref().unwrap_or(&self.store).display());
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
    if let Some(start) = &self.start {
      text.push_str(&format!("start {}\n", hex(start)));
    }
    for p in &self.prefixes {
      text.push_str(&format!("prefix {}\n", hex(p)));
    }
    std::fs::create_dir_all(self.root.join(META)).map_err(|e| e.to_string())?;
    crate::write_whole(&self.root.join(META).join("workspace"), text)
  }

  pub fn load_index(&self) -> Index {
    Index::open(&self.root.join(META))
  }

  /// Note an operation. A state is logged before anything names it (before the head moves to
  /// it), so that a kill in between leaves a state that is somewhere to go back to, never one
  /// nothing names. A record is one write; a kill can still tear the last one, and the next
  /// record then starts on a line of its own, leaving the torn one to name nothing.
  pub(crate) fn log(&self, what: &str, id: &Id) -> Result<()> {
    use std::io::{Read, Seek, Write};
    let path = self.root.join(META).join("oplog");
    let mut file = std::fs::File::options().read(true).append(true).create(true).open(path).map_err(|e| e.to_string())?;
    let mut record = format!("{} {what} {}\n", now_nanos() / 1_000_000, hex(id));
    let mut last = [0u8; 1];
    if file.seek(std::io::SeekFrom::End(-1)).is_ok() && file.read_exact(&mut last).is_ok() && last[0] != b'\n' {
      record.insert(0, '\n');
    }
    file.write_all(record.as_bytes()).map_err(|e| e.to_string())
  }

  /// The operations of this workspace, oldest first: `(milliseconds, what, commit)`.
  pub fn operations(&self) -> Vec<(u128, String, String)> {
    let text = std::fs::read_to_string(self.root.join(META).join("oplog")).unwrap_or_default();
    // A line torn by a kill names nothing: only whole records are read.
    let parse = |l: &str| {
      let mut f = l.split(' ');
      let record = (f.next()?.parse().ok()?, f.next()?.to_string(), f.next()?.to_string());
      (f.next().is_none() && record.2.len() == 64 && record.2.bytes().all(|b| b.is_ascii_hexdigit())).then_some(record)
    };
    text.lines().filter_map(parse).collect()
  }

  /// Say that `path` (relative to the workspace) was written or removed. A
  /// snapshot taken with `Look::Recorded` looks at recorded paths and nothing else.
  /// Note that a path was written. The path is one inside the workspace:
  /// relative to its top, or absolute and below it. Anything else is
  /// refused, since what is noted here is read and committed.
  pub fn record(&self, path: &[u8]) -> Result<()> {
    self.record_all(&[path])
  }

  /// `record` for several paths, all of them or none: each is checked before any is noted.
  pub fn record_all(&self, paths: &[&[u8]]) -> Result<()> {
    // `.nv/exclude` is not a file of the tree, and a change to it is seen without a report.
    let rules = self.root.join(META).join("exclude");
    let theirs = |path: &[u8]| {
      let full = if path.first() == Some(&b'/') { to_path(path).to_path_buf() } else { self.root.join(to_path(path)) };
      full.parent().and_then(|dir| std::fs::canonicalize(dir).ok()).map(|dir| dir.join(full.file_name().unwrap_or_default()))
        == rules.parent().and_then(|dir| std::fs::canonicalize(dir).ok()).map(|dir| dir.join("exclude"))
    };
    let inside: Vec<Vec<u8>> = paths.iter().filter(|path| !theirs(path)).map(|path| self.inside(path)).collect::<Result<_>>()?;
    self.note(&inside)
  }

  /// A path as the workspace names it: from its top, with no `.` or `..` in it.
  pub fn inside(&self, path: &[u8]) -> Result<Vec<u8>> {
    let refused = || format!("`{}` is not a path inside the workspace", String::from_utf8_lossy(path));
    let mut relative = path.to_vec();
    if path.first() == Some(&b'/') {
      let root = std::fs::canonicalize(&self.root).map_err(|e| e.to_string())?;
      let root = root.as_os_str().as_encoded_bytes();
      // The directory may be reached through a link; the file may not exist any more.
      let given = to_path(path);
      let parent = given.parent().and_then(|p| std::fs::canonicalize(p).ok()).ok_or_else(refused)?;
      let full = parent.join(given.file_name().ok_or_else(refused)?);
      let full = full.as_os_str().as_encoded_bytes();
      relative = full.strip_prefix(root).and_then(|rest| rest.strip_prefix(b"/")).ok_or_else(refused)?.to_vec();
    }
    // `..` takes back the name before it, and may not take back more than there is.
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in relative.split(|&b| b == b'/').filter(|part| !part.is_empty() && *part != b".") {
      if part == b".." {
        parts.pop().ok_or_else(refused)?;
      } else {
        parts.push(part);
      }
    }
    if parts.is_empty() || SKIPPED.iter().any(|skipped| parts[0] == skipped.as_bytes()) {
      return Err(refused());
    }
    // A directory on the way that is a link leads somewhere else, perhaps out of the workspace.
    for upto in 1..parts.len() {
      let dir = self.root.join(to_path(&parts[..upto].join(&b'/')));
      if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(format!("`{}` is beyond a symbolic link", String::from_utf8_lossy(path)));
      }
    }
    Ok(parts.join(&b'/'))
  }

  /// Note paths to be looked at by the next command that trusts reported writes: ones nv knows of
  /// itself (they were recorded and are no longer), which need not be paths a writer may report.
  pub(crate) fn note(&self, paths: &[Vec<u8>]) -> Result<()> {
    use std::io::Write;
    if paths.is_empty() {
      return Ok(());
    }
    let mut file =
      std::fs::File::options().append(true).create(true).open(self.root.join(META).join("journal")).map_err(|e| e.to_string())?;
    let all: Vec<u8> = paths.iter().flat_map(|path| path.iter().copied().chain([0])).collect();
    file.write_all(&all).map_err(|e| e.to_string())
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
  /// Only at the files the index knows: what changed among them, not what is new.
  Tracked,
}

fn blob(store: &mut Store, id: &Id) -> Result<std::sync::Arc<Vec<u8>>> {
  store.get(id)?.ok_or_else(|| format!("blob {} is not in the store", hex(id)))
}

/// Where the store keeps the one materialised copy of a blob that every
/// workspace clones. Spread over 256 directories: a workspace of a hundred
/// thousand files should not make one directory of a hundred thousand.
fn shared_copy(files_dir: &Path, id: &Id) -> PathBuf {
  files_dir.join(hex(&id[..1])).join(hex(id))
}

/// One file to write into a workspace. `bytes` is its content when the
/// store's shared copy does not exist yet, or when it is a link's target.
struct Job {
  path: Vec<u8>,
  entry: Entry,
  bytes: Option<std::sync::Arc<Vec<u8>>>,
  /// Write the content where it goes, with no shared copy: for a tree that
  /// is itself what workspaces are cloned from.
  direct: bool,
}

/// Write one file. Touches the disk and nothing else, so many can run at once.
/// Returns the size and modification time the file ended up with.
fn place_one(files_dir: &Path, root: &Path, job: &Job) -> Result<(u64, u128)> {
  let dest = root.join(to_path(&job.path));
  let fail = |what: &str, at: &Path, e: std::io::Error| format!("cannot {what} {}: {e}", at.display());
  // A link standing at the path is removed, never written through: where it leads is not the
  // workspace's. A directory holding nothing but directories makes way too.
  match std::fs::symlink_metadata(&dest) {
    Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(&dest).map_err(|e| fail("remove the link at", &dest, e))?,
    Ok(meta) if meta.is_dir() && job.entry.mode != MODE_GITLINK => remove_empty(&dest).map_err(|e| fail("write", &dest, e))?,
    _ => {}
  }
  if job.entry.mode == MODE_GITLINK {
    std::fs::create_dir_all(&dest).map_err(|e| fail("create", &dest, e))?;
    return Ok((0, 0));
  }
  if job.entry.mode == MODE_LINK {
    let to = to_path(job.bytes.as_deref().ok_or("a link without a target")?);
    if std::os::unix::fs::symlink(&to, &dest).is_err() {
      let _ = std::fs::remove_file(&dest);
      std::os::unix::fs::symlink(&to, &dest).map_err(|e| fail("link", &dest, e))?;
    }
  } else if job.direct {
    std::fs::write(&dest, job.bytes.as_deref().ok_or("a file without content")?.as_slice()).map_err(|e| fail("write", &dest, e))?;
    if job.entry.mode & 0o111 != 0 {
      std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| fail("mark executable", &dest, e))?;
    }
  } else {
    let cached = shared_copy(files_dir, &job.entry.id);
    if let Some(bytes) = &job.bytes {
      // Written under another name first: a reader never sees half a file.
      let temp = cached.with_extension(format!("tmp{}-{:?}", std::process::id(), std::thread::current().id()));
      std::fs::write(&temp, bytes.as_slice()).map_err(|e| fail("write", &temp, e))?;
      std::fs::rename(&temp, &cached).map_err(|e| fail("write", &cached, e))?;
    }
    clone_file(&cached, &dest).map_err(|e| fail("write", &dest, e))?;
    if job.entry.mode & 0o111 != 0 {
      std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| fail("mark executable", &dest, e))?;
    }
  }
  let meta = std::fs::symlink_metadata(&dest).map_err(|e| fail("look at", &dest, e))?;
  Ok((meta.len(), mtime_of(&meta)))
}

fn shared_dirs(files_dir: &Path) -> Result<()> {
  for byte in 0..=255u8 {
    let dir = files_dir.join(hex(&[byte]));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
  }
  Ok(())
}

/// An index entry for a file placed with this size and time. A time too
/// close to `now` is not trusted: see `RACY_NANOS`.
fn vouch(entry: Entry, size: u64, mtime: u128, now: u128) -> Indexed {
  // nv has just written the file: it has not changed after now. The moment itself is noted the first time it is looked at.
  Indexed { entry, size, mtime: if now.saturating_sub(mtime) < RACY_NANOS { 0 } else { mtime }, ctime: NOT_AFTER | now as u64 }
}

/// The files one command wrote, and what the index should hold for each.
/// Whether a file's time can be trusted is judged when the command is done
/// writing, not as each file lands: judged at once, every file is younger
/// than `RACY_NANOS` and the next look would read them all again.
#[derive(Default)]
pub(crate) struct Placed(Vec<(Vec<u8>, Indexed)>);

impl Placed {
  pub(crate) fn add(&mut self, path: Vec<u8>, placed: Indexed) {
    self.0.push((path, placed));
  }
  pub(crate) fn into_index(self, index: &mut Index) {
    let now = now_nanos();
    for (path, placed) in self.0 {
      index.insert(path, vouch(placed.entry, placed.size, placed.mtime, now));
    }
  }
}

/// Write one file of a commit into a workspace, from the store's shared
/// copy, and say what it is on disk: its entry, size and time as they are,
/// for `Placed` to judge.
pub(crate) fn place(store: &mut Store, root: &Path, path: &[u8], entry: Entry) -> Result<Indexed> {
  place_with(store, root, path, entry, &mut Eols::of_worktree(root))
}

fn is_attributes(path: &[u8]) -> bool {
  path.rsplit(|&b| b == b'/').next() == Some(b".gitattributes")
}

/// The content of a regular file as its rules want it in a workspace, when
/// that is not the content stored: the file is then written as it is, and
/// shares nothing with the store's copy.
fn converted(store: &mut Store, eols: &mut Eols, path: &[u8], entry: Entry) -> Result<Option<std::sync::Arc<Vec<u8>>>> {
  if eols.none() || (entry.mode != 0o100644 && entry.mode != 0o100755) {
    return Ok(None);
  }
  let how = eols.how(path);
  if !matches!(how, crate::eol::How::Text { crlf: true, .. }) {
    return Ok(None);
  }
  Ok(crate::eol::to_worktree(how, &blob(store, &entry.id)?).map(std::sync::Arc::new))
}

/// The line-ending rules of a tree about to be written: the `.gitattributes`
/// among `found`, and for a view of part of the tree, those above the view.
fn tree_eols(store: &mut Store, root: Id, found: &[(Vec<u8>, Entry)], prefixes: &[Vec<u8>]) -> Result<Eols> {
  let mut files: Vec<(Vec<u8>, Entry)> =
    found.iter().filter(|(path, entry)| is_attributes(path) && entry.mode & 0o170000 == 0o100000).cloned().collect();
  for prefix in prefixes {
    let mut dir = prefix.as_slice();
    loop {
      dir = dir.iter().rposition(|&b| b == b'/').map_or(&dir[..0], |at| &dir[..at]);
      let path = if dir.is_empty() { b".gitattributes".to_vec() } else { [dir, b"/.gitattributes"].concat() };
      if !files.iter().any(|(p, _)| *p == path)
        && let Some(entry) = crate::manifest::lookup(store, root, &path)?
      {
        files.push((path, entry));
      }
      if dir.is_empty() {
        break;
      }
    }
  }
  let mut read = Vec::with_capacity(files.len());
  for (path, entry) in files {
    let dir = path.len().saturating_sub(".gitattributes".len() + 1);
    read.push((path[..dir].to_vec(), blob(store, &entry.id)?));
  }
  Ok(Eols::of_files(read))
}

/// `place`, for many files of one workspace: the rules are read once.
pub(crate) fn place_with(store: &mut Store, root: &Path, path: &[u8], entry: Entry, eols: &mut Eols) -> Result<Indexed> {
  let files_dir = store.dir.join("files");
  let cached = shared_copy(&files_dir, &entry.id);
  let fail = |at: &Path, e: std::io::Error| format!("cannot create {}: {e}", at.display());
  std::fs::create_dir_all(cached.parent().expect("under files")).map_err(|e| fail(&cached, e))?;
  let dest = root.join(to_path(path));
  // A link where a directory of the tree has to be makes way for the directory: the file is
  // written in the workspace, never wherever the link leads.
  while let Some(link) = link_on_the_way(root, path) {
    std::fs::remove_file(&link).map_err(|e| fail(&link, e))?;
  }
  std::fs::create_dir_all(dest.parent().expect("under the workspace")).map_err(|e| fail(&dest, e))?;
  if let Some(bytes) = converted(store, eols, path, entry)? {
    // Whatever is there, a link included, makes way: the bytes go into a file of this name.
    let _ = std::fs::remove_file(&dest);
    let (size, mtime) = place_one(&files_dir, root, &Job { path: path.to_vec(), entry, bytes: Some(bytes), direct: true })?;
    return Ok(Indexed { entry, size, mtime, ctime: 0 });
  }
  // The store's ready copy of the file is a plain file on disk: before it is cloned it is read and
  // compared with its name, and one that is no longer the file is made again from the store.
  let ready = entry.mode != MODE_GITLINK
    && entry.mode != MODE_LINK
    && match std::fs::read(&cached) {
      Ok(bytes) if object_id(Kind::Blob, &bytes) == entry.id => true,
      Ok(_) => {
        let _ = std::fs::remove_file(&cached);
        false
      }
      Err(_) => false,
    };
  let needs_bytes = entry.mode != MODE_GITLINK && (entry.mode == MODE_LINK || !ready);
  let bytes = if needs_bytes { Some(blob(store, &entry.id)?) } else { None };
  let (size, mtime) = place_one(&files_dir, root, &Job { path: path.to_vec(), entry, bytes, direct: false })?;
  if is_attributes(path) {
    eols.forget();
  }
  Ok(Indexed { entry, size, mtime, ctime: 0 })
}

/// Remove a file from a workspace, and the directories that leaves empty.
/// Changes in the order to make them on disk: what goes, then what comes.
/// Where two names are one file to the file system, a change may remove one
/// and write the other; written first, the file would then be removed.
pub(crate) fn removals_first(mut changes: Vec<Change>) -> Vec<Change> {
  changes.sort_by_key(|(_, entry)| entry.is_some());
  changes
}

/// The first directory on the way to `path` that is a symbolic link. What is beyond it is not
/// the workspace's: it may be anywhere on the machine.
pub(crate) fn link_on_the_way(root: &Path, path: &[u8]) -> Option<PathBuf> {
  let mut at = root.to_path_buf();
  let mut parts = path.split(|&b| b == b'/').peekable();
  while let Some(part) = parts.next() {
    if parts.peek().is_none() {
      break;
    }
    at.push(to_path(part));
    if std::fs::symlink_metadata(&at).is_ok_and(|meta| meta.file_type().is_symlink()) {
      return Some(at);
    }
  }
  None
}

pub(crate) fn unplace(root: &Path, path: &[u8]) -> bool {
  // Beyond a link there is no file of this workspace to remove: the file is gone already.
  if link_on_the_way(root, path).is_some() {
    return true;
  }
  let dest = root.join(to_path(path));
  // Gone already is as good as removed; still there afterwards is not.
  let _ = std::fs::remove_file(&dest);
  let removed = std::fs::symlink_metadata(&dest).is_err();
  // The directories it leaves empty go too, but not the one the command was run from, nor one
  // above it: someone stands there, and Git leaves it (twelfth round: the caller's next command
  // found its own directory gone).
  let standing = std::env::current_dir().ok();
  let mut dir = dest.parent();
  while let Some(d) = dir.filter(|d| *d != root) {
    let stood_in = standing.as_ref().is_some_and(|cwd| std::fs::canonicalize(d).is_ok_and(|d| cwd.starts_with(d)));
    if stood_in || std::fs::remove_dir(d).is_err() {
      break;
    }
    dir = d.parent();
  }
  removed
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

/// Content read from the store and waiting to be written is kept below this.
const BATCH_BYTES: usize = 64 << 20;

/// Write a batch of files on every core. The files are independent, and what
/// each costs is system calls, which is where the time of a checkout goes.
fn place_all(files_dir: &Path, root: &Path, jobs: &[Job]) -> Result<Vec<(u64, u128)>> {
  let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(jobs.len().div_ceil(64)).max(1);
  let chunk = jobs.len().div_ceil(threads).max(1);
  let placed: Vec<Result<Vec<(u64, u128)>>> = std::thread::scope(|scope| {
    let handles: Vec<_> =
      jobs.chunks(chunk).map(|part| scope.spawn(move || part.iter().map(|job| place_one(files_dir, root, job)).collect())).collect();
    handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("a checkout thread panicked".to_string()))).collect()
  });
  let mut out = Vec::with_capacity(jobs.len());
  for part in placed {
    out.extend(part?);
  }
  Ok(out)
}

/// What stands, untracked, where the changes would write: a file under the name of one to be
/// written (to the file system's mind: on one that folds case, `WAY` is where `way` goes), or a
/// file where a directory has to be made. A link where a directory has to be is counted too,
/// unless the commit left tracks a file at that very path.
fn untracked_in_the_way(store: &mut Store, root: &Path, from: Option<Id>, changes: &[Change]) -> Result<Vec<String>> {
  let leaving: std::collections::HashSet<Vec<u8>> =
    changes.iter().filter(|(_, entry)| entry.is_none()).map(|(path, _)| Names::fold(path)).collect();
  let tracked = |store: &mut Store, path: &[u8]| -> Result<bool> {
    Ok(match from {
      // As a file: a directory the commit has is not a file it has at that path.
      Some(root) => crate::manifest::lookup(store, root, path)?.is_some_and(|entry| !entry.is_dir()),
      None => false,
    })
  };
  let mut found = std::collections::BTreeSet::new();
  for (path, _) in changes.iter().filter(|(_, entry)| entry.is_some()) {
    // The directories on the way.
    let mut upto = 0;
    while let Some(slash) = path[upto..].iter().position(|&b| b == b'/') {
      let parent = &path[..upto + slash];
      upto += slash + 1;
      let Ok(meta) = std::fs::symlink_metadata(root.join(to_path(parent))) else { continue };
      if meta.is_dir() || leaving.contains(&Names::fold(parent)) || tracked(store, parent)? {
        continue;
      }
      // A link standing where the commit left has a directory is that directory replaced: it
      // makes way. A file there, or a link where the commit has nothing, is somebody's.
      let was_dir = match from {
        Some(root) => crate::manifest::lookup(store, root, parent)?.is_some_and(|entry| entry.is_dir()),
        None => false,
      };
      if !(meta.file_type().is_symlink() && was_dir) {
        found.insert(String::from_utf8_lossy(parent).into_owned());
      }
    }
    // The file itself, where the commit left has none of that name. (Seen through a link, it is
    // not in the workspace at all: the link is what stands there.)
    if link_on_the_way(root, path).is_none()
      && std::fs::symlink_metadata(root.join(to_path(path))).is_ok_and(|meta| !meta.is_dir())
      && !tracked(store, path)?
      && !leaving.contains(&Names::fold(path))
    {
      found.insert(String::from_utf8_lossy(path).into_owned());
    }
  }
  Ok(found.into_iter().collect())
}

/// Refused, before a commit is made of what is recorded: files holding the markers a sync writes
/// where both sides changed the same lines (put back, say, by a restore after the sync's own note
/// was cleared). Called once what is on disk is recorded, so only what is recorded is read: the
/// files that differ from the last commit, as the commit would publish them.
pub(crate) fn refuse_sync_markers(store: &mut Store, ws: &Workspace) -> Result<()> {
  refuse_sync_markers_in(store, ws, ws.head)
}

/// `refuse_sync_markers`, for the change from the last commit to `state`.
fn refuse_sync_markers_in(store: &mut Store, ws: &Workspace, state: Option<Id>) -> Result<()> {
  let tree = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  let (from, to) = (tree(store, ws.base)?, tree(store, state)?);
  let mut found = Vec::new();
  // A file the index says is still what was recorded is read from the disk, which costs less
  // than taking its content out of the store; what the index cannot vouch for is read from the store.
  let index = ws.load_index();
  let (noted, here) = noted_for(ws);
  let ctimes = match std::fs::read_to_string(&noted).is_ok_and(|was| was == here) {
    true => Ctimes::Noted,
    false => Ctimes::CopiedBy(std::fs::symlink_metadata(&noted).map_or(0, |meta| ctime_of(&meta))),
  };
  for (path, entry) in crate::manifest::diff(store, from, to)? {
    let Some(entry) = entry.filter(|e| !e.is_dir() && e.mode != crate::manifest::MODE_GITLINK && e.mode != MODE_LINK) else { continue };
    let full = ws.root.join(to_path(&path));
    let on_disk = index.get(&path).filter(|known| known.entry == entry).and_then(|known| {
      let meta = std::fs::symlink_metadata(&full).ok().filter(|meta| meta.is_file())?;
      if !unchanged(&known, &meta, ctimes) || link_on_the_way(&ws.root, &path).is_some() {
        return None;
      }
      read_regular(&full).filter(|bytes| bytes.len() as u64 == meta.len())
    });
    let bytes = match on_disk {
      Some(bytes) => std::sync::Arc::new(bytes),
      None => match store.get(&entry.id)? {
        Some(bytes) => bytes,
        None => continue,
      },
    };
    let lines = || bytes.split(|&b| b == b'\n');
    if lines().any(|line| line == b"<<<<<<< yours") && lines().any(|line| line == b">>>>>>> theirs") {
      found.push(String::from_utf8_lossy(&path).into_owned());
    }
  }
  if found.is_empty() {
    return Ok(());
  }
  Err(format!(
    "these hold both versions between the markers a sync leaves (<<<<<<< yours ... >>>>>>> theirs): {}\nEdit them to what they should be, then commit.",
    found.join(", ")
  ))
}

/// The bytes of a regular file at `path` itself: `None` for a link (not followed), a pipe or a
/// device (not waited on), or anything that cannot be read. What is opened is what is checked,
/// so a file replaced by a pipe in between is not read either.
fn read_regular(path: &Path) -> Option<Vec<u8>> {
  use std::io::Read;
  use std::os::unix::fs::OpenOptionsExt;
  let mut file = std::fs::File::options().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).ok()?;
  if !file.metadata().ok()?.is_file() {
    return None;
  }
  let mut bytes = Vec::new();
  file.read_to_end(&mut bytes).ok()?;
  Some(bytes)
}

/// The store's ready copies, each compared with what it is a copy of: single files under
/// `files/`, and whole trees under `trees/`. They are plain files that a checkout clones without
/// reading; this reads them all. One that differs is removed, to be made again when next needed,
/// and named in what is returned.
pub fn check_copies(store: &mut Store) -> Result<Vec<String>> {
  let mut wrong = Vec::new();
  let files = store.dir.join("files");
  for dir in std::fs::read_dir(&files).into_iter().flatten().flatten() {
    for entry in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
      let path = entry.path();
      let named = entry.file_name().to_str().and_then(crate::unhex).and_then(|bytes| Id::try_from(bytes).ok());
      let (Some(id), Ok(bytes)) = (named, std::fs::read(&path)) else { continue };
      if object_id(Kind::Blob, &bytes) != id {
        let _ = std::fs::remove_file(&path);
        wrong.push(format!("{}: the ready copy of this file had other bytes, and was removed", path.display()));
      }
    }
  }
  let trees = store.dir.join("trees");
  for entry in std::fs::read_dir(&trees).into_iter().flatten().flatten() {
    let dir = entry.path();
    let named = entry.file_name().to_str().and_then(crate::unhex).and_then(|bytes| Id::try_from(bytes).ok());
    let Some(root) = named.filter(|_| dir.is_dir()) else { continue };
    let mut expected: Vec<(Vec<u8>, Entry)> = Vec::new();
    crate::manifest::walk(store, root, &[], &mut |path, file| {
      expected.push((path.to_vec(), file));
      Ok(())
    })?;
    let mut eols = Eols::of_worktree(&dir);
    let differs = expected.iter().filter(|(_, file)| file.mode != MODE_GITLINK).any(|(path, file)| {
      let full = dir.join(to_path(path));
      let Ok(meta) = std::fs::symlink_metadata(&full) else { return true };
      let Ok((_, bytes)) = read_content(&full, &meta) else { return true };
      object_id(Kind::Blob, &bytes) != file.id
        && crate::eol::to_store(eols.how(path), &bytes).is_none_or(|stored| object_id(Kind::Blob, &stored) != file.id)
    });
    let listed = Index::open(&trees.join(format!("{}.meta", hex(&root)))).entries();
    let index_differs = listed.len() != expected.len()
      || listed.iter().zip(&expected).any(|((path, known), (wanted, file))| path != wanted || known.entry != *file);
    if differs || index_differs {
      let _ = std::fs::remove_dir_all(&dir);
      let _ = std::fs::remove_dir_all(trees.join(format!("{}.meta", hex(&root))));
      wrong.push(format!("{}: the ready copy of this tree had a file with other bytes, and was removed", dir.display()));
    }
  }
  Ok(wrong)
}

/// Whole trees kept in the store, ready to be cloned.
const TREES_KEPT: usize = 2;

/// The store's own copy of the tree `root`, complete on disk, and the index
/// that goes with it: `(tree, directory holding the index, files written)`.
///
/// A workspace of everything is one clone of this. If the tree is not there
/// but another is, that one is cloned and the difference applied, so a
/// commit near one already seen costs its changes. Only the first tree of a
/// store is written file by file.
fn whole_tree(store: &mut Store, root: Id) -> Result<(PathBuf, PathBuf, usize)> {
  let trees = store.dir.join("trees");
  let fail = |what: &str, path: &Path, e: std::io::Error| format!("cannot {what} {}: {e}", path.display());
  std::fs::create_dir_all(&trees).map_err(|e| fail("create", &trees, e))?;
  let (tree, meta) = (trees.join(hex(&root)), trees.join(format!("{}.meta", hex(&root))));
  if tree.is_dir() && meta.join("index").exists() {
    return Ok((tree, meta, 0));
  }
  // The trees there are, newest first.
  let mut others: Vec<(std::time::SystemTime, Id)> = Vec::new();
  for entry in std::fs::read_dir(&trees).map_err(|e| fail("list", &trees, e))?.flatten() {
    let id = entry.file_name().to_str().and_then(unhex).and_then(|b| Id::try_from(b).ok());
    if let (Some(id), Ok(meta)) = (id, entry.metadata()) {
      others.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), id));
    }
  }
  others.sort_by_key(|a| std::cmp::Reverse(a.0));
  let unique = format!("tmp{}", std::process::id());
  let (tree_tmp, meta_tmp) = (trees.join(format!("{}.{unique}", hex(&root))), trees.join(format!("{}.meta.{unique}", hex(&root))));
  let _ = (std::fs::remove_dir_all(&tree_tmp), std::fs::remove_dir_all(&meta_tmp));
  let mut written = 0;

  let near = others.first().map(|(_, id)| *id).filter(|id| trees.join(format!("{}.meta", hex(id))).join("index").exists());
  let mut built = false;
  if let Some(from) = near {
    let changes = crate::manifest::diff(store, Some(from), Some(root))?;
    let mut entries: std::collections::BTreeMap<Vec<u8>, Indexed> =
      Index::open(&trees.join(format!("{}.meta", hex(&from)))).entries().into_iter().collect();
    // Past a point, writing afresh is cheaper than cloning and undoing. And a change of
    // line-ending rules changes files that did not change: then too, afresh.
    if changes.len() <= entries.len() / 2 + 64 && !changes.iter().any(|(path, _)| is_attributes(path)) {
      clone_tree(&trees.join(hex(&from)), &tree_tmp).map_err(|e| fail("clone into", &tree_tmp, e))?;
      let mut eols = Eols::of_worktree(&tree_tmp);
      // What goes, first: a file may be written where a directory of files that go stood.
      for (path, entry) in &removals_first(changes.clone()) {
        match entry {
          Some(entry) => {
            // The time as it is: a tree in the store is never written again,
            // so whether a time can be trusted is for each workspace to judge.
            let placed = place_with(store, &tree_tmp, path, *entry, &mut eols)?;
            let mtime = std::fs::symlink_metadata(tree_tmp.join(to_path(path))).map_or(0, |m| mtime_of(&m));
            entries.insert(path.clone(), Indexed { mtime, ..placed });
            written += 1;
          }
          None => {
            unplace(&tree_tmp, path);
            entries.remove(path);
          }
        }
      }
      Index::create(&meta_tmp, entries.into_iter().collect())?;
      built = true;
    }
  }
  if !built {
    std::fs::create_dir_all(&tree_tmp).map_err(|e| fail("create", &tree_tmp, e))?;
    let mut found: Vec<(Vec<u8>, Entry)> = Vec::new();
    walk(store, root, &[], &mut |path, entry| {
      found.push((path.to_vec(), entry));
      Ok(())
    })?;
    make_dirs(&tree_tmp, &found)?;
    let mut eols = tree_eols(store, root, &found, &[])?;
    let mut index: Vec<(Vec<u8>, Entry, u64, u128)> = Vec::with_capacity(found.len());
    let files_dir = store.dir.join("files");
    let mut jobs: Vec<Job> = Vec::new();
    let mut held = 0usize;
    let mut flush = |jobs: &mut Vec<Job>| -> Result<()> {
      for (job, (size, mtime)) in jobs.iter().zip(place_all(&files_dir, &tree_tmp, jobs)?) {
        index.push((job.path.clone(), job.entry, size, mtime));
      }
      jobs.clear();
      Ok(())
    };
    for (path, entry) in found {
      let bytes = match converted(store, &mut eols, &path, entry)? {
        Some(bytes) => Some(bytes),
        None if entry.mode == MODE_GITLINK => None,
        None => Some(blob(store, &entry.id)?),
      };
      held += bytes.as_ref().map_or(0, |b| b.len());
      written += (entry.mode != MODE_GITLINK && entry.mode != MODE_LINK) as usize;
      jobs.push(Job { path, entry, bytes, direct: true });
      if held >= BATCH_BYTES {
        flush(&mut jobs)?;
        held = 0;
      }
    }
    flush(&mut jobs)?;
    Index::create(
      &meta_tmp,
      index.into_iter().map(|(path, entry, size, mtime)| (path, Indexed { entry, size, mtime, ctime: 0 })).collect(),
    )?;
  }
  // Into place under its name. If another process got there first, its tree is as good.
  if std::fs::rename(&tree_tmp, &tree).is_ok() {
    let _ = std::fs::remove_dir_all(&meta);
    std::fs::rename(&meta_tmp, &meta).map_err(|e| fail("write", &meta, e))?;
  } else {
    let _ = (std::fs::remove_dir_all(&tree_tmp), std::fs::remove_dir_all(&meta_tmp));
  }
  // Keep the newest few. A tree shares its blocks with the workspaces cloned
  // from it, so what an old one holds on to is mostly names.
  for (_, id) in others.iter().skip(TREES_KEPT - 1) {
    let _ = std::fs::remove_dir_all(trees.join(hex(id)));
    let _ = std::fs::remove_dir_all(trees.join(format!("{}.meta", hex(id))));
  }
  Ok((tree, meta, written))
}

/// Create the directories a list of files needs: each once, not once per file in it.
fn make_dirs(root: &Path, files: &[(Vec<u8>, Entry)]) -> Result<()> {
  let mut dirs: std::collections::BTreeSet<&[u8]> = std::collections::BTreeSet::new();
  for (path, _) in files {
    if let Some(slash) = path.iter().rposition(|&b| b == b'/') {
      dirs.insert(&path[..slash]);
    }
  }
  for dir in dirs {
    let dir = root.join(to_path(dir));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
  }
  Ok(())
}

/// Write the files of `commit` under `prefixes` (all of it when empty) to `target`.
///
/// A workspace of everything is cloned in one call from the store's copy of
/// that tree (`whole_tree`). A workspace of some prefixes is small by
/// intent and is written file by file, each cloned from the store's shared
/// copy of its content.
pub fn checkout(store: &mut Store, commit: Id, target: &Path, prefixes: Vec<Vec<u8>>) -> Result<CheckedOut> {
  let fail = |what: &str, path: &Path, e: std::io::Error| format!("cannot {what} {}: {e}", path.display());
  let vacant = match std::fs::read_dir(target) {
    Ok(mut entries) => entries.next().is_none(),
    Err(_) => !target.exists(),
  };
  if std::fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink()) {
    return Err(format!("{} is a symbolic link: a checkout makes a workspace in a directory, not through a link to one", target.display()));
  }
  // On a file system that takes `Link` and `link` for one name, a tree that has both cannot be
  // written as it is: one would be written through the other.
  if let Some(root) = root_of(store, &commit)? {
    std::fs::create_dir_all(target).map_err(|e| fail("create", target, e))?;
    // A tree is looked over once: that it has no such names is noted beside the store's copy of it.
    let noted = store.dir.join("trees").join(format!("{}.names-checked", hex(&root)));
    if !noted.exists() && folds_names(target) {
      if let Some((a, b)) = crate::manifest::folded_twins(store, root, b"")? {
        return Err(format!(
          "the commit has both `{}` and `{}`, which this file system takes for one name: it cannot be checked out here",
          String::from_utf8_lossy(&a),
          String::from_utf8_lossy(&b)
        ));
      }
      if std::fs::create_dir_all(store.dir.join("trees")).is_ok() {
        let _ = std::fs::write(&noted, b"");
      }
    }
  }
  if !vacant {
    return Err(format!(
      "{} is there already and is not empty: a checkout makes a workspace, it does not write over files. To go to another commit in a workspace, use `nv restore` or `nv git switch` there; or check out into a new directory.",
      target.display()
    ));
  }
  if let (true, true, Some(root)) = (prefixes.is_empty(), vacant, root_of(store, &commit)?) {
    let (tree, meta, written) = whole_tree(store, root)?;
    let _ = std::fs::remove_dir(target);
    if let Some(parent) = target.parent() {
      std::fs::create_dir_all(parent).map_err(|e| fail("create", parent, e))?;
    }
    clone_tree(&tree, target).map_err(|e| fail("clone into", target, e))?;
    // The clone keeps each file's time, so the tree's index describes the
    // workspace too. Only the times of files written a moment ago are not
    // taken on trust, and those there are only in a tree just built.
    let now = now_nanos();
    let entries = Index::open(&meta).entries();
    Index::create(&target.join(META), entries.into_iter().map(|(path, i)| (path, vouch(i.entry, i.size, i.mtime, now))).collect())?;
    note_here(target)?;
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
      start: Some(commit),
      seq: 0,
    };
    ws.save()?;
    let mut done = CheckedOut { materialised: written, ..CheckedOut::default() };
    for (_, indexed) in ws.load_index().entries() {
      if indexed.entry.mode != MODE_GITLINK {
        done.files += 1;
        done.bytes += if indexed.entry.mode == MODE_LINK { 0 } else { indexed.size };
      }
    }
    return Ok(done);
  }
  let files_dir = store.dir.join("files");
  shared_dirs(&files_dir)?;
  std::fs::create_dir_all(target).map_err(|e| fail("create", target, e))?;
  let mut found: Vec<(Vec<u8>, Entry)> = Vec::new();
  if let Some(root) = root_of(store, &commit)? {
    walk(store, root, &prefixes, &mut |path, entry| {
      found.push((path.to_vec(), entry));
      Ok(())
    })?;
  }
  make_dirs(target, &found)?;
  let mut eols = match root_of(store, &commit)? {
    Some(root) => tree_eols(store, root, &found, &prefixes)?,
    None => Eols::of_files([]),
  };
  // Which shared copies exist already: one listing, not a question per file.
  let mut have: std::collections::HashSet<Id> = std::collections::HashSet::new();
  for fan in std::fs::read_dir(&files_dir).map_err(|e| fail("list", &files_dir, e))?.flatten() {
    for file in std::fs::read_dir(fan.path()).into_iter().flatten().flatten() {
      if let Some(id) = file.file_name().to_str().and_then(unhex).and_then(|b| Id::try_from(b).ok()) {
        have.insert(id);
      }
    }
  }

  let mut done = CheckedOut::default();
  let mut index: Vec<(Vec<u8>, Entry, u64, u128)> = Vec::with_capacity(found.len());
  let mut jobs: Vec<Job> = Vec::new();
  let mut held = 0usize;
  let mut flush = |jobs: &mut Vec<Job>, done: &mut CheckedOut| -> Result<()> {
    for (job, (size, mtime)) in jobs.iter().zip(place_all(&files_dir, target, jobs)?) {
      if job.entry.mode != MODE_GITLINK {
        done.files += 1;
        done.bytes += if job.entry.mode == MODE_LINK { 0 } else { size };
      }
      index.push((job.path.clone(), job.entry, size, mtime));
    }
    jobs.clear();
    Ok(())
  };
  for (path, entry) in found {
    let regular = entry.mode != MODE_GITLINK && entry.mode != MODE_LINK;
    if let Some(bytes) = converted(store, &mut eols, &path, entry)? {
      held += bytes.len();
      done.materialised += 1;
      jobs.push(Job { path, entry, bytes: Some(bytes), direct: true });
      continue;
    }
    // Two files of one content: the first writes the shared copy, the second finds it there.
    let bytes = if entry.mode == MODE_LINK || (regular && have.insert(entry.id)) {
      let bytes = blob(store, &entry.id)?;
      held += bytes.len();
      done.materialised += regular as usize;
      Some(bytes)
    } else {
      None
    };
    jobs.push(Job { path, entry, bytes, direct: false });
    if held >= BATCH_BYTES {
      flush(&mut jobs, &mut done)?;
      held = 0;
    }
  }
  flush(&mut jobs, &mut done)?;

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
    start: Some(commit),
    seq: 0,
  };
  ws.save()?;
  // Whether a time is too fresh to trust is judged now, with the last file
  // written, not as each one was: only the files of the last moments are.
  let now = now_nanos();
  Index::create(&target.join(META), index.into_iter().map(|(path, entry, size, mtime)| (path, vouch(entry, size, mtime, now))).collect())?;
  note_here(target)?;
  let _ = std::fs::remove_file(target.join(META).join("journal"));
  Ok(done)
}

/// A file on disk: its mode and bytes.
pub type Content = (u32, Vec<u8>);
/// `(path, what is on disk now, the entry last recorded)`.
pub type Difference = (Vec<u8>, Option<Content>, Option<Entry>);

#[derive(Default, Debug, Clone)]
pub struct Looked {
  /// Files whose size and time were looked at, and files that had to be read.
  pub stats: usize,
  pub reads: usize,
  /// Files that had to be read and may not be: left out of what is recorded.
  pub unreadable: Vec<Vec<u8>>,
}

/// Why what is at a path is not known.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unknown {
  /// Beyond a link: no file of this workspace's is there.
  BeyondLink,
  /// Something is there that is neither a file nor a link (a pipe, a device), or cannot be read.
  Unreadable,
}

/// What is at `path` in the workspace: a file or a link, read; `None` where nothing is, or a
/// directory. Nothing else is read: a pipe would be waited on for ever.
pub(crate) fn content_at(root: &Path, path: &[u8]) -> std::result::Result<Option<Content>, Unknown> {
  if link_on_the_way(root, path).is_some() {
    return Err(Unknown::BeyondLink);
  }
  let full = root.join(to_path(path));
  match std::fs::symlink_metadata(&full) {
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(_) => Err(Unknown::Unreadable),
    Ok(meta) if meta.is_dir() => Ok(None),
    Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => read_content(&full, &meta).map(Some).map_err(|_| Unknown::Unreadable),
    Ok(_) => Err(Unknown::Unreadable),
  }
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
/// In an entry's `ctime`: not the moment the file last changed, but a moment it had not changed
/// after. nv notes this for a file it wrote itself, so that the file need not be looked at again
/// just to learn the first; a change made later than this is a change.
pub(crate) const NOT_AFTER: u64 = 1 << 63;

/// What the index's notes of when files last changed are worth here.
#[derive(Clone, Copy)]
enum Ctimes {
  /// They are about these files.
  Noted,
  /// This is a copy of the workspace they were noted in. Its files all changed, to the file
  /// system, when the copy was made: by this moment. One changed later was changed in the copy.
  CopiedBy(u64),
}

/// Whether a file is, going by what the file system says of it, still what the index has.
/// `ctimes` is whether the index's notes of when files last changed are about these files:
/// they are not in a copy of the workspace, where every file is a new file.
fn unchanged(known: &Indexed, meta: &std::fs::Metadata, ctimes: Ctimes) -> bool {
  let link = known.entry.mode == MODE_LINK;
  let changed = ctime_of(meta);
  let as_noted = match ctimes {
    // The index's own note: the moment itself, or a moment nv had finished writing the file by.
    Ctimes::Noted if known.ctime & NOT_AFTER != 0 => changed <= known.ctime & !NOT_AFTER,
    Ctimes::Noted => known.ctime != 0 && known.ctime == changed,
    // A copy: every file was last changed when it was copied, and not since.
    Ctimes::CopiedBy(when) => changed <= when,
  };
  known.mtime != 0
    && as_noted
    && known.size == meta.len()
    && known.mtime == mtime_of(meta)
    && link == meta.file_type().is_symlink()
    && (link || (known.entry.mode & 0o111 != 0) == (meta.permissions().mode() & 0o111 != 0))
}

/// Files that were read and found to be what the index says, with the size
/// and time they have now: the index can vouch for them from here on.
type Refreshed = Vec<(Vec<u8>, Indexed)>;

/// Compare one path on disk with what the index has for it.
/// What comparing files with the index comes to: how many were looked at and read, the
/// differences, and the index entries to bring up to date.
#[derive(Default)]
struct Compared {
  looked: Looked,
  out: Vec<Difference>,
  refreshed: Refreshed,
  /// For each directory asked about: whether it, or one above it, is a symbolic link.
  links: crate::store::PathMap<bool>,
  /// The directory asked about last, and the answer.
  last_dir: (Vec<u8>, bool),
  /// Where the whole path of the file being compared is put together.
  full: PathBuf,
}

/// Whether a path lies beyond a symbolic link: one of the directories on the way to it is a
/// link, and what is found there is somewhere else's file. Asked once for each directory.
fn beyond_link(root: &Path, path: &[u8], known: &mut crate::store::PathMap<bool>) -> bool {
  let Some(slash) = path.iter().rposition(|&b| b == b'/') else { return false };
  let dir = &path[..slash];
  if let Some(answer) = known.get(dir) {
    return *answer;
  }
  let answer =
    std::fs::symlink_metadata(root.join(to_path(dir))).is_ok_and(|meta| meta.file_type().is_symlink()) || beyond_link(root, dir, known);
  known.insert(dir.to_vec(), answer);
  answer
}

fn compare(ws: &Workspace, path: &[u8], known: Option<&Indexed>, found: &mut Compared, eols: &mut Eols, ctimes: Ctimes) -> Result<()> {
  if known.is_some_and(|k| k.entry.mode == MODE_GITLINK) {
    // A submodule's directory is not ours to track.
    return Ok(());
  }
  // The file's whole path, in a buffer the thread keeps from one file to the next: nothing is
  // allocated for a file that turns out to be as the index has it.
  let mut full = std::mem::take(&mut found.full);
  full.as_mut_os_string().clear();
  full.push(&ws.root);
  full.push(to_path(path));
  let done = compare_at(ws, path, known, found, eols, ctimes, &full);
  found.full = full;
  done
}

fn compare_at(
  ws: &Workspace,
  path: &[u8],
  known: Option<&Indexed>,
  found: &mut Compared,
  eols: &mut Eols,
  ctimes: Ctimes,
  full: &Path,
) -> Result<()> {
  found.looked.stats += 1;
  // Files come a directory at a time: the directory just asked about is the next one's, nearly always.
  let dir = path.iter().rposition(|&b| b == b'/').map_or(&path[..0], |slash| &path[..slash]);
  let beyond = if found.last_dir.0 == dir {
    found.last_dir.1
  } else {
    let answer = beyond_link(&ws.root, path, &mut found.links);
    found.last_dir = (dir.to_vec(), answer);
    answer
  };
  let there = if beyond { Err(std::io::Error::from(std::io::ErrorKind::NotFound)) } else { std::fs::symlink_metadata(full) };
  let meta = match there {
    // A pipe, a socket, a device: nothing a tree holds, and reading one could wait for ever.
    Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => meta,
    // Not allowed to look is not the same as not there: the file is left as it is known.
    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Ok(()),
    _ => {
      if let Some(k) = known {
        found.out.push((path.to_vec(), None, Some(k.entry)));
      }
      return Ok(());
    }
  };
  if let Some(k) = known.filter(|k| unchanged(k, &meta, ctimes)) {
    // When it last changed, by the file system's account, is noted now if only a bound was known.
    if k.ctime != ctime_of(&meta) {
      found.refreshed.push((path.to_vec(), Indexed { ctime: ctime_of(&meta), ..*k }));
    }
    return Ok(());
  }
  found.looked.reads += 1;
  // A link is read as a link, and what it leads to is never opened: a pipe there would wait for ever.
  let opened = if meta.is_file() { std::fs::File::open(full).map(drop) } else { Ok(()) };
  let (mode, mut bytes) = match opened {
    // One file that may not be read is not a reason to record nothing of the others.
    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
      eprintln!("nv: {} may not be read ({e}): it is left out of this look, and of what is recorded", full.display());
      found.looked.unreadable.push(path.to_vec());
      return Ok(());
    }
    _ => read_content(full, &meta)?,
  };
  // As it is first: most files are stored as they are on disk. Then as its rules say it is stored.
  let mut same = known.is_some_and(|k| k.entry.mode == mode && k.entry.id == object_id(Kind::Blob, &bytes));
  if !same
    && mode != MODE_LINK
    && let Some(stored) = crate::eol::to_store(eols.how(path), &bytes)
  {
    bytes = stored;
    same = known.is_some_and(|k| k.entry.mode == mode && k.entry.id == object_id(Kind::Blob, &bytes));
  }
  if let Some(k) = known.filter(|_| same) {
    let now = indexed(full, k.entry);
    if now.mtime != 0 {
      found.refreshed.push((path.to_vec(), now));
    }
    return Ok(());
  }
  found.out.push((path.to_vec(), Some((mode, bytes)), known.map(|k| k.entry)));
  Ok(())
}

struct Walk<'a> {
  ignores: crate::ignore::Ignores,
  /// The directories the walk is inside, outermost first: how long each one's
  /// path is, and whether the rules ignore it. A directory that holds known
  /// files is entered whatever the rules say, so for those the rules are only
  /// asked when a file that is not known turns up.
  inside: Vec<(usize, Option<bool>)>,
  ws: &'a Workspace,
  index: &'a Index,
  names: &'a Names<'a>,
  seen: crate::index::Seen,
  /// The files to compare with the index, found by the walk and looked at after it, several at a time.
  todo: Vec<(Vec<u8>, Option<Indexed>)>,
  /// When set, a directory to go into is noted here and not gone into: for others to walk at the same time.
  later: Option<Vec<Below>>,
}

/// A directory still to be walked, and all a walk needs to know on arriving there.
struct Below {
  dir: PathBuf,
  rel: Vec<u8>,
  inside: Vec<(usize, Option<bool>)>,
  ignores: crate::ignore::Ignores,
}

impl Walk<'_> {
  /// Whether the directory being walked is ignored, or is inside one that is.
  fn here_ignored(&mut self, rel: &[u8]) -> bool {
    let mut ignored = false;
    for at in 0..self.inside.len() {
      let (len, known) = self.inside[at];
      ignored = known.unwrap_or_else(|| ignored || self.ignores.ignored(&rel[..len], true));
      self.inside[at].1 = Some(ignored);
    }
    ignored
  }

  fn dir(&mut self, dir: &Path, rel: &mut Vec<u8>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
      // One directory that may not be looked into is not a reason to look at nothing.
      Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && !rel.is_empty() => {
        eprintln!("nv: {} may not be listed ({e}): what is in it is left out of this look, and of what is recorded", dir.display());
        return Ok(());
      }
      listed => listed.map_err(|e| format!("cannot list {}: {e}", dir.display()))?,
    };
    // The listing says whether the directory has rules of its own: no file is opened to find out.
    let mut listed: Vec<(std::ffi::OsString, bool)> = Vec::new();
    let mut has_rules = false;
    for entry in entries.flatten() {
      let name = entry.file_name();
      // A directory with a repository of its own in it is that repository's, not this one's.
      if name == ".git" && !rel.is_empty() {
        return Ok(());
      }
      // Likewise a directory that is an nv workspace of its own.
      if name == META && !rel.is_empty() && dir.join(META).join("workspace").is_file() {
        return Ok(());
      }
      has_rules |= name == ".gitignore";
      // nv's own directory, and Git's, are the ones at the top: deeper, the names are names.
      if !(rel.is_empty() && SKIPPED.iter().any(|s| name == *s)) {
        listed.push((name, entry.file_type().is_ok_and(|t| t.is_dir())));
      }
    }
    let own_rules = has_rules && self.ignores.enter(rel);
    for (name, is_dir) in listed {
      let len = rel.len();
      if len > 0 {
        rel.push(b'/');
      }
      rel.extend_from_slice(name.as_bytes());
      if in_view(&self.ws.prefixes, rel, is_dir) {
        if !is_dir {
          // A known file is looked at whatever the rules say; they are for what is not known.
          let known = self.index.get_noting(&mut self.seen, rel);
          if known.is_some() {
            self.todo.push((rel.clone(), known));
          } else if !(self.here_ignored(rel) || self.ignores.ignored(rel, false)) {
            // Not known by this name; perhaps by another spelling of it.
            let path = self.names.path_of(rel);
            let known = if path == *rel { None } else { self.index.get_noting(&mut self.seen, &path) };
            self.todo.push((path, known));
          }
        } else if self.index.get(rel).is_none_or(|k| k.entry.mode != MODE_GITLINK) {
          // Asking the index is cheaper than asking the rules, and most directories hold known files.
          let enter = if self.index.has_under(rel) {
            Some(None)
          } else if self.here_ignored(rel) || self.ignores.ignored(rel, true) {
            None
          } else {
            Some(Some(false))
          };
          if let Some(ignored) = enter {
            self.inside.push((rel.len(), ignored));
            match &mut self.later {
              Some(later) => {
                later.push(Below { dir: dir.join(&name), rel: rel.clone(), inside: self.inside.clone(), ignores: self.ignores.clone() })
              }
              None => self.dir(&dir.join(&name), rel)?,
            }
            self.inside.pop();
          }
        }
      }
      rel.truncate(len);
    }
    if own_rules {
      self.ignores.leave();
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
  match look {
    Look::Everywhere => status_at(ws, false, &[Vec::new()]),
    // What the index knows, and besides it what was reported: a file recorded once and since made
    // "not tracked" again (by a look that shows snapshots as changes) is among the reported.
    Look::Tracked => status_at(ws, true, &ws.recorded()),
    Look::Recorded => {
      let index = ws.load_index();
      let mut rules = crate::ignore::Checker::new(&ws.root);
      let mut todo = Vec::new();
      let reported = ws.recorded();
      // A change to the rules of what is left out changes which files there are to record, anywhere
      // below it: no list of reported paths says which, so every file is looked at.
      if reported.iter().any(|path| path.rsplit(|&b| b == b'/').next().is_some_and(|name| name == b".gitignore"))
        || rules_outside_changed(ws)
      {
        return status(ws, Look::Everywhere);
      }
      // A path that was a directory and is now a file takes the files it held with it; a path that
      // was a file and is now a directory, or is now below one that was a file, takes that file.
      let mut also: Vec<Vec<u8>> = Vec::new();
      for path in &reported {
        let now = std::fs::symlink_metadata(ws.root.join(to_path(path)));
        if !now.as_ref().is_ok_and(|meta| meta.is_dir()) {
          // Asked of the index in place: a reported write costs nothing in the size of the tree.
          if index.has_under(path) {
            let below = [path.as_slice(), b"/"].concat();
            also.extend(index.entries().into_iter().map(|(known, _)| known).filter(|known| known.starts_with(&below)));
          }
        } else {
          // Everything in it is new to nv, reported file by file or not.
          let (found, _) = status_at(ws, false, std::slice::from_ref(path))?;
          also.extend(found.into_iter().map(|(found, _, _)| found));
        }
        let mut upto = 0;
        while let Some(slash) = path[upto..].iter().position(|&b| b == b'/') {
          let parent = &path[..upto + slash];
          upto += slash + 1;
          if index.get(parent).is_some() {
            also.push(parent.to_vec());
          }
        }
        if now.is_ok_and(|meta| meta.is_dir()) && index.get(path).is_some() {
          also.push(path.clone());
        }
      }
      let mut reported = reported;
      reported.extend(also);
      reported.sort();
      reported.dedup();
      {
        let names = Names::of(ws, &index);
        for path in reported {
          let ignored = rules.ignored(&path);
          let (path, known) = names.known(path);
          // A reported write to a file the rules ignore is not a file to track, as with a walk.
          if in_view(&ws.prefixes, &path, false) && (known.is_some() || !ignored) {
            todo.push((path, known));
          }
        }
      }
      // Two spellings reported of one file are one path, as the index knows it: looked at once.
      todo.sort_by(|a, b| a.0.cmp(&b.0));
      todo.dedup_by(|a, b| a.0 == b.0);
      if names_fold(ws) {
        let mut seen = std::collections::HashSet::new();
        todo.retain(|(path, _)| seen.insert(Names::fold(path)));
      }
      finish(ws, index, todo)
    }
  }
}

/// The differences among the files the index knows (`tracked`), and among
/// everything at or below each of `under` (the empty path is the whole
/// view). Nothing else is looked at: adding one file does not walk a tree.
pub fn status_at(ws: &Workspace, tracked: bool, under: &[Vec<u8>]) -> Result<(Vec<Difference>, Looked)> {
  let index = ws.load_index();
  let names = Names::of(ws, &index);
  let mut todo: Vec<(Vec<u8>, Option<Indexed>)> =
    if tracked { index.entries().into_iter().map(|(path, known)| (path, Some(known))).collect() } else { Vec::new() };
  for path in under {
    let below = |other: &[u8]| path.is_empty() || (other.len() > path.len() && other.starts_with(path) && other[path.len()] == b'/');
    let full = ws.root.join(to_path(path));
    if !path.is_empty() && !std::fs::symlink_metadata(&full).is_ok_and(|meta| meta.is_dir()) {
      // A file, or nothing: itself if it is or was a file, and what the index had below it if it was a directory.
      // A file the rules ignore is not looked at unless the index has it, as in a walk.
      let (name, known) = names.known(path.clone());
      if known.is_some() || !crate::ignore::is_ignored(&ws.root, path) {
        todo.push((name, known));
      }
      todo.extend(index.entries().into_iter().filter(|(known, _)| below(known)).map(|(path, known)| (path, Some(known))));
      continue;
    }
    // A directory where the index has a file: the file is gone, and that is a difference too.
    if !path.is_empty() && index.get(path).is_some() {
      todo.push((path.clone(), index.get(path)));
    }
    // A directory: walked, with the rules of the directories above it in force.
    let mut ignores = crate::ignore::Ignores::new(&ws.root);
    let ends: Vec<usize> =
      path.iter().enumerate().filter(|(_, b)| **b == b'/').map(|(at, _)| at).chain((!path.is_empty()).then_some(path.len())).collect();
    for above in std::iter::once(0).chain(ends.iter().copied()).take(ends.len()) {
      let dir = &path[..above];
      if ws.root.join(to_path(dir)).join(".gitignore").is_file() {
        ignores.enter(dir);
      }
    }
    let start = Below { dir: full, rel: path.clone(), inside: ends.iter().map(|end| (*end, None)).collect(), ignores };
    let (seen, found) = walk_from(ws, &index, &names, start)?;
    todo.extend(found);
    // What the index has there and the walk did not come across is gone from the disk.
    todo.extend(index.unseen(&seen).into_iter().filter(|(known, _)| below(known)).map(|(path, known)| (path, Some(known))));
  }
  todo.sort_by(|a, b| a.0.cmp(&b.0));
  todo.dedup_by(|a, b| a.0 == b.0);
  finish(ws, index, todo)
}

/// How files are named on a file system that does not tell `A` from `a`,
/// nor a letter with an accent from the letter followed by the accent. A
/// name the index does not have may there be a file it does have, spelt
/// another way; Git on such a file system takes it for that file, and so
/// does this.
/// Whether this workspace's file system takes names that differ in case for one name.
fn names_fold(ws: &Workspace) -> bool {
  ws.root.join(META.to_ascii_uppercase()).join("workspace").is_file()
}

struct Names<'a> {
  index: &'a Index,
  folds: bool,
  known: std::sync::OnceLock<std::collections::HashMap<Vec<u8>, Vec<u8>>>,
}

impl<'a> Names<'a> {
  fn of(ws: &Workspace, index: &'a Index) -> Self {
    // The workspace's own directory, asked for in capitals.
    let folds = ws.root.join(META.to_ascii_uppercase()).join("workspace").is_file();
    Names { index, folds, known: std::sync::OnceLock::new() }
  }

  fn fold(path: &[u8]) -> Vec<u8> {
    path.split(|&b| b == b'/').map(crate::manifest::fold_name).collect::<Vec<_>>().join(&b'/')
  }

  /// The path to know a file by, for one the index does not have under the name it was found
  /// with: the path the index has it under, or the name with its characters composed.
  /// Every path of the index is folded once, the first time this is asked.
  fn path_of(&self, found: &[u8]) -> Vec<u8> {
    if !self.folds {
      return found.to_vec();
    }
    let known = self.known.get_or_init(|| self.index.entries().into_iter().map(|(path, _)| (Self::fold(&path), path)).collect());
    match (known.get(&Self::fold(found)), std::str::from_utf8(found)) {
      (Some(path), _) => path.clone(),
      (None, Ok(text)) if !found.is_ascii() => composed(text).into_bytes(),
      _ => found.to_vec(),
    }
  }

  /// A path someone named, and what the index has for it.
  fn known(&self, path: Vec<u8>) -> (Vec<u8>, Option<Indexed>) {
    match self.index.get(&path) {
      Some(known) => (path, Some(known)),
      None => {
        let path = self.path_of(&path);
        let known = self.index.get(&path);
        (path, known)
      }
    }
  }
}

type Found = (crate::index::Seen, Vec<(Vec<u8>, Option<Indexed>)>);

/// Walk a directory and everything below it. The top two levels are listed
/// first, and the directories found there are then walked several at a
/// time: listing a directory is a question to the operating system, and a
/// tree is thousands of them.
fn walk_from(ws: &Workspace, index: &Index, names: &Names, start: Below) -> Result<Found> {
  let walk_one = |below: Below, split: bool| -> Result<(Found, Vec<Below>)> {
    let Below { dir, mut rel, inside, ignores } = below;
    let mut walk = Walk { ignores, inside, ws, index, names, seen: index.seen(), todo: Vec::new(), later: split.then(Vec::new) };
    walk.dir(&dir, &mut rel)?;
    Ok(((walk.seen, walk.todo), walk.later.unwrap_or_default()))
  };
  let (mut found, first) = walk_one(start, true)?;
  let take = |found: &mut Found, (seen, todo): Found| {
    found.0.merge(seen);
    found.1.extend(todo);
  };
  let mut second = Vec::new();
  for below in first {
    let (part, later) = walk_one(below, true)?;
    take(&mut found, part);
    second.extend(later);
  }
  let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8).min(second.len());
  if threads <= 1 {
    for below in second {
      take(&mut found, walk_one(below, false)?.0);
    }
    return Ok(found);
  }
  let queue = std::sync::Mutex::new(second);
  let parts: Vec<Result<Vec<Found>>> = std::thread::scope(|scope| {
    let running: Vec<_> = (0..threads)
      .map(|_| {
        scope.spawn(|| {
          let mut mine = Vec::new();
          loop {
            let Some(below) = queue.lock().map_err(|_| "a thread walking the tree stopped".to_string())?.pop() else { return Ok(mine) };
            mine.push(walk_one(below, false)?.0);
          }
        })
      })
      .collect();
    running.into_iter().map(|thread| thread.join().unwrap_or_else(|_| Err("a thread walking the tree stopped".to_string()))).collect()
  });
  for part in parts {
    for one in part? {
      take(&mut found, one);
    }
  }
  Ok(found)
}

/// Compare each of `todo` with the index, on as many threads as it is worth:
/// nearly all of the time is the operating system answering about one file.
/// Which directory the index's notes of when files last changed are about: the
/// workspace's top, by its number in the file system. A copy of a workspace is
/// other files, each changed (to the file system) at the moment it was copied.
fn noted_for(ws: &Workspace) -> (PathBuf, String) {
  noted_at(&ws.root)
}

fn noted_at(root: &Path) -> (PathBuf, String) {
  use std::os::unix::fs::MetadataExt;
  let here = std::fs::metadata(root).map(|meta| format!("{} {}", meta.dev(), meta.ino())).unwrap_or_default();
  (root.join(META).join("index.root"), here)
}

/// Say that the index just written is about the files of this directory: nv wrote them all.
fn note_here(root: &Path) -> Result<()> {
  let (noted, here) = noted_at(root);
  crate::write_whole(&noted, here)
}

fn finish(ws: &Workspace, mut index: Index, todo: Vec<(Vec<u8>, Option<Indexed>)>) -> Result<(Vec<Difference>, Looked)> {
  let (noted, here) = noted_for(ws);
  let same_place = std::fs::read_to_string(&noted).is_ok_and(|was| was == here);
  // In a copy, the note itself was copied with the files: when it changed is when the copy was made.
  // With no note at all nothing is known, and every file is read once.
  let ctimes = match same_place {
    true => Ctimes::Noted,
    false => Ctimes::CopiedBy(std::fs::symlink_metadata(&noted).map_or(0, |meta| ctime_of(&meta))),
  };
  type Part = Result<(Looked, Vec<Difference>, Refreshed)>;
  let part = |todo: &[(Vec<u8>, Option<Indexed>)]| -> Part {
    let mut found = Compared::default();
    let mut eols = Eols::of_worktree(&ws.root);
    for (path, known) in todo {
      compare(ws, path, known.as_ref(), &mut found, &mut eols, ctimes)?;
    }
    Ok((found.looked, found.out, found.refreshed))
  };
  let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8).min(todo.len() / 256 + 1);
  let parts: Vec<Part> = if threads <= 1 {
    vec![part(&todo)]
  } else {
    let size = todo.len().div_ceil(threads);
    std::thread::scope(|scope| {
      let running: Vec<_> = todo.chunks(size).map(|chunk| scope.spawn(move || part(chunk))).collect();
      running.into_iter().map(|thread| thread.join().unwrap_or_else(|_| Err("a thread looking at files stopped".to_string()))).collect()
    })
  };
  let (mut looked, mut out, mut refreshed) = (Looked::default(), Vec::new(), Refreshed::new());
  for part in parts {
    let (l, o, r) = part?;
    looked.stats += l.stats;
    looked.reads += l.reads;
    looked.unreadable.extend(l.unreadable);
    out.extend(o);
    refreshed.extend(r);
  }
  out.sort_by(|a, b| a.0.cmp(&b.0));
  // A file that is not there because Git was told to keep it out of the tree is not a file removed.
  if out.iter().any(|(_, new, _)| new.is_none())
    && let Some(kept_out) = crate::colocated::kept_out(&ws.root)
  {
    out.retain(|(path, new, _)| new.is_some() || !kept_out.contains(path));
  }
  if !refreshed.is_empty() {
    for (path, now) in refreshed {
      index.insert(path, now);
    }
    index.save()?;
  }
  if !same_place {
    crate::write_whole(&noted, here)?;
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
/// The note a sync leaves while it writes the ref's files into the workspace.
pub(crate) fn arriving(ws: &Workspace) -> PathBuf {
  ws.root.join(META).join("arriving")
}

/// The note a push leaves from before it is sent until its answer is heard: how many commits it
/// carried, and the last of them. A push killed or timed out may have landed all the same, and
/// until the workspace knows, nothing may go back past it or make its commits again.
pub(crate) fn unheard(ws: &Workspace) -> PathBuf {
  ws.root.join(META).join("pushing")
}

/// The commits a push carried that the workspace has not heard about: `(how many, the last)`.
pub fn unheard_push(ws: &Workspace) -> Option<(usize, Id)> {
  let text = std::fs::read_to_string(unheard(ws)).ok()?;
  let mut fields = text.split_whitespace();
  let count = fields.next()?.parse().ok()?;
  let last = fields.next().and_then(unhex).and_then(|bytes| Id::try_from(bytes).ok())?;
  Some((count, last))
}

/// Refused while a push of this workspace's may have landed unheard.
pub fn not_unheard(ws: &Workspace, what: &str) -> Result<()> {
  match unheard_push(ws) {
    None => Ok(()),
    Some(_) => Err(format!(
      "{what} is refused: the last push from this workspace was not heard back from, and may have landed. `nv sync <dir>` finds out where the workspace stands, or `nv push <dir>` again. Nothing was touched."
    )),
  }
}

/// A sync that was killed while writing left files that are half the ref's and half the
/// workspace's. Recorded as they are, they would be taken for the workspace's own changes.
pub fn interrupted(ws: &Workspace) -> Result<()> {
  if arriving(ws).exists() {
    return Err(
      "a sync was cut short while it was writing files here: `nv sync <dir>` finishes it, and nothing is recorded until then".to_string(),
    );
  }
  Ok(())
}

/// The rules of what is left out that are not files of the tree, and so are never reported: the
/// workspace's `.nv/exclude`, and Git's `info/exclude` where Git shares the directory. Their
/// bytes, hashed: a copy of the workspace has the same.
fn rules_outside(ws: &Workspace) -> String {
  let mut hasher = blake3::Hasher::new();
  for file in [ws.root.join(META).join("exclude"), ws.root.join(".git").join("info").join("exclude")] {
    match std::fs::read(file) {
      Ok(bytes) => hasher.update(&(bytes.len() as u64).to_le_bytes()).update(&bytes),
      Err(_) => hasher.update(b"-"),
    };
  }
  hasher.finalize().to_hex().to_string()
}

fn rules_seen(ws: &Workspace) -> PathBuf {
  ws.root.join(META).join("exclude.seen")
}

fn note_rules(ws: &Workspace, rules: Option<String>) {
  if let Some(rules) = rules.filter(|rules| std::fs::read_to_string(rules_seen(ws)).map_or(true, |seen| seen.trim() != rules)) {
    let _ = crate::write_whole(&rules_seen(ws), rules);
  }
}

/// Whether those rules changed since the last look at every file. Nothing noted yet is nothing
/// changed: the first snapshot notes them.
fn rules_outside_changed(ws: &Workspace) -> bool {
  std::fs::read_to_string(rules_seen(ws)).is_ok_and(|seen| seen.trim() != rules_outside(ws))
}

pub fn snapshot(store: &mut Store, ws: &mut Workspace, author: &str, look: Look) -> Result<Option<Id>> {
  snapshot_looking(store, ws, author, look).map(|(id, _)| id)
}

/// `snapshot`, saying also what its look found: which files may not be read, among others.
fn snapshot_looking(store: &mut Store, ws: &mut Workspace, author: &str, look: Look) -> Result<(Option<Id>, Looked)> {
  interrupted(ws)?;
  // The rules outside the tree as this look finds them, noted once what it found is recorded, when
  // it looked at every file (or there is nothing noted yet).
  let rules = (look == Look::Everywhere || rules_outside_changed(ws) || !rules_seen(ws).exists()).then(|| rules_outside(ws));
  let (found, looked) = status(ws, look)?;
  // What was reported is forgotten only once it is recorded: a kill before that leaves the
  // report, and the paths are looked at again.
  let journal = ws.root.join(META).join("journal");
  if found.is_empty() {
    let _ = std::fs::remove_file(journal);
    note_rules(ws, rules);
    return Ok((None, looked));
  }
  let mut index = ws.load_index();
  let mut changes: Vec<Change> = Vec::with_capacity(found.len());
  for (path, new, old) in found {
    let entry = match new {
      Some((mode, bytes)) => {
        let id = store.put(Kind::Blob, &bytes, old.map(|o| o.id)).map_err(|e| format!("{}: {e}", String::from_utf8_lossy(&path)))?;
        Some(Entry { mode, id })
      }
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
  ws.log("snapshot", &id)?;
  ws.head = Some(id);
  ws.save()?;
  index.save()?;
  let _ = std::fs::remove_file(journal);
  note_rules(ws, rules);
  Ok((Some(id), looked))
}

/// Publish: one commit on the last published one, holding everything the
/// snapshots since then changed. Takes a snapshot first if the disk has
/// moved on. `None` when there is nothing to publish.
pub fn commit(store: &mut Store, ws: &mut Workspace, author: &str, message: &str, look: Look) -> Result<Option<Id>> {
  commit_saying(store, ws, author, rest_of(author, message), look)
}

/// `commit`, with the author, committer and message as the bytes a Git commit carries them in.
pub fn commit_saying(store: &mut Store, ws: &mut Workspace, author: &str, rest: Vec<u8>, look: Look) -> Result<Option<Id>> {
  snapshot(store, ws, author, look)?;
  if ws.head == ws.base {
    return Ok(None);
  }
  refuse_sync_markers(store, ws)?;
  let from = match ws.base {
    Some(base) => root_of(store, &base)?,
    None => None,
  };
  let to = match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  };
  // A directory a snapshot put there whole, and that still is that directory, is published as one change.
  let whole = whole_since(store, ws.head, ws.base)?;
  let changes = crate::manifest::diff_keeping(store, from, to, &whole)?;
  if changes.is_empty() {
    // The snapshots undid each other.
    ws.head = ws.base;
    ws.save()?;
    return Ok(None);
  }
  let rest = with_settled(store, ws, rest, &changes)?;
  let id = Commit { parents: ws.base.into_iter().collect(), rest, changes }.store(store)?;
  root_of(store, &id)?;
  store.flush()?;
  ws.log("commit", &id)?;
  (ws.base, ws.head) = (Some(id), Some(id));
  ws.save()?;
  Ok(Some(id))
}

/// Who a commit made here is by, and when, as Git would say it: the
/// environment first, then the workspace's own `user.name` and `user.email`,
/// then the user's Git configuration; the time now, in the zone this
/// machine is in, unless the environment gives one.
pub fn signature(ws: &Workspace, role: &str) -> Result<String> {
  let config = std::fs::read_to_string(ws.root.join(".nv").join("git-config")).unwrap_or_default();
  let own =
    |key: &str| config.lines().rev().find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('='))).map(str::to_string);
  let from_env = |key: &str| std::env::var(format!("GIT_{role}_{key}")).ok().filter(|v| !v.is_empty());
  let name = from_env("NAME").or_else(|| own("user.name")).or_else(|| crate::git::config_value(&ws.root, "user", "name"));
  let email = from_env("EMAIL").or_else(|| own("user.email")).or_else(|| crate::git::config_value(&ws.root, "user", "email"));
  // A commit is published under somebody's name. Git refuses to make one up, and so does this.
  let (Some(name), Some(email)) = (name, email) else {
    return Err(format!(
      "nobody to sign the commit as: set `user.name` and `user.email` in your Git configuration, or for this workspace with `nv git config user.name <name>` and `nv git config user.email <address>`, or in GIT_{role}_NAME and GIT_{role}_EMAIL"
    ));
  };
  let when = match from_env("DATE") {
    Some(date) => crate::git::stamp(&date)
      .ok_or_else(|| format!("GIT_{role}_DATE={date} is not a date nv reads: seconds and a zone (`1700000000 +0900`), or ISO 8601"))?,
    None => {
      let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
      // SAFETY: `localtime_r` writes into the struct it is handed and nothing else.
      let east = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let at: libc::time_t = now as libc::time_t;
        libc::localtime_r(&at, &mut tm);
        tm.tm_gmtoff
      };
      format!("{now} {}{:02}{:02}", if east < 0 { '-' } else { '+' }, east.abs() / 3600, east.abs() % 3600 / 60)
    }
  };
  Ok(format!("{name} <{email}> {when}"))
}

/// Publish a state recorded since the last commit, though the files have moved on: one commit on
/// the last, of what that state changed. What was recorded after it stays recorded, on top of the
/// new commit, as changes not committed. `None` when the state is the last commit's files.
///
/// For a writer that does not commit: what its conditions passed on is committed as it was, while
/// it goes on writing. Not beside Git, where a commit is made by Git, from the files.
pub fn commit_state(store: &mut Store, ws: &mut Workspace, state: Id, rest: Vec<u8>, author: &str) -> Result<Option<Id>> {
  if crate::colocated::shared(&ws.root) {
    return Err("committing a state the files have moved on from is not done beside Git, where Git commits the files".to_string());
  }
  // A state of this workspace's since its last commit: on the way back from where it is now.
  let mut at = ws.head;
  let mut found = false;
  while let Some(id) = at.filter(|id| Some(*id) != ws.base) {
    if id == state {
      found = true;
      break;
    }
    at = Commit::load(store, &id)?.parents.first().copied();
  }
  if !found {
    if Some(state) == ws.base {
      return Ok(None);
    }
    return Err(format!("{} is not a state recorded here since the last commit", &hex(&state)[..12]));
  }
  refuse_sync_markers_in(store, ws, Some(state))?;
  let tree = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  let (from, to, now) = (tree(store, ws.base)?, tree(store, Some(state))?, tree(store, ws.head)?);
  let whole = whole_since(store, Some(state), ws.base)?;
  let changes = crate::manifest::diff_keeping(store, from, to, &whole)?;
  if changes.is_empty() {
    return Ok(None);
  }
  // The settling of files a sync left out of this workspace's own commit completes that commit,
  // under its message: not a commit of its own after one that falls short of what it says.
  let id = match incomplete(store, ws)? {
    Some((short, paths)) if changes.iter().all(|(path, _)| paths.contains(path)) => {
      let was = Commit::load(store, &short)?;
      let mut whole = was.changes.clone();
      for (path, entry) in changes {
        match whole.iter_mut().find(|(p, _)| *p == path) {
          Some(change) => change.1 = entry,
          None => whole.push((path, entry)),
        }
      }
      whole.sort_by(|a, b| a.0.cmp(&b.0));
      let _ = std::fs::remove_file(incomplete_file(ws));
      let rest = with_settled(store, ws, was.rest.clone(), &whole)?;
      let mut made = Commit { parents: was.parents.clone(), rest, changes: whole }.store(store)?;
      root_of(store, &made)?;
      // Those after it, made again on top as they were: none of them changed what was settled.
      let after: Vec<Commit> = {
        let chain = pending(store, ws)?;
        let at = chain.iter().position(|(id, _)| *id == short).map_or(chain.len(), |i| i + 1);
        chain[at..].iter().map(|(_, c)| c.clone()).collect()
      };
      let last_was = ws.base;
      for later in after {
        made = Commit { parents: vec![made], rest: later.rest, changes: later.changes }.store(store)?;
        root_of(store, &made)?;
      }
      // The settling does not answer for the message: what was said of its base still stands.
      if let Some((_, under)) = moved_under(ws).filter(|(id, _)| Some(*id) == last_was || *id == short) {
        set_moved(ws, Some(made), &under)?;
      }
      made
    }
    _ => {
      let rest = with_settled(store, ws, rest, &changes)?;
      Commit { parents: ws.base.into_iter().collect(), rest, changes }.store(store)?
    }
  };
  root_of(store, &id)?;
  // What came after the state, recorded again on the new commit.
  let head = if Some(state) == ws.head {
    id
  } else {
    let whole = whole_since(store, ws.head, Some(state))?;
    let later = crate::manifest::diff_keeping(store, to, now, &whole)?;
    if later.is_empty() { id } else { carry(store, id, later, author)? }
  };
  store.flush()?;
  ws.log("commit", &id)?;
  if head != id {
    ws.log("snapshot", &head)?;
  }
  let based = ws.base;
  (ws.base, ws.head) = (Some(id), Some(head));
  ws.save()?;
  take_branch_along(store, ws, based)?;
  Ok(Some(id))
}

/// The commits published here and not pushed, oldest first: since the ref's tip as this workspace
/// last saw it, or, for one that never saw it, since the commit it was made at.
pub fn pending(store: &mut Store, ws: &Workspace) -> Result<Vec<(Id, Commit)>> {
  let until = ws.pushed.or(ws.start);
  let mut chain = Vec::new();
  let mut at = ws.base;
  while let Some(id) = at.filter(|id| Some(*id) != until) {
    let commit = Commit::load(store, &id)?;
    at = commit.parents.first().copied();
    chain.push((id, commit));
    if chain.len() > 100_000 {
      return Err("more than 100,000 commits since the ref: this workspace does not stand on it".to_string());
    }
  }
  chain.reverse();
  Ok(chain)
}

/// Make the commits not pushed again, as fewer: one new commit for each `(to, rest)`, of all that
/// the commits up to and including `to` changed since the one before. Where each `to` is a
/// checkpoint, each new commit is a state the conditions passed on. Commits after the last `to`
/// are made again as they were, on top; what is recorded and not committed stays so. The files
/// are not touched: the last commit has the same files as before.
pub fn rebuild(store: &mut Store, ws: &mut Workspace, plan: &[(Id, Vec<u8>)], author: &str) -> Result<Vec<Id>> {
  not_unheard(ws, "rebuilding the commits not pushed")?;
  if crate::colocated::shared(&ws.root) {
    return Err("rebuilding commits is not done beside Git: there, Git's own commands do it".to_string());
  }
  // What is not pushed has to have an end: the ref as last seen, or the commit the workspace was
  // made at. Without either it is the whole history, and nothing is made again of that.
  if ws.pushed.or(ws.start).is_none() {
    return Err(
      "this workspace has no ref it pushes to (`nv remote`), nor a commit it was made at: every commit it has would be made again. Nothing was changed".to_string(),
    );
  }
  let chain = pending(store, ws)?;
  if chain.is_empty() {
    return Err("there are no commits here that are not pushed".to_string());
  }
  if let Some((id, _)) = chain.iter().find(|(_, c)| c.parents.len() > 1) {
    return Err(format!("{} is a merge: a ref holds one line of commits, and so does a rebuild", &hex(id)[..12]));
  }
  // Each `to` among them, in order.
  let mut ends = Vec::new();
  let mut from = 0;
  for (to, _) in plan {
    let Some(at) = chain[from..].iter().position(|(id, _)| id == to) else {
      return Err(if chain.iter().any(|(id, _)| id == to) {
        format!("{} comes before the commit named before it: name them oldest first", &hex(to)[..12])
      } else {
        format!("{} is not one of the commits here that are not pushed (`nv pending` lists them)", &hex(to)[..12])
      });
    };
    ends.push(from + at);
    from += at + 1;
  }
  let tree = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  let bottom = chain[0].1.parents.first().copied();
  let mut on = bottom;
  let mut made = Vec::new();
  let next = |store: &mut Store, on: Option<Id>, upto: Id, rest: Vec<u8>| -> Result<Option<Id>> {
    let whole = whole_since(store, Some(upto), bottom)?;
    let (a, b) = (tree(store, on)?, tree(store, Some(upto))?);
    let changes = crate::manifest::diff_keeping(store, a, b, &whole)?;
    if changes.is_empty() {
      return Ok(None);
    }
    let id = Commit { parents: on.into_iter().collect(), rest, changes }.store(store)?;
    root_of(store, &id)?;
    Ok(Some(id))
  };
  let mut start = 0;
  for ((_, rest), end) in plan.iter().zip(&ends) {
    // What the commits it is made of said they settled, it says too.
    let settled: Vec<String> = chain[start..=*end].iter().flat_map(|(_, c)| settled_lines(&c.rest)).collect();
    start = end + 1;
    let rest = with_trailers(rest.clone(), &settled);
    if let Some(id) = next(store, on, chain[*end].0, rest)? {
      made.push(id);
      on = Some(id);
    }
  }
  // After the last one named, the commits as they were.
  for (id, commit) in &chain[ends.last().map_or(0, |e| e + 1)..] {
    if let Some(again) = next(store, on, *id, commit.rest.clone())? {
      made.push(again);
      on = Some(again);
    }
  }
  if tree(store, on)? != tree(store, ws.base)? {
    return Err("the commits made again do not come to the same files: nothing was changed".to_string());
  }
  let head = if ws.head == ws.base {
    on
  } else {
    let whole = whole_since(store, ws.head, ws.base)?;
    let (a, b) = (tree(store, ws.base)?, tree(store, ws.head)?);
    let later = crate::manifest::diff_keeping(store, a, b, &whole)?;
    match (on, later.is_empty()) {
      (Some(on), false) => Some(carry(store, on, later, author)?),
      _ => on,
    }
  };
  store.flush()?;
  for id in &made {
    ws.log("commit", id)?;
  }
  if head != on
    && let Some(head) = head
  {
    ws.log("snapshot", &head)?;
  }
  let based = ws.base;
  (ws.base, ws.head) = (on, head);
  ws.save()?;
  take_branch_along(store, ws, based)?;
  // Messages written again are written on the base the commits now stand on.
  set_moved(ws, None, &[])?;
  Ok(made)
}

/// The directories set whole by the commits from `from` back to, and not
/// including, `until`, along first parents.
pub(crate) fn whole_since(store: &mut Store, from: Option<Id>, until: Option<Id>) -> Result<std::collections::HashSet<Id>> {
  let mut whole = std::collections::HashSet::new();
  let mut at = from;
  while let Some(id) = at.filter(|id| Some(*id) != until) {
    let commit = Commit::load(store, &id)?;
    whole.extend(crate::manifest::whole_dirs(&commit.changes));
    at = commit.parents.first().copied();
  }
  Ok(whole)
}

/// What is on disk outside the directories this workspace is a view of:
/// files someone wrote where the workspace does not look. They are in no
/// snapshot and no commit, so a command that records says that they are
/// there. Only the directories on the way to the view are listed, so this
/// costs a few listings whatever the size of the tree.
pub fn outside_view(ws: &Workspace) -> Vec<String> {
  if ws.prefixes.is_empty() {
    return Vec::new();
  }
  let mut found = Vec::new();
  let mut ignores = crate::ignore::Ignores::new(&ws.root);
  let mut todo: Vec<Vec<u8>> = vec![Vec::new()];
  while let Some(dir) = todo.pop() {
    let Ok(entries) = std::fs::read_dir(ws.root.join(to_path(&dir))) else { continue };
    ignores.enter(&dir);
    for entry in entries.flatten() {
      let name = entry.file_name();
      if dir.is_empty() && SKIPPED.iter().any(|skipped| name == *skipped) {
        continue;
      }
      let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
      let path = if dir.is_empty() { name.as_bytes().to_vec() } else { [dir.as_slice(), b"/", name.as_bytes()].concat() };
      let covered =
        |prefix: &Vec<u8>| path == *prefix || (path.len() > prefix.len() && path.starts_with(prefix) && path[prefix.len()] == b'/');
      if ws.prefixes.iter().any(covered) {
        continue;
      }
      // On the way to a directory of the view: look inside it too.
      if is_dir && in_view(&ws.prefixes, &path, true) {
        todo.push(path);
      } else if !ignores.ignored(&path, is_dir) {
        found.push(String::from_utf8_lossy(&path).into_owned() + if is_dir { "/" } else { "" });
      }
    }
  }
  found.sort();
  found
}

/// Hold the workspace for one writer at a time, until the returned file is
/// dropped. A hook, a watcher and a person's command can all arrive at once,
/// and each reads the workspace's state, changes it and writes it back.
pub fn lock(root: &Path) -> Result<std::fs::File> {
  let path = root.join(META).join("lock");
  let file = std::fs::File::options().create(true).append(true).open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
  crate::limits::lock(&file, &format!("the workspace {}", root.display()))?;
  Ok(file)
}

/// Report what the operating system said was written. A file is reported
/// as it is; a directory that appeared, with what is in it; a path that is
/// gone, with every known file that was under it: a directory renamed or
/// removed is one event, and says nothing of its files. Returns how many
/// paths were reported.
pub fn record_events(ws: &Workspace, events: &std::collections::BTreeSet<Vec<u8>>) -> Result<usize> {
  fn files_under(dir: &Path, rel: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
      let len = rel.len();
      rel.push(b'/');
      rel.extend_from_slice(entry.file_name().as_bytes());
      if entry.file_type().is_ok_and(|t| t.is_dir()) {
        files_under(&entry.path(), rel, out);
      } else {
        out.push(rel.clone());
      }
      rel.truncate(len);
    }
  }
  let index = ws.load_index();
  let mut rules = crate::ignore::Checker::new(&ws.root);
  let mut reported = Vec::new();
  for path in events {
    let full = ws.root.join(to_path(path));
    match std::fs::symlink_metadata(&full) {
      Ok(meta) if meta.is_dir() => {
        // A directory where a file was: the file is gone, whatever the directory holds.
        if index.get(path).is_some() {
          reported.push(path.clone());
        }
        // A directory made where one was (moved away, and another made): what the index has below
        // it and the disk does not is gone, though no event says so.
        if index.has_under(path) {
          let below = [path.as_slice(), b"/"].concat();
          for (known, _) in index.entries().into_iter().filter(|(known, _)| known.starts_with(&below)) {
            if std::fs::symlink_metadata(ws.root.join(to_path(&known))).is_err() {
              reported.push(known);
            }
          }
        }
        // Build output arrives as directories by the hundred; they are not walked.
        if !rules.ignored(&[path.as_slice(), b"/."].concat()) || index.has_under(path) {
          files_under(&full, &mut path.clone(), &mut reported);
        }
      }
      Ok(_) => reported.push(path.clone()),
      Err(_) => {
        reported.push(path.clone());
        if index.has_under(path) {
          let below = |known: &[u8]| known.len() > path.len() && known.starts_with(path) && known[path.len()] == b'/';
          reported.extend(index.entries().into_iter().map(|(known, _)| known).filter(|known| below(known)));
        }
      }
    }
  }
  for path in &reported {
    ws.record(path)?;
  }
  Ok(reported.len())
}

/// What going back to a recorded state did.
#[derive(Debug)]
pub struct Restored {
  /// The snapshot taken of what was on disk and not yet recorded, if there was any such thing.
  pub kept: Option<Id>,
  pub written: usize,
  pub removed: usize,
}

/// Make the files what they were at `target`, a snapshot or a commit.
///
/// What is on disk is recorded first, so going back loses nothing: the state
/// left behind has an id and can be gone back to in turn. Only the files that
/// differ between the two states are touched, and what is published is not:
/// the next commit holds whatever lies between the last one and the files.
///
/// With `Look::Recorded` the tree is not walked: the writer is trusted to
/// have reported every write, and besides the reported paths only the files
/// about to be overwritten are looked at. A change nobody reported, to a
/// file the two states agree on, then stays as it is on disk.
pub fn restore_to(store: &mut Store, ws: &mut Workspace, target: Id, author: &str, look: Look) -> Result<Restored> {
  // A state is somewhere to go back to when it stands on where the workspace is, or on a commit
  // this workspace made itself and has since built on. A state that stands on anything else (what
  // the ref had before a sync, what Git had before a pull, a commit the workspace was reset away
  // from) does not have what came in between, and its files here would be those changes undone:
  // the next commit would publish the undoing of somebody else's work.
  if let Some(base) = ws.base {
    let logged = ws.operations();
    let made: std::collections::HashSet<&str> =
      logged.iter().filter(|(_, what, _)| what == "commit").map(|(_, _, id)| id.as_str()).collect();
    // Where the workspace is, and back from there through the commits it made itself.
    let mut stands_on = std::collections::HashSet::from([base]);
    let mut at = base;
    while made.contains(hex(&at).as_str()) {
      let Some(parent) = Commit::load(store, &at)?.parents.first().copied() else { break };
      stands_on.insert(parent);
      at = parent;
    }
    // The state has to stand on one of those: be it, or have it behind it. (A state that has more
    // behind it than the workspace now does, because the workspace was reset to an older commit,
    // is still somewhere to go: its files bring back, as changes not yet committed, what the reset
    // took away, as checking its files out with Git would.)
    let own: std::collections::HashSet<&str> = logged.iter().map(|(_, _, id)| id.as_str()).collect();
    let mut at = Some(target);
    let mut within = false;
    let mut all_own = true;
    for _ in 0..65536 {
      let Some(id) = at else {
        // Down to a state with nothing before it, all of them recorded here: made before the
        // first commit, on nothing anyone else made.
        within = all_own;
        break;
      };
      if stands_on.contains(&id) {
        within = true;
        break;
      }
      all_own &= own.contains(hex(&id).as_str());
      at = Commit::load(store, &id)?.parents.first().copied();
    }
    if !within {
      return Err(if crate::colocated::shared(&ws.root) {
        "that state is from before Git moved this directory to other commits (a pull, a merge, another branch, a commit made with Git). Going back to its files would undo, in the next commit, what those commits did.\nTo see what that state had: nv since <dir> <state>\nTo go back to it and what it stood on, move Git there first (git switch, git reset), then restore."
          .to_string()
      } else {
        "that state is from before this workspace last caught up with its ref, or stands on commits the workspace is no longer on. Going back to its files would undo, in the next commit, what others pushed since.\nTo undo the last catching-up itself, files and all: nv restore <dir> before-sync\nTo see what that state had: nv since <dir> <state>"
          .to_string()
      });
    }
  }
  restore_across(store, ws, target, author, look)
}

/// `restore_to` without asking whether the state knows of what the ref has.
fn restore_across(store: &mut Store, ws: &mut Workspace, target: Id, author: &str, look: Look) -> Result<Restored> {
  let before = ws.head;
  if look == Look::Recorded {
    let root = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
    let (from, to) = (root(store, ws.head)?, root(store, Some(target))?);
    let differing: Vec<Vec<u8>> = crate::manifest::diff(store, from, to)?.into_iter().map(|(path, _)| path).collect();
    ws.note(&differing)?;
  }
  let (_, looked) = snapshot_looking(store, ws, author, look)?;
  let kept = ws.head.filter(|head| Some(*head) != before);
  let from = match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  };
  let to = root_of(store, &target)?;
  let mut done = Restored { kept, written: 0, removed: 0 };
  let mut stuck: Vec<String> = Vec::new();
  let mut index = ws.load_index();
  let mut placed = Placed::default();
  let mut eols = Eols::of_worktree(&ws.root);
  let changes: Vec<(Vec<u8>, Option<Entry>)> =
    crate::manifest::diff(store, from, to)?.into_iter().filter(|(path, _)| in_view(&ws.prefixes, path, false)).collect();
  // Before anything is written: a directory where a file must go, holding files nv does not track.
  let mut blocking = in_the_way(&ws.root, &changes);
  let mut unlistable: std::collections::HashMap<PathBuf, bool> = Default::default();
  for (path, entry) in &changes {
    let full = ws.root.join(to_path(path));
    // A link on the way that the state left does not have as that link was never recorded:
    // making a directory of it would lose where it led.
    if let Some(link) = link_on_the_way(&ws.root, path) {
      let rel = link.strip_prefix(&ws.root).unwrap_or(&link).as_os_str().as_bytes().to_vec();
      let recorded = match from {
        Some(root) => crate::manifest::lookup(store, root, &rel)?,
        None => None,
      };
      let same = recorded.is_some_and(|entry| {
        std::fs::symlink_metadata(&link).is_ok_and(|meta| {
          read_content(&link, &meta).is_ok_and(|(mode, bytes)| mode == entry.mode && object_id(Kind::Blob, &bytes) == entry.id)
        })
      });
      if !same {
        blocking.push(String::from_utf8_lossy(&rel).into_owned());
      }
      continue;
    }
    // A directory that may not be listed or written in, on the way to the file: found now, not
    // half-way through. The nearest one there is the one the file, or the directories it needs,
    // are made in; any above it has to be one that can be gone through.
    let blocked_before = blocking.len();
    let mut on_the_way = full.parent();
    while let Some(dir) = on_the_way.filter(|dir| dir.starts_with(&ws.root) && *dir != ws.root.as_path()) {
      if let Ok(meta) = std::fs::symlink_metadata(dir) {
        // A file or a link standing there is the other checks' to judge: only a directory is gone into.
        if !meta.is_dir() {
          break;
        }
        let closed = *unlistable.entry(dir.to_path_buf()).or_insert_with(|| {
          use std::os::unix::fs::PermissionsExt;
          std::fs::read_dir(dir).is_err() || std::fs::metadata(dir).is_ok_and(|meta| meta.permissions().mode() & 0o200 == 0)
        });
        if closed {
          blocking.push(dir.strip_prefix(&ws.root).unwrap_or(dir).display().to_string());
        }
        break;
      }
      on_the_way = dir.parent();
    }
    if blocking.len() > blocked_before {
      continue;
    }
    let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
    // A directory where something else has to be: emptied first, so each directory in it has to
    // be one things can be removed from.
    if meta.is_dir() && entry.is_some_and(|e| !e.is_dir()) {
      use std::os::unix::fs::PermissionsExt;
      let mut todo = vec![full.clone()];
      while let Some(dir) = todo.pop() {
        let writable = std::fs::metadata(&dir).is_ok_and(|meta| meta.permissions().mode() & 0o300 == 0o300);
        match std::fs::read_dir(&dir) {
          Ok(listing) if writable => todo.extend(listing.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path())),
          _ => {
            blocking.push(dir.strip_prefix(&ws.root).unwrap_or(&dir).display().to_string());
            break;
          }
        }
      }
      continue;
    }
    // A pipe, a socket, a device: nothing nv tracks, so nothing it may remove.
    if !meta.is_file() && !meta.file_type().is_symlink() {
      blocking.push(String::from_utf8_lossy(path).into_owned());
      continue;
    }
    // A file the state left has nothing at this path of: nv did not record it (the rules leave it
    // out), so writing over it would lose it. A file that may not be read was not recorded either.
    let known = match from {
      Some(root) => crate::manifest::lookup(store, root, path)?.is_some(),
      None => false,
    };
    let unreadable = meta.is_file() && std::fs::File::open(&full).is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
    // An untracked file that is, byte for byte, what would be written is not lost by writing it.
    let same = !unreadable
      && entry.is_some_and(|entry| {
        read_content(&full, &meta).is_ok_and(|(mode, bytes)| mode == entry.mode && object_id(Kind::Blob, &bytes) == entry.id)
      });
    if (entry.is_some() && !known && !same) || unreadable {
      blocking.push(String::from_utf8_lossy(path).into_owned());
    }
  }
  // And a file where a directory has to be made for one of them.
  blocking.extend(
    untracked_in_the_way(store, &ws.root, from, &changes)?
      .into_iter()
      .filter(|path| !changes.iter().any(|(changed, _)| changed.as_slice() == path.as_bytes())),
  );
  if !blocking.is_empty() {
    blocking.sort();
    blocking.dedup();
    return Err(format!(
      "these are not recorded (nv does not track them, or may not read them), and stand where the state restored has something else: {}\nMove them away and restore again. Nothing was written; what was on disk and could be recorded, is.",
      blocking.join(", ")
    ));
  }
  // The rules of what is left out, when a restore changes them, change which files there are to
  // record: the next command that trusts reported writes is told, and looks at everything.
  let rules: Vec<Vec<u8>> = changes
    .iter()
    .map(|(path, _)| path.clone())
    .filter(|path| path.rsplit(|&b| b == b'/').next().is_some_and(|name| name == b".gitignore"))
    .collect();
  ws.note(&rules)?;
  for (path, entry) in removals_first(changes) {
    match entry {
      Some(entry) => {
        placed.add(path.clone(), place_with(store, &ws.root, &path, entry, &mut eols)?);
        done.written += 1;
      }
      None => {
        // The state gone back to does not have it, so neither does the index: if the file
        // could not be removed, the next look finds it as a file that is new.
        index.remove(&path);
        if unplace(&ws.root, &path) {
          done.removed += 1;
        } else {
          stuck.push(String::from_utf8_lossy(&path).into_owned());
        }
      }
    }
  }
  ws.log("restore", &target)?;
  ws.head = Some(target);
  placed.into_index(&mut index);
  ws.save()?;
  index.save()?;
  if !stuck.is_empty() {
    return Err(format!(
      "restored, except that these could not be removed and are still there: {}\n{} files written, {} removed",
      stuck.join(", "),
      done.written,
      done.removed
    ));
  }
  // A file that may not be read was not recorded, and so was not restored: what it holds is not
  // known to be what the state has. Not a restore that went through.
  if !looked.unreadable.is_empty() {
    let mut left: Vec<String> = looked.unreadable.iter().map(|path| String::from_utf8_lossy(path).into_owned()).collect();
    left.sort();
    return Err(format!(
      "restored, except these, which may not be read: they were not recorded, and are left as they are: {}\n{} files written, {} removed. Make them readable and restore again.",
      left.join(", "),
      done.written,
      done.removed
    ));
  }
  Ok(done)
}

/// Where a workspace stood before it last caught up with its ref: enough to
/// stand there again, the files and what it believes of the ref alike.
pub struct Standing {
  pub head: Option<Id>,
  pub base: Option<Id>,
  pub pushed: Option<Id>,
  pub seq: u64,
}

impl Standing {
  fn file(ws: &Workspace) -> PathBuf {
    ws.root.join(META).join("before-sync")
  }

  pub(crate) fn keep(&self, ws: &Workspace) -> Result<()> {
    let id = |id: Option<Id>| id.map_or("-".to_string(), |id| hex(&id));
    crate::write_whole(&Self::file(ws), format!("{} {} {} {}\n", id(self.head), id(self.base), id(self.pushed), self.seq))
  }

  /// Note the commit the catching-up left the workspace on: undoing it is offered only while the
  /// workspace is still there.
  pub(crate) fn arrived_at(ws: &Workspace, base: Id) {
    if let Ok(text) = std::fs::read_to_string(Self::file(ws)) {
      let kept: Vec<&str> = text.split_whitespace().take(4).collect();
      let _ = crate::write_whole(&Self::file(ws), format!("{} {}\n", kept.join(" "), hex(&base)));
    }
  }

  /// The commit the catching-up left the workspace on, if that was noted.
  pub fn left_at(ws: &Workspace) -> Option<Id> {
    let text = std::fs::read_to_string(Self::file(ws)).ok()?;
    text.split_whitespace().nth(4).and_then(unhex).and_then(|bytes| Id::try_from(bytes).ok())
  }

  pub(crate) fn forget(ws: &Workspace) {
    let _ = std::fs::remove_file(Self::file(ws));
  }

  pub fn kept(ws: &Workspace) -> Option<Standing> {
    let text = std::fs::read_to_string(Self::file(ws)).ok()?;
    let mut fields = text.split_whitespace();
    let mut id = || fields.next().map(|field| unhex(field).and_then(|b| Id::try_from(b).ok()));
    let (head, base, pushed) = (id()?, id()?, id()?);
    Some(Standing { head, base, pushed, seq: fields.next()?.parse().ok()? })
  }
}

/// Undo the last catching-up with the ref: the files as they were before
/// it, and the workspace on the commits it was on, so that what the ref has
/// since is again something the workspace has not seen. A push will be
/// refused as it was before, and a sync will merge again.
pub fn restore_before_sync(store: &mut Store, ws: &mut Workspace, author: &str, look: Look) -> Result<Restored> {
  // A push of what the sync made may have landed: undoing the sync would make its commits again.
  not_unheard(ws, "undoing the last sync")?;
  let Some(was) = Standing::kept(ws) else {
    return Err(
      "there is no catching-up to undo: this workspace has not synced since it was made, or has pushed since it last did".to_string(),
    );
  };
  let head = was.head.ok_or("before it caught up, this workspace had no commit to go back to")?;
  // Commits made since, or another branch gone to, are not part of that catching-up, and undoing
  // it would drop them without a word.
  if let Some(left) = Standing::left_at(ws)
    && ws.base != Some(left)
  {
    return Err(
      "this workspace has committed, or moved to other commits, since it last caught up: undoing that catching-up now would drop what was done since.\nTo set this workspace's unpushed work aside instead: nv restore <dir> pushed".to_string(),
    );
  }
  let based = ws.base;
  let done = restore_across(store, ws, head, author, look)?;
  (ws.base, ws.pushed, ws.seq) = (was.base, was.pushed, was.seq);
  ws.save()?;
  take_branch_along(store, ws, based)?;
  set_unsettled(ws, &[])?;
  set_settling(ws, &[])?;
  Ok(done)
}

/// In a workspace `nv git` answers for, the branch it is on is where its commit is. A command
/// that moves the workspace's commit other than through `nv git` (a sync, a push that was put on
/// a newer tip, `nv commit`) takes the branch along, if the branch was where the workspace was.
pub fn take_branch_along(store: &mut Store, ws: &Workspace, was: Option<Id>) -> Result<()> {
  let head = std::fs::read_to_string(ws.root.join(META).join("git-head")).unwrap_or_default();
  let (Some(name), Some(now)) = (head.trim().strip_prefix("ref: "), ws.base) else { return Ok(()) };
  let name = format!("heads/{name}");
  let tip = store.read_ref(&name);
  if tip != Some(now) && (tip == was || tip.is_none()) {
    store.compare_and_set_ref(&name, tip, now)?;
  }
  Ok(())
}

/// Go to the ref as this workspace last saw it, leaving behind everything it has that the ref
/// does not: commits published here and not pushed, snapshots, and what is on disk. All of it is
/// recorded first and its id handed back. For a workspace whose own work cannot go on top of
/// what the ref has since.
pub fn set_aside(store: &mut Store, ws: &mut Workspace, author: &str, look: Look) -> Result<Restored> {
  // What is set aside is what the ref does not have: not known while a push may have landed.
  not_unheard(ws, "setting aside what was not pushed")?;
  // The ref as last seen; for a workspace that never saw it, the commit the workspace was made at.
  let pushed = ws.pushed.or(ws.start).ok_or("this workspace has not pushed to or synced with a server, and was not made at a commit")?;
  snapshot(store, ws, author, look)?;
  let was = ws.head.filter(|head| *head != pushed);
  let based = ws.base;
  let mut done = restore_across(store, ws, pushed, author, look)?;
  (ws.base, ws.head) = (Some(pushed), Some(pushed));
  ws.save()?;
  take_branch_along(store, ws, based)?;
  set_unsettled(ws, &[])?;
  set_settling(ws, &[])?;
  if let Some(was) = was {
    ws.log("set-aside", &was)?;
  }
  done.kept = was;
  Ok(done)
}

/// What a sync left with both versions, and the commits of others' it met there: for the commit
/// that settles each file to say so (`Settled:`), as a history read later can find out no other way.
fn settling_file(ws: &Workspace) -> PathBuf {
  ws.root.join(META).join("settling")
}

fn settling(ws: &Workspace) -> Vec<(String, Vec<String>)> {
  std::fs::read_to_string(settling_file(ws))
    .unwrap_or_default()
    .lines()
    .filter_map(|line| {
      let (path, against) = line.split_once('\t')?;
      Some((path.to_string(), against.split('\t').filter(|a| !a.is_empty()).map(str::to_string).collect()))
    })
    .collect()
}

fn set_settling(ws: &Workspace, entries: &[(String, Vec<String>)]) -> Result<()> {
  if entries.is_empty() {
    let _ = std::fs::remove_file(settling_file(ws));
    return Ok(());
  }
  let text: String = entries.iter().map(|(path, against)| format!("{path}\t{}\n", against.join("\t"))).collect();
  crate::write_whole(&settling_file(ws), text)
}

/// Note, of each of `paths` a sync left with both versions, the commits on the way back from
/// `tip` to `common` that changed it: what it was settled against, by Git's id and summary line.
/// Where a Git id cannot be had, nv's is given, and said to be nv's.
pub(crate) fn note_settling(store: &mut Store, ws: &Workspace, tip: Id, common: Option<Id>, paths: &[String]) -> Result<()> {
  if paths.is_empty() {
    return Ok(());
  }
  let mut met: Vec<(String, Vec<Id>)> = paths.iter().map(|p| (p.clone(), Vec::new())).collect();
  let (mut at, mut walked) = (Some(tip), 0);
  while let Some(id) = at.filter(|id| Some(*id) != common && walked < 100_000) {
    let commit = Commit::load(store, &id)?;
    for (path, ids) in met.iter_mut() {
      if commit.changes.iter().any(|(changed, _)| changed.as_slice() == path.as_bytes()) {
        ids.insert(0, id);
      }
    }
    at = commit.parents.first().copied();
    walked += 1;
  }
  let mut all = settling(ws);
  for (path, ids) in met {
    let mut against = Vec::new();
    for id in ids {
      let named = match crate::git::git_id_of(store, &id) {
        Ok(oid) => hex(&oid)[..7].to_string(),
        Err(_) => format!("nv {}", &hex(&id)[..12]),
      };
      let said = String::from_utf8_lossy(&Commit::load(store, &id)?.rest).into_owned();
      let subject = said.split_once("\n\n").and_then(|(_, m)| m.lines().next()).unwrap_or_default().to_string();
      against.push(format!("{named} ({subject})"));
    }
    match all.iter_mut().find(|(p, _)| *p == path) {
      Some((_, known)) => known.extend(against.into_iter().filter(|a| !known.contains(a)).collect::<Vec<_>>()),
      None => all.push((path, against)),
    }
  }
  set_settling(ws, &all)
}

/// A commit's message, with a `Settled:` line for each file it holds that a sync left with both
/// versions, now settled: which, and against whose commits. Those are then no longer noted.
fn with_settled(_store: &mut Store, ws: &Workspace, rest: Vec<u8>, changes: &[Change]) -> Result<Vec<u8>> {
  let all = settling(ws);
  if all.is_empty() {
    return Ok(rest);
  }
  let left = unsettled(ws);
  let (taken, kept): (Vec<_>, Vec<_>) =
    all.into_iter().partition(|(path, _)| !left.contains(path) && changes.iter().any(|(changed, _)| changed.as_slice() == path.as_bytes()));
  if taken.is_empty() {
    return Ok(rest);
  }
  let lines: Vec<String> = taken
    .iter()
    .map(
      |(path, against)| {
        if against.is_empty() { format!("Settled: {path}") } else { format!("Settled: {path} with {}", against.join(", ")) }
      },
    )
    .collect();
  set_settling(ws, &kept)?;
  Ok(with_trailers(rest, &lines))
}

/// `rest` with these lines at the end of its message, as Git's trailers are, those it has already
/// left out.
pub(crate) fn with_trailers(rest: Vec<u8>, lines: &[String]) -> Vec<u8> {
  let said = String::from_utf8_lossy(&rest).into_owned();
  let new: Vec<&String> = lines.iter().filter(|line| !said.lines().any(|l| l == line.as_str())).collect();
  if new.is_empty() {
    return rest;
  }
  let body = said.trim_end();
  // After a paragraph of trailers, more of them; after anything else, a paragraph of their own.
  let last = body.rsplit("\n\n").next().unwrap_or_default();
  let trailing = !last.is_empty() && last.lines().all(|l| l.starts_with("Settled: "));
  let joined = new.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n");
  format!("{body}{}{joined}\n", if trailing { "\n" } else { "\n\n" }).into_bytes()
}

/// The `Settled:` lines of a commit's message.
pub(crate) fn settled_lines(rest: &[u8]) -> Vec<String> {
  String::from_utf8_lossy(rest).lines().filter(|l| l.starts_with("Settled: ")).map(str::to_string).collect()
}

fn unsettled_file(ws: &Workspace) -> PathBuf {
  ws.root.join(META).join("conflicts")
}

fn moved_file(ws: &Workspace) -> PathBuf {
  ws.root.join(META).join("moved")
}

/// Note that this workspace's own commit was put on commits of others that change files it
/// changes, and what those say of themselves: its message was written before them.
pub(crate) fn set_moved(ws: &Workspace, commit: Option<Id>, under: &[String]) -> Result<()> {
  match commit {
    Some(id) => crate::write_whole(&moved_file(ws), format!("{}\n{}\n", hex(&id), under.join("\n"))),
    None => {
      let _ = std::fs::remove_file(moved_file(ws));
      Ok(())
    }
  }
}

/// The commit whose message was written before the commits of others it now stands on, and those
/// commits' summary lines.
pub fn moved_under(ws: &Workspace) -> Option<(Id, Vec<String>)> {
  let text = std::fs::read_to_string(moved_file(ws)).ok()?;
  let mut lines = text.lines();
  let id = lines.next().and_then(crate::unhex).and_then(|b| <Id>::try_from(b.as_slice()).ok())?;
  Some((id, lines.filter(|l| !l.is_empty()).map(str::to_string).collect()))
}

fn incomplete_file(ws: &Workspace) -> PathBuf {
  ws.root.join(META).join("incomplete")
}

/// Note the commit of this workspace's own that a sync put on the ref's tip without the files it
/// left holding both versions, and which they are: the commit their settling completes.
pub(crate) fn set_incomplete(ws: &Workspace, commit: Option<Id>, paths: &[String]) -> Result<()> {
  match commit.filter(|_| !paths.is_empty()) {
    Some(id) => crate::write_whole(&incomplete_file(ws), format!("{}\n{}\n", hex(&id), paths.join("\n"))),
    None => {
      let _ = std::fs::remove_file(incomplete_file(ws));
      Ok(())
    }
  }
}

/// The commit a settling is noted to complete, as noted: whether it still can is `incomplete`'s.
pub fn settles_into(ws: &Workspace) -> Option<Id> {
  let text = std::fs::read_to_string(incomplete_file(ws)).ok()?;
  text.lines().next().and_then(crate::unhex).and_then(|b| <Id>::try_from(b.as_slice()).ok())
}

/// The commit a settling completes, and the paths it was left without, while that commit is
/// still the last made here, not pushed, and no push of it may have landed unheard.
fn incomplete(store: &mut Store, ws: &Workspace) -> Result<Option<(Id, Vec<Vec<u8>>)>> {
  let Ok(text) = std::fs::read_to_string(incomplete_file(ws)) else { return Ok(None) };
  let mut lines = text.lines();
  let Some(id) = lines.next().and_then(crate::unhex).and_then(|b| <Id>::try_from(b.as_slice()).ok()) else { return Ok(None) };
  // One of the commits not pushed (a sync puts them on the tip one by one, and a clash is the
  // one's that changed the file); those after it are made again on top when it is completed.
  if unheard(ws).exists() || !pending(store, ws)?.iter().any(|(c, _)| *c == id) {
    return Ok(None);
  }
  Ok(Some((id, lines.map(|l| l.as_bytes().to_vec()).collect())))
}

/// Note the files a sync left holding both versions between markers.
pub(crate) fn set_unsettled(ws: &Workspace, paths: &[String]) -> Result<()> {
  if paths.is_empty() {
    let _ = std::fs::remove_file(unsettled_file(ws));
    return Ok(());
  }
  crate::write_whole(&unsettled_file(ws), paths.join("\n") + "\n")
}

/// The files a sync left for someone to settle that still hold its markers.
/// A file that no longer does is settled, and forgotten here.
pub fn unsettled(ws: &Workspace) -> Vec<String> {
  let listed = std::fs::read_to_string(unsettled_file(ws)).unwrap_or_default();
  let marked = |path: &&str| {
    std::fs::read(ws.root.join(path))
      .is_ok_and(|bytes| bytes.split(|&b| b == b'\n').any(|line| line.starts_with(b"<<<<<<< yours") || line.starts_with(b">>>>>>> theirs")))
  };
  let left: Vec<String> = listed.lines().filter(marked).map(str::to_string).collect();
  let _ = set_unsettled(ws, &left);
  left
}

fn mark_file(ws: &Workspace, name: &str) -> PathBuf {
  ws.root.join(META).join("marks").join(name)
}

/// Record what is on disk and give that state a name, taking the name from
/// whatever state had it before.
pub fn mark(store: &mut Store, ws: &mut Workspace, name: &str, author: &str, look: Look) -> Result<Id> {
  let plain = !name.is_empty()
    && name.len() <= 64
    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    && !name.starts_with('.');
  if !plain || ["base", "pushed", "before-sync"].contains(&name) {
    return Err(format!("`{name}` cannot be a name: letters, digits, `-`, `_` and `.` only, and not `base`, `pushed` or `before-sync`"));
  }
  snapshot(store, ws, author, look)?;
  let id = ws.head.ok_or("there is nothing to name yet")?;
  let file = mark_file(ws, name);
  std::fs::create_dir_all(file.parent().expect("under .nv")).map_err(|e| e.to_string())?;
  crate::write_whole(&file, hex(&id))?;
  ws.log(&format!("mark:{name}"), &id)?;
  Ok(id)
}

/// Give a state already recorded a name, as `mark` does for the state it records.
pub fn name_state(ws: &Workspace, name: &str, id: &Id) -> Result<()> {
  let file = mark_file(ws, name);
  std::fs::create_dir_all(file.parent().expect("under .nv")).map_err(|e| e.to_string())?;
  crate::write_whole(&file, hex(id))?;
  ws.log(&format!("mark:{name}"), id)
}

/// A recorded state by what someone would call it: a name given with
/// `mark`, `base` for the last published commit, `~N` for the Nth snapshot
/// or commit before the latest, or the start of an id.
pub fn state_named(store: &mut Store, ws: &Workspace, spec: &str) -> Result<Id> {
  let parse = |text: &str| unhex(text.trim()).and_then(|b| Id::try_from(b).ok());
  if spec == "base" {
    return ws.base.ok_or_else(|| "nothing has been published yet".to_string());
  }
  if spec == "before-sync" {
    return Standing::kept(ws).and_then(|was| was.head).ok_or_else(|| {
      "there is no catching-up to undo: this workspace has not synced since it was made, or has pushed since it last did".to_string()
    });
  }
  if spec == "pushed" {
    // For a workspace that has not seen the ref, the commit it was made at: where it goes to set its work aside.
    return ws
      .pushed
      .or(ws.start)
      .ok_or_else(|| "this workspace has not pushed to or synced with a server, and was not made at a commit".to_string());
  }
  if let Some(id) = std::fs::read_to_string(mark_file(ws, spec)).ok().and_then(|text| parse(&text)) {
    return Ok(id);
  }
  let logged: Vec<(String, String)> = ws.operations().into_iter().map(|(_, what, id)| (what, id)).collect();
  // `~N`: N recorded states back from where the workspace is, snapshots and commits alike.
  // The states this workspace recorded, newest first, and behind the oldest of them the commits it was made on.
  if let Some(back) = spec.strip_prefix('~').and_then(|n| n.parse::<usize>().ok()) {
    let states: Vec<&String> = logged.iter().filter(|(what, _)| what == "snapshot" || what == "commit").map(|(_, id)| id).collect();
    if let Some(at) = states.len().checked_sub(back + 1) {
      return parse(states[at]).ok_or_else(|| "the log of operations is damaged".to_string());
    }
    let mut at = match states.first() {
      Some(oldest) => parse(oldest).ok_or_else(|| "the log of operations is damaged".to_string())?,
      None => ws.head.ok_or_else(|| "nothing has been recorded yet".to_string())?,
    };
    for _ in 0..back + 1 - states.len().max(1) {
      at = Commit::load(store, &at)?.parents.first().copied().ok_or_else(|| format!("there are not {back} states before this one"))?;
    }
    return Ok(at);
  }
  let hexish = spec.len() >= 6 && spec.bytes().all(|b| b.is_ascii_hexdigit());
  let mut found: Vec<&String> = logged.iter().map(|(_, id)| id).filter(|id| hexish && id.starts_with(&spec.to_ascii_lowercase())).collect();
  found.sort();
  found.dedup();
  match found[..] {
    [one] => parse(one).ok_or_else(|| "the log of operations is damaged".to_string()),
    [] if spec.bytes().all(|b| b.is_ascii_hexdigit()) && spec.len() < 6 => {
      Err(format!("`{spec}` is too short to name a recorded state by its id: give at least six characters"))
    }
    [] => Err(format!("`{spec}` is not a name given with `nv mark`, `base`, `pushed`, `~N`, or the start of a recorded state's id")),
    _ => Err(format!("`{spec}` is the start of {} ids", found.len())),
  }
}

/// The paths that differ between a recorded state and the files now, with
/// `A`, `M` or `D` for what happened to each since.
pub fn since(store: &mut Store, ws: &Workspace, state: Id, look: Look) -> Result<Vec<(Vec<u8>, char)>> {
  let from = root_of(store, &state)?;
  let to = match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  };
  // What that state had, for every path that has changed since: recorded changes first, then the files.
  let mut then: std::collections::BTreeMap<Vec<u8>, Option<Entry>> = std::collections::BTreeMap::new();
  let mut now: std::collections::BTreeMap<Vec<u8>, Option<Entry>> = std::collections::BTreeMap::new();
  for (path, entry) in crate::manifest::diff(store, from, to)? {
    let was = match from {
      Some(root) => crate::manifest::lookup(store, root, &path)?,
      None => None,
    };
    then.insert(path.clone(), was);
    now.insert(path, entry);
  }
  for (path, new, old) in status(ws, look)?.0 {
    then.entry(path.clone()).or_insert(old);
    now.insert(path, new.map(|(mode, bytes)| Entry { mode, id: object_id(Kind::Blob, &bytes) }));
  }
  let mut out = Vec::new();
  for (path, was) in then {
    match (was, now.get(&path).copied().flatten()) {
      (a, b) if a == b => {}
      (None, _) => out.push((path, 'A')),
      (_, None) => out.push((path, 'D')),
      _ => out.push((path, 'M')),
    }
  }
  Ok(out)
}

/// Turn the snapshots since the last published commit back into plain
/// differences from it. The files are not touched and the snapshots stay in
/// the store, to be gone back to by id or by name. For a view of the
/// workspace that has no notion of snapshots, such as Git's.
pub fn fold(store: &mut Store, ws: &mut Workspace) -> Result<()> {
  if ws.head == ws.base {
    return Ok(());
  }
  // What the snapshots held is, from here on, a difference between the files and the commit
  // like any other: a command that trusts reported writes is told of those paths again.
  let tree = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  let (from, to) = (tree(store, ws.base)?, tree(store, ws.head)?);
  let _ = (from, to);
  // Where the snapshots were, so that nv's own commands find them again: a look in Git's terms
  // is not to change what a later `nv commit` publishes.
  if let Some(head) = ws.head {
    let base = ws.base.map_or("-".to_string(), |base| hex(&base));
    crate::write_whole(&ws.root.join(META).join("folded"), format!("{} {base}\n", hex(&head)))?;
  }
  let base = ws.base;
  move_head(store, ws, base)
}

/// Undo `fold`, for a command of nv's own: the workspace is again on the snapshots it had before
/// a look in Git's terms set them aside, provided nothing was committed there in between.
pub fn unfold(store: &mut Store, ws: &mut Workspace) -> Result<()> {
  let noted = ws.root.join(META).join("folded");
  let Ok(text) = std::fs::read_to_string(&noted) else { return Ok(()) };
  let mut words = text.split_whitespace();
  let id = |word: Option<&str>| word.and_then(unhex).and_then(|bytes| Id::try_from(bytes).ok());
  let (head, base) = (id(words.next()), words.clone().next().map(|word| if word == "-" { None } else { id(Some(word)) }));
  // `base` is `Some(None)` where the snapshots were before any commit.
  if let (Some(head), Some(base)) = (head, base)
    && ws.base == base
    && ws.head == base
    && store.contains(&head)
  {
    // The index first, then where the workspace stands, in one write: the head on the snapshots,
    // the base where it was. Written as two, a kill between them left the snapshots as the
    // workspace's commit, and the next push published what nobody had committed.
    let tree = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
    let (from, to) = (tree(store, ws.head)?, tree(store, Some(head))?);
    let mut index = ws.load_index();
    for (path, entry) in crate::manifest::diff(store, from, to)? {
      match entry {
        // Known to be this entry in the snapshot; whether the file still is, is for the next look to say.
        Some(entry) => index.insert(path, Indexed { entry, size: 0, mtime: 0, ctime: 0 }),
        None => index.remove(&path),
      }
    }
    index.save()?;
    ws.head = Some(head);
    ws.save()?;
  }
  let _ = std::fs::remove_file(noted);
  Ok(())
}

/// A workspace with no commit yet, and a store of its own inside it: what
/// `git init` makes.
pub fn init(root: &Path) -> Result<Workspace> {
  let meta = root.join(META);
  std::fs::create_dir_all(&meta).map_err(|e| format!("cannot create {}: {e}", meta.display()))?;
  drop(Store::create(&meta.join("store"))?);
  let store = std::fs::canonicalize(meta.join("store")).map_err(|e| e.to_string())?;
  let ws = Workspace {
    root: root.to_path_buf(),
    store,
    base: None,
    head: None,
    prefixes: Vec::new(),
    remote: None,
    remote_ref: None,
    pushed: None,
    start: None,
    seq: 0,
  };
  ws.save()?;
  Ok(ws)
}

/// Commit the differences `select` picks, and only those, with `rest` as the
/// commit's author, committer and message. The others stay differences.
/// `None` when nothing was picked and an empty commit was not asked for.
pub fn commit_selected(
  store: &mut Store,
  ws: &mut Workspace,
  rest: Vec<u8>,
  select: &dyn Fn(&Difference) -> bool,
  allow_empty: bool,
) -> Result<Option<Id>> {
  let (found, _) = status(ws, Look::Everywhere)?;
  let chosen: Vec<Difference> = found.into_iter().filter(|d| select(d)).collect();
  if chosen.is_empty() && !allow_empty {
    return Ok(None);
  }
  let mut index = ws.load_index();
  let mut changes: Vec<Change> = Vec::with_capacity(chosen.len());
  for (path, new, old) in chosen {
    match new {
      Some((mode, bytes)) => {
        let entry = Entry { mode, id: store.put(Kind::Blob, &bytes, old.map(|o| o.id))? };
        index.insert(path.clone(), indexed(&ws.root.join(to_path(&path)), entry));
        changes.push((path, Some(entry)));
      }
      None => {
        index.remove(&path);
        changes.push((path, None));
      }
    }
  }
  let id = Commit { parents: ws.head.into_iter().collect(), rest, changes }.store(store)?;
  root_of(store, &id)?;
  store.flush()?;
  ws.log("commit", &id)?;
  (ws.base, ws.head) = (Some(id), Some(id));
  ws.save()?;
  index.save()?;
  Ok(Some(id))
}

/// Commit exactly `changes`, whatever is on disk. The files stay as they
/// are, so a file that differs from what was committed shows as a
/// difference afterwards. This is what committing from an index is.
pub fn commit_entries(store: &mut Store, ws: &mut Workspace, rest: Vec<u8>, changes: Vec<Change>, merged: Option<Id>) -> Result<Id> {
  commit_entries_as(store, ws, rest, changes, merged, None)
}

/// As `commit_entries`; with `replacing`, the commit made takes the place of the workspace's
/// commit (an amend): on that one's parents, with everything since them as its changes. Either
/// way the store has the commit before the workspace names it, and the workspace is written once:
/// a kill at any moment leaves what was or what was meant.
pub fn commit_entries_as(
  store: &mut Store,
  ws: &mut Workspace,
  rest: Vec<u8>,
  changes: Vec<Change>,
  merged: Option<Id>,
  replacing: Option<&Commit>,
) -> Result<Id> {
  let mut index = ws.load_index();
  for (path, entry) in &changes {
    match entry {
      // Known to be this entry in the commit; whether the file is, the next look will say.
      Some(entry) => index.insert(path.clone(), Indexed { entry: *entry, size: 0, mtime: 0, ctime: 0 }),
      None => index.remove(path),
    }
  }
  // A merge being concluded has the commit merged in as a second parent.
  let mut id = Commit { parents: ws.head.into_iter().chain(merged).collect(), rest: rest.clone(), changes }.store(store)?;
  root_of(store, &id)?;
  if let Some(previous) = replacing {
    let root = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
    let (from, to) = (root(store, previous.parents.first().copied())?, root(store, Some(id))?);
    let changes = crate::manifest::diff(store, from, to)?;
    id = Commit { parents: previous.parents.clone(), rest, changes }.store(store)?;
    root_of(store, &id)?;
  }
  store.flush()?;
  ws.log("commit", &id)?;
  (ws.base, ws.head) = (Some(id), Some(id));
  ws.save()?;
  index.save()?;
  Ok(id)
}

/// Write these entries to their paths, without the workspace taking them as
/// what it is on: the files then differ from its commit by exactly this.
pub fn write_entries(store: &mut Store, ws: &Workspace, entries: &[(Vec<u8>, Entry)]) -> Result<()> {
  let mut eols = Eols::of_worktree(&ws.root);
  for (path, entry) in entries {
    place_with(store, &ws.root, path, *entry, &mut eols)?;
  }
  Ok(())
}

/// Move the workspace to another commit. Refused, with nothing touched, if
/// a file it would change has been changed here, or sits here untracked.
pub fn switch(store: &mut Store, ws: &mut Workspace, target: Id) -> Result<()> {
  switch_over(store, ws, target, &[])
}

/// `switch`, where the files at `staged` are what the commit gone to has (staged as it has them,
/// for a fast-forward): those are not in the way, if on disk they are that too.
pub fn switch_over(store: &mut Store, ws: &mut Workspace, target: Id, staged: &[Vec<u8>]) -> Result<()> {
  let (found, _) = status(ws, Look::Everywhere)?;
  let from = match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  };
  let to = root_of(store, &target)?;
  let changes = removals_first(crate::manifest::diff(store, from, to)?);
  // A file gone from the disk that the commit gone to does not have either is not in the way, nor
  // one staged as the commit has it that is that on disk too.
  let already = |path: &Vec<u8>, new: &Option<Content>, entry: &Option<Entry>| match (new, entry) {
    (Some((mode, bytes)), Some(entry)) => staged.contains(path) && *mode == entry.mode && object_id(Kind::Blob, bytes) == entry.id,
    _ => false,
  };
  let in_the_way: Vec<String> = found
    .iter()
    .filter(|(path, new, _)| changes.iter().any(|(p, entry)| p == path && (new.is_some() || entry.is_some()) && !already(path, new, entry)))
    .map(|(path, _, _)| String::from_utf8_lossy(path).into_owned())
    .collect();
  if !in_the_way.is_empty() {
    return Err(format!("Your local changes to the following files would be overwritten by checkout:\n\t{}", in_the_way.join("\n\t")));
  }
  let blocking = self::in_the_way(&ws.root, &changes);
  if !blocking.is_empty() {
    return Err(format!("Updating the following directories would lose untracked files in them:\n\t{}", blocking.join("\n\t")));
  }
  let mut untracked = untracked_in_the_way(store, &ws.root, from, &changes)?;
  untracked.retain(|path| {
    !found.iter().any(|(p, new, _)| p.as_slice() == path.as_bytes() && changes.iter().any(|(c, entry)| c == p && already(p, new, entry)))
  });
  if !untracked.is_empty() {
    return Err(format!(
      "The following untracked working tree files would be overwritten by checkout:\n\t{}\nPlease move or remove them before you switch branches.",
      untracked.join("\n\t")
    ));
  }
  let mut index = ws.load_index();
  let mut placed = Placed::default();
  let mut eols = Eols::of_worktree(&ws.root);
  for (path, entry) in &changes {
    match entry {
      Some(entry) => placed.add(path.clone(), place_with(store, &ws.root, path, *entry, &mut eols)?),
      None => {
        unplace(&ws.root, path);
        index.remove(path);
      }
    }
  }
  (ws.base, ws.head) = (Some(target), Some(target));
  placed.into_index(&mut index);
  ws.save()?;
  index.save()
}

/// What nv does not track and that stands where these changes would write: a directory holding
/// such files where a file has to go, a file where a directory has to be made, a file under the
/// name (to the file system) of a new one. To be asked before the first of them is written.
pub fn blocked(store: &mut Store, ws: &Workspace, changes: &[Change]) -> Result<Vec<String>> {
  let from = match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  };
  let mut blocking = in_the_way(&ws.root, changes);
  blocking.extend(untracked_in_the_way(store, &ws.root, from, changes)?);
  blocking.sort();
  blocking.dedup();
  Ok(blocking)
}

/// What nv does not track and that stands in the way of putting every tracked file back as the
/// workspace's commit has it and then going to `target`: found before a hard reset discards
/// anything, so that it is done whole or not begun.
pub fn hard_reset_blocked(store: &mut Store, ws: &Workspace, target: Option<Id>, going: &[Vec<u8>]) -> Result<Vec<String>> {
  let tree = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  let (from, to) = (tree(store, ws.head)?, tree(store, target.or(ws.head))?);
  // The tracked files that are not as committed: each is written again.
  let (found, _) = status(ws, Look::Everywhere)?;
  let mut changes: Vec<Change> = found.into_iter().filter_map(|(path, _, old)| old.map(|old| (path, Some(old)))).collect();
  let mut blocking = untracked_in_the_way(store, &ws.root, from, &changes)?;
  blocking.extend(in_the_way(&ws.root, &changes));
  // And what going to the target writes.
  changes = crate::manifest::diff(store, from, to)?;
  blocking.extend(in_the_way(&ws.root, &changes));
  blocking.extend(untracked_in_the_way(store, &ws.root, from, &changes)?);
  // Less what the reset removes itself before it writes: files added and in no commit.
  blocking.retain(|path| !going.iter().any(|gone| gone.as_slice() == path.as_bytes()));
  blocking.sort();
  blocking.dedup();
  Ok(blocking)
}

/// Make the workspace's files what `changes` say, on top of what it has.
/// Refused, with nothing touched, if one of those files was changed here.
pub fn apply_changes(store: &mut Store, ws: &Workspace, changes: &[Change]) -> Result<()> {
  apply_into(store, ws, changes)?.save()
}

/// `apply_changes`, with the index left for the caller to save.
fn apply_into(store: &mut Store, ws: &Workspace, changes: &[Change]) -> Result<Index> {
  // A change to a whole directory is, on disk, the files that differ.
  let root = match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  };
  let changes = &removals_first(crate::manifest::expand(store, root, changes)?);
  let (found, _) = status(ws, Look::Everywhere)?;
  let in_the_way: Vec<String> = found
    .iter()
    .filter(|(path, _, _)| changes.iter().any(|(p, _)| p == path))
    .map(|(path, _, _)| String::from_utf8_lossy(path).into_owned())
    .collect();
  if !in_the_way.is_empty() {
    return Err(format!("Your local changes to the following files would be overwritten:\n\t{}", in_the_way.join("\n\t")));
  }
  // And what nv does not track, standing where something has to be written: found before the first write.
  let mut blocking = self::in_the_way(&ws.root, changes);
  blocking.extend(untracked_in_the_way(store, &ws.root, root, changes)?);
  if !blocking.is_empty() {
    blocking.sort();
    blocking.dedup();
    return Err(format!("The following untracked working tree files would be overwritten by merge:\n\t{}", blocking.join("\n\t")));
  }
  let mut index = ws.load_index();
  let mut placed = Placed::default();
  let mut eols = Eols::of_worktree(&ws.root);
  for (path, entry) in changes {
    match entry {
      Some(entry) => placed.add(path.clone(), place_with(store, &ws.root, path, *entry, &mut eols)?),
      None => {
        unplace(&ws.root, path);
        index.remove(path);
      }
    }
  }
  placed.into_index(&mut index);
  Ok(index)
}

/// Take changes that did not come from the files: write them into the
/// workspace and record them as a snapshot, as if they had been made here.
/// Refused, with nothing touched, if one of those files was changed here.
pub fn take_changes(store: &mut Store, ws: &mut Workspace, changes: Vec<Change>, author: &str) -> Result<Id> {
  let mut index = apply_into(store, ws, &changes)?;
  let id = Commit { parents: ws.head.into_iter().collect(), rest: rest_of(author, "snapshot"), changes }.store(store)?;
  store.flush()?;
  ws.log("snapshot", &id)?;
  ws.head = Some(id);
  ws.save()?;
  index.save()?;
  Ok(id)
}

/// Put back, from the commit the workspace is on, the files among `paths`
/// (or under them) that differ from it. Files the commit does not have are
/// left alone. Returns how many were put back.
pub fn restore(store: &mut Store, ws: &Workspace, paths: &[Vec<u8>]) -> Result<usize> {
  let under =
    |path: &[u8]| paths.iter().any(|p| p.is_empty() || p == path || (path.len() > p.len() && path.starts_with(p) && path[p.len()] == b'/'));
  let (found, _) = status(ws, Look::Everywhere)?;
  // Whole or not begun: what stands in the way of one of the files is found before any is written.
  let wanted: Vec<Change> =
    found.iter().filter(|(path, _, old)| old.is_some() && under(path)).map(|(path, _, old)| (path.clone(), *old)).collect();
  let blocking = blocked(store, ws, &wanted)?;
  if !blocking.is_empty() {
    return Err(format!("these are not tracked, and stand where a tracked file has to be written: {}", blocking.join(", ")));
  }
  let mut index = ws.load_index();
  let mut placed = Placed::default();
  let mut restored = 0;
  let mut eols = Eols::of_worktree(&ws.root);
  for (path, _, old) in found {
    if let Some(entry) = old.filter(|_| under(&path)) {
      placed.add(path.clone(), place_with(store, &ws.root, &path, entry, &mut eols)?);
      restored += 1;
    }
  }
  placed.into_index(&mut index);
  index.save()?;
  Ok(restored)
}

/// Say the workspace is on `target` without touching its files: what the
/// files then differ by is whatever lies between the two commits.
pub fn move_head(store: &mut Store, ws: &mut Workspace, target: Option<Id>) -> Result<()> {
  move_head_trusting(store, ws, target, &|_| false)
}

/// `move_head`, where for some paths the caller knows the file on disk to be
/// what the commit has, though its bytes may not be the stored ones: Git
/// said so, having put the file through its own filters.
pub(crate) fn move_head_trusting(
  store: &mut Store,
  ws: &mut Workspace,
  target: Option<Id>,
  as_committed: &dyn Fn(&[u8]) -> bool,
) -> Result<()> {
  let root = |store: &mut Store, id: Option<Id>| match id {
    Some(id) => root_of(store, &id),
    None => Ok(None),
  };
  let (from, to) = (root(store, ws.head)?, root(store, target)?);
  let mut index = ws.load_index();
  for (path, entry) in crate::manifest::diff(store, from, to)? {
    match entry {
      Some(entry) if as_committed(&path) => {
        let now = indexed(&ws.root.join(to_path(&path)), entry);
        index.insert(path, now);
      }
      // Known to be this entry in the commit; whether the file still is, is for the next look to say.
      Some(entry) => index.insert(path, Indexed { entry, size: 0, mtime: 0, ctime: 0 }),
      None => index.remove(&path),
    }
  }
  (ws.base, ws.head) = (target, target);
  // The files did not move with the head, so here the index goes first: see `Workspace::save`.
  index.save()?;
  ws.save()
}

/// A second workspace with everything the first has on disk, tracked or not,
/// sharing its blocks until either side writes.
pub fn fork(from: &Path, to: &Path) -> Result<()> {
  Workspace::open(from)?;
  if to.exists() {
    return Err(format!("{} already exists", to.display()));
  }
  // A copy inside what it is a copy of would be files of the original, and copied into itself.
  let inside = |dir: &Path| std::fs::canonicalize(dir.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))).ok();
  if let (Some(whole), Some(parent)) = (std::fs::canonicalize(from).ok(), inside(to))
    && parent.starts_with(&whole)
  {
    return Err(format!("{} is inside {}: a fork goes beside the workspace, not in it", to.display(), from.display()));
  }
  // The index goes along with the files. It is brought up to date first, with nobody else at the
  // workspace: an edit the index has not seen would otherwise be, in the fork, an edit nobody can see.
  let differing = {
    let _lock = lock(from)?;
    let (found, _) = status(&Workspace::open(from)?, Look::Everywhere)?;
    if let Err(e) = clone_tree(from, to) {
      // Half a copy with nv's directory in it would pass for a workspace: taken away, its modes
      // first, which were copied with it and may not let it be.
      let _ = std::process::Command::new("chmod").arg("-R").arg("u+rwx").arg(to).status();
      let _ = std::fs::remove_dir_all(to);
      return Err(format!("cannot copy {} to {}: {e}", from.display(), to.display()));
    }
    found
  };
  // What differed from the index when the copy was made is not taken on the index's word in the copy.
  if !differing.is_empty() {
    let mut index = Index::open(&to.join(META));
    for (path, _, _) in &differing {
      if let Some(known) = index.get(path) {
        index.insert(path.clone(), Indexed { mtime: 0, ..known });
      }
    }
    index.save()?;
  }
  // The note of which directory the index is about, written last and saying "a copy": every file
  // of the fork changed, to the file system, before this did, and one that changes later is an edit.
  crate::write_whole(&to.join(META).join("index.root"), "a copy")
}

#[cfg(test)]
mod tests {
  use super::*;

  /// What is at a path, for finishing an arrival: a file or a link is read, nothing else is (a
  /// pipe would be waited on for ever), and what cannot be read is said apart from what is not
  /// there (reviewed with the twelfth round's fixes).
  #[test]
  fn content_at_reads_files_and_links_and_nothing_else() {
    let root = std::env::temp_dir().join(format!("nv-content-at-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("d")).unwrap();
    std::fs::write(root.join("f"), b"text").unwrap();
    std::os::unix::fs::symlink("f", root.join("l")).unwrap();
    let fifo = std::ffi::CString::new(root.join("p").as_os_str().as_bytes()).unwrap();
    // SAFETY: a path the test owns.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    assert_eq!(content_at(&root, b"f"), Ok(Some((0o100644, b"text".to_vec()))));
    assert_eq!(content_at(&root, b"l"), Ok(Some((MODE_LINK, b"f".to_vec()))));
    assert_eq!(content_at(&root, b"gone"), Ok(None));
    assert_eq!(content_at(&root, b"d"), Ok(None));
    assert_eq!(content_at(&root, b"p"), Err(Unknown::Unreadable));
    let _ = std::fs::remove_dir_all(&root);
  }
}
