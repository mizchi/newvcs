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

/// A name with its characters composed (NFC): how Git spells a name on a
/// file system that hands names back decomposed.
pub(crate) fn composed(name: &str) -> String {
  use unicode_normalization::UnicodeNormalization;
  name.nfc().collect()
}

pub(crate) fn to_path(bytes: &[u8]) -> PathBuf {
  PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
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
        std::fs::remove_dir_all(to)?;
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
  let status = std::process::Command::new("cp").arg("-R").arg(from).arg(to).status()?;
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
    self.save()?;
    index.save()?;
    if let Some(head) = &self.head {
      self.log("sync", head)?;
    }
    Ok(())
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

  pub(crate) fn log(&self, what: &str, id: &Id) -> Result<()> {
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
  /// Note that a path was written. The path is one inside the workspace:
  /// relative to its top, or absolute and below it. Anything else is
  /// refused, since what is noted here is read and committed.
  pub fn record(&self, path: &[u8]) -> Result<()> {
    use std::io::Write;
    let path = self.inside(path)?;
    let mut file =
      std::fs::File::options().append(true).create(true).open(self.root.join(META).join("journal")).map_err(|e| e.to_string())?;
    file.write_all(&[path.as_slice(), b"\0"].concat()).map_err(|e| e.to_string())
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
  Indexed { entry, size, mtime: if now.saturating_sub(mtime) < RACY_NANOS { 0 } else { mtime } }
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
  std::fs::create_dir_all(dest.parent().expect("under the workspace")).map_err(|e| fail(&dest, e))?;
  if let Some(bytes) = converted(store, eols, path, entry)? {
    // Whatever is there, a link included, makes way: the bytes go into a file of this name.
    let _ = std::fs::remove_file(&dest);
    let (size, mtime) = place_one(&files_dir, root, &Job { path: path.to_vec(), entry, bytes: Some(bytes), direct: true })?;
    return Ok(Indexed { entry, size, mtime });
  }
  let needs_bytes = entry.mode != MODE_GITLINK && (entry.mode == MODE_LINK || !cached.exists());
  let bytes = if needs_bytes { Some(blob(store, &entry.id)?) } else { None };
  let (size, mtime) = place_one(&files_dir, root, &Job { path: path.to_vec(), entry, bytes, direct: false })?;
  if is_attributes(path) {
    eols.forget();
  }
  Ok(Indexed { entry, size, mtime })
}

/// Remove a file from a workspace, and the directories that leaves empty.
/// Changes in the order to make them on disk: what goes, then what comes.
/// Where two names are one file to the file system, a change may remove one
/// and write the other; written first, the file would then be removed.
pub(crate) fn removals_first(mut changes: Vec<Change>) -> Vec<Change> {
  changes.sort_by_key(|(_, entry)| entry.is_some());
  changes
}

pub(crate) fn unplace(root: &Path, path: &[u8]) -> bool {
  let dest = root.join(to_path(path));
  // Gone already is as good as removed; still there afterwards is not.
  let _ = std::fs::remove_file(&dest);
  let removed = std::fs::symlink_metadata(&dest).is_err();
  let mut dir = dest.parent();
  while let Some(d) = dir.filter(|d| *d != root) {
    if std::fs::remove_dir(d).is_err() {
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
      for (path, entry) in &changes {
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
    Index::create(&meta_tmp, index.into_iter().map(|(path, entry, size, mtime)| (path, Indexed { entry, size, mtime })).collect())?;
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
  eols: &mut Eols,
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
  let (mode, mut bytes) = read_content(&full, &meta)?;
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
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    // The listing says whether the directory has rules of its own: no file is opened to find out.
    let mut listed: Vec<(std::ffi::OsString, bool)> = Vec::new();
    let mut has_rules = false;
    for entry in entries.flatten() {
      let name = entry.file_name();
      // A directory with a repository of its own in it is that repository's, not this one's.
      if name == ".git" && !rel.is_empty() {
        return Ok(());
      }
      has_rules |= name == ".gitignore";
      if !SKIPPED.iter().any(|s| name == *s) {
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
    Look::Tracked => status_at(ws, true, &[]),
    Look::Recorded => {
      let index = ws.load_index();
      let mut rules = crate::ignore::Checker::new(&ws.root);
      let mut todo = Vec::new();
      {
        let names = Names::of(ws, &index);
        for path in ws.recorded() {
          let ignored = rules.ignored(&path);
          let (path, known) = names.known(path);
          // A reported write to a file the rules ignore is not a file to track, as with a walk.
          if in_view(&ws.prefixes, &path, false) && (known.is_some() || !ignored) {
            todo.push((path, known));
          }
        }
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
    match std::str::from_utf8(path) {
      Ok(text) if !path.is_ascii() => composed(text).to_lowercase().into_bytes(),
      _ => path.to_ascii_lowercase(),
    }
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
fn finish(ws: &Workspace, mut index: Index, todo: Vec<(Vec<u8>, Option<Indexed>)>) -> Result<(Vec<Difference>, Looked)> {
  type Part = Result<(Looked, Vec<Difference>, Refreshed)>;
  let part = |todo: &[(Vec<u8>, Option<Indexed>)]| -> Part {
    let (mut looked, mut out, mut refreshed) = (Looked::default(), Vec::new(), Refreshed::new());
    let mut eols = Eols::of_worktree(&ws.root);
    for (path, known) in todo {
      compare(ws, path, known.as_ref(), &mut looked, &mut out, &mut refreshed, &mut eols)?;
    }
    Ok((looked, out, refreshed))
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
    out.extend(o);
    refreshed.extend(r);
  }
  out.sort_by(|a, b| a.0.cmp(&b.0));
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
  ws.save()?;
  index.save()?;
  ws.log("snapshot", &id)?;
  Ok(Some(id))
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
  let id = Commit { parents: ws.base.into_iter().collect(), rest, changes }.store(store)?;
  root_of(store, &id)?;
  store.flush()?;
  (ws.base, ws.head) = (Some(id), Some(id));
  ws.save()?;
  ws.log("commit", &id)?;
  Ok(Some(id))
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
  use std::os::fd::AsRawFd;
  let path = root.join(META).join("lock");
  let file = std::fs::File::options().create(true).append(true).open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
  // SAFETY: the descriptor is open for as long as `file` is.
  if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
    return Err(format!("cannot lock {}: {}", path.display(), std::io::Error::last_os_error()));
  }
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
  // A state from before the workspace last caught up with its ref does not have what others
  // pushed. Its files, on top of where the workspace now is, would be those changes undone,
  // and the next commit would publish the undoing.
  if let Some(pushed) = ws.pushed {
    let mut at = Some(target);
    let mut within = false;
    while let Some(id) = at {
      if id == pushed {
        within = true;
        break;
      }
      at = Commit::load(store, &id)?.parents.first().copied();
    }
    if !within {
      return Err(
        "that state is from before this workspace last caught up with its ref. Going back to its files would undo, in the next commit, what others pushed since.\nTo undo the catching-up itself, files and all: nv restore <dir> before-sync\nTo see what that state had: nv since <dir> <state>"
          .to_string(),
      );
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
    for (path, _) in crate::manifest::diff(store, from, to)? {
      ws.record(&path)?;
    }
  }
  snapshot(store, ws, author, look)?;
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
  for (path, entry) in removals_first(crate::manifest::diff(store, from, to)?) {
    if !in_view(&ws.prefixes, &path, false) {
      continue;
    }
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
  ws.head = Some(target);
  placed.into_index(&mut index);
  ws.save()?;
  index.save()?;
  ws.log("restore", &target)?;
  if !stuck.is_empty() {
    return Err(format!(
      "restored, except that these could not be removed and are still there: {}\n{} files written, {} removed",
      stuck.join(", "),
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
  let Some(was) = Standing::kept(ws) else {
    return Err("this workspace has not caught up with a ref since it was made: there is nothing to undo".to_string());
  };
  let head = was.head.ok_or("before it caught up, this workspace had no commit to go back to")?;
  let done = restore_across(store, ws, head, author, look)?;
  (ws.base, ws.pushed, ws.seq) = (was.base, was.pushed, was.seq);
  ws.save()?;
  set_unsettled(ws, &[])?;
  Ok(done)
}

fn unsettled_file(ws: &Workspace) -> PathBuf {
  ws.root.join(META).join("conflicts")
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
    std::fs::read(ws.root.join(path)).is_ok_and(|bytes| bytes.split(|&b| b == b'\n').any(|line| line.starts_with(b"<<<<<<< yours")))
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

/// A recorded state by what someone would call it: a name given with
/// `mark`, `base` for the last published commit, `~N` for the Nth snapshot
/// or commit before the latest, or the start of an id.
pub fn state_named(store: &mut Store, ws: &Workspace, spec: &str) -> Result<Id> {
  let parse = |text: &str| unhex(text.trim()).and_then(|b| Id::try_from(b).ok());
  if spec == "base" {
    return ws.base.ok_or_else(|| "nothing has been published yet".to_string());
  }
  if spec == "before-sync" {
    return Standing::kept(ws).and_then(|was| was.head).ok_or_else(|| "this workspace has not caught up with a ref yet".to_string());
  }
  if spec == "pushed" {
    return ws.pushed.ok_or_else(|| "this workspace has not pushed to or synced with a server yet".to_string());
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
  let base = ws.base;
  move_head(store, ws, base)
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
  (ws.base, ws.head) = (Some(id), Some(id));
  ws.save()?;
  index.save()?;
  ws.log("commit", &id)?;
  Ok(Some(id))
}

/// Commit exactly `changes`, whatever is on disk. The files stay as they
/// are, so a file that differs from what was committed shows as a
/// difference afterwards. This is what committing from an index is.
pub fn commit_entries(store: &mut Store, ws: &mut Workspace, rest: Vec<u8>, changes: Vec<Change>, merged: Option<Id>) -> Result<Id> {
  let mut index = ws.load_index();
  for (path, entry) in &changes {
    match entry {
      // Known to be this entry in the commit; whether the file is, the next look will say.
      Some(entry) => index.insert(path.clone(), Indexed { entry: *entry, size: 0, mtime: 0 }),
      None => index.remove(path),
    }
  }
  // A merge being concluded has the commit merged in as a second parent.
  let id = Commit { parents: ws.head.into_iter().chain(merged).collect(), rest, changes }.store(store)?;
  root_of(store, &id)?;
  store.flush()?;
  (ws.base, ws.head) = (Some(id), Some(id));
  ws.save()?;
  index.save()?;
  ws.log("commit", &id)?;
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
  let (found, _) = status(ws, Look::Everywhere)?;
  let from = match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  };
  let to = root_of(store, &target)?;
  let changes = removals_first(crate::manifest::diff(store, from, to)?);
  let in_the_way: Vec<String> = found
    .iter()
    .filter(|(path, _, _)| changes.iter().any(|(p, _)| p == path))
    .map(|(path, _, _)| String::from_utf8_lossy(path).into_owned())
    .collect();
  if !in_the_way.is_empty() {
    return Err(format!("Your local changes to the following files would be overwritten by checkout:\n\t{}", in_the_way.join("\n\t")));
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
  ws.head = Some(id);
  ws.save()?;
  index.save()?;
  ws.log("snapshot", &id)?;
  Ok(id)
}

/// Put back, from the commit the workspace is on, the files among `paths`
/// (or under them) that differ from it. Files the commit does not have are
/// left alone. Returns how many were put back.
pub fn restore(store: &mut Store, ws: &Workspace, paths: &[Vec<u8>]) -> Result<usize> {
  let under =
    |path: &[u8]| paths.iter().any(|p| p.is_empty() || p == path || (path.len() > p.len() && path.starts_with(p) && path[p.len()] == b'/'));
  let (found, _) = status(ws, Look::Everywhere)?;
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
      Some(entry) => index.insert(path, Indexed { entry, size: 0, mtime: 0 }),
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
  clone_tree(from, to).map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}
