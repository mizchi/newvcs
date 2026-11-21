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

  pub(crate) fn save(&self) -> Result<()> {
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

/// Write one file of a commit into a workspace, from the store's shared
/// copy, and say what the index should hold for it.
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
    return Ok(vouch(entry, size, mtime, now_nanos()));
  }
  let needs_bytes = entry.mode != MODE_GITLINK && (entry.mode == MODE_LINK || !cached.exists());
  let bytes = if needs_bytes { Some(blob(store, &entry.id)?) } else { None };
  let (size, mtime) = place_one(&files_dir, root, &Job { path: path.to_vec(), entry, bytes, direct: false })?;
  if is_attributes(path) {
    eols.forget();
  }
  Ok(vouch(entry, size, mtime, now_nanos()))
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
  eols: Eols,
  /// The directories the walk is inside, outermost first: how long each one's
  /// path is, and whether the rules ignore it. A directory that holds known
  /// files is entered whatever the rules say, so for those the rules are only
  /// asked when a file that is not known turns up.
  inside: Vec<(usize, Option<bool>)>,
  ws: &'a Workspace,
  index: &'a Index,
  seen: crate::index::Seen,
  looked: Looked,
  out: Vec<Difference>,
  refreshed: Refreshed,
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
          if known.is_some() || !(self.here_ignored(rel) || self.ignores.ignored(rel, false)) {
            compare(self.ws, rel, known.as_ref(), &mut self.looked, &mut self.out, &mut self.refreshed, &mut self.eols)?;
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
            self.dir(&dir.join(&name), rel)?;
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
  let mut index = ws.load_index();
  let (out, looked, refreshed) = if look == Look::Recorded {
    let (mut looked, mut out, mut refreshed) = (Looked::default(), Vec::new(), Refreshed::new());
    let mut eols = Eols::of_worktree(&ws.root);
    let mut rules = crate::ignore::Checker::new(&ws.root);
    for path in ws.recorded() {
      let known = index.get(&path);
      // A reported write to a file the rules ignore is not a file to track, as with a walk.
      if in_view(&ws.prefixes, &path, false) && (known.is_some() || !rules.ignored(&path)) {
        compare(ws, &path, known.as_ref(), &mut looked, &mut out, &mut refreshed, &mut eols)?;
      }
    }
    (out, looked, refreshed)
  } else {
    let mut walk = Walk {
      ignores: crate::ignore::Ignores::new(&ws.root),
      eols: Eols::of_worktree(&ws.root),
      inside: Vec::new(),
      ws,
      index: &index,
      seen: index.seen(),
      looked: Looked::default(),
      out: Vec::new(),
      refreshed: Vec::new(),
    };
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
pub fn commit(store: &mut Store, ws: &mut Workspace, author: &str, message: &str, look: Look) -> Result<Option<Id>> {
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
  let mut index = ws.load_index();
  let mut eols = Eols::of_worktree(&ws.root);
  for (path, entry) in crate::manifest::diff(store, from, to)? {
    if !in_view(&ws.prefixes, &path, false) {
      continue;
    }
    match entry {
      Some(entry) => {
        index.insert(path.clone(), place_with(store, &ws.root, &path, entry, &mut eols)?);
        done.written += 1;
      }
      None => {
        unplace(&ws.root, &path);
        index.remove(&path);
        done.removed += 1;
      }
    }
  }
  ws.head = Some(target);
  index.save()?;
  ws.save()?;
  ws.log("restore", &target)?;
  Ok(done)
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
  if !plain || name == "base" {
    return Err(format!("`{name}` cannot be a name: letters, digits, `-`, `_` and `.` only, and not `base`"));
  }
  snapshot(store, ws, author, look)?;
  let id = ws.head.ok_or("there is nothing to name yet")?;
  let file = mark_file(ws, name);
  std::fs::create_dir_all(file.parent().expect("under .nv")).map_err(|e| e.to_string())?;
  std::fs::write(&file, hex(&id)).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
  ws.log(&format!("mark:{name}"), &id)?;
  Ok(id)
}

/// A recorded state by what someone would call it: a name given with
/// `mark`, `base` for the last published commit, `~N` for the Nth snapshot
/// or commit before the latest, or the start of an id.
pub fn state_named(ws: &Workspace, spec: &str) -> Result<Id> {
  let parse = |text: &str| unhex(text.trim()).and_then(|b| Id::try_from(b).ok());
  if spec == "base" {
    return ws.base.ok_or_else(|| "nothing has been published yet".to_string());
  }
  if let Some(id) = std::fs::read_to_string(mark_file(ws, spec)).ok().and_then(|text| parse(&text)) {
    return Ok(id);
  }
  let logged: Vec<(String, String)> = ws.operations().into_iter().map(|(_, what, id)| (what, id)).collect();
  if let Some(back) = spec.strip_prefix('~').and_then(|n| n.parse::<usize>().ok()) {
    let states: Vec<&String> = logged.iter().filter(|(what, _)| what == "snapshot" || what == "commit").map(|(_, id)| id).collect();
    let at = states.len().checked_sub(back + 1).ok_or_else(|| format!("there are only {} recorded states", states.len()))?;
    return parse(states[at]).ok_or_else(|| "the log of operations is damaged".to_string());
  }
  let hexish = spec.len() >= 6 && spec.bytes().all(|b| b.is_ascii_hexdigit());
  let mut found: Vec<&String> = logged.iter().map(|(_, id)| id).filter(|id| hexish && id.starts_with(&spec.to_ascii_lowercase())).collect();
  found.sort();
  found.dedup();
  match found[..] {
    [one] => parse(one).ok_or_else(|| "the log of operations is damaged".to_string()),
    [] => Err(format!("`{spec}` is not a name given with `nv mark`, `base`, `~N`, or the start of a recorded state's id")),
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
  drop(Store::open(&meta.join("store"))?);
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
  index.save()?;
  ws.save()?;
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
  index.save()?;
  ws.save()?;
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
  let changes = crate::manifest::diff(store, from, to)?;
  let in_the_way: Vec<String> = found
    .iter()
    .filter(|(path, _, _)| changes.iter().any(|(p, _)| p == path))
    .map(|(path, _, _)| String::from_utf8_lossy(path).into_owned())
    .collect();
  if !in_the_way.is_empty() {
    return Err(format!("Your local changes to the following files would be overwritten by checkout:\n\t{}", in_the_way.join("\n\t")));
  }
  let mut index = ws.load_index();
  let mut eols = Eols::of_worktree(&ws.root);
  for (path, entry) in &changes {
    match entry {
      Some(entry) => index.insert(path.clone(), place_with(store, &ws.root, path, *entry, &mut eols)?),
      None => {
        unplace(&ws.root, path);
        index.remove(path);
      }
    }
  }
  (ws.base, ws.head) = (Some(target), Some(target));
  index.save()?;
  ws.save()
}

/// Make the workspace's files what `changes` say, on top of what it has.
/// Refused, with nothing touched, if one of those files was changed here.
pub fn apply_changes(store: &mut Store, ws: &Workspace, changes: &[Change]) -> Result<()> {
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
  let mut eols = Eols::of_worktree(&ws.root);
  for (path, entry) in changes {
    match entry {
      Some(entry) => index.insert(path.clone(), place_with(store, &ws.root, path, *entry, &mut eols)?),
      None => {
        unplace(&ws.root, path);
        index.remove(path);
      }
    }
  }
  index.save()
}

/// Put back, from the commit the workspace is on, the files among `paths`
/// (or under them) that differ from it. Files the commit does not have are
/// left alone. Returns how many were put back.
pub fn restore(store: &mut Store, ws: &Workspace, paths: &[Vec<u8>]) -> Result<usize> {
  let under =
    |path: &[u8]| paths.iter().any(|p| p.is_empty() || p == path || (path.len() > p.len() && path.starts_with(p) && path[p.len()] == b'/'));
  let (found, _) = status(ws, Look::Everywhere)?;
  let mut index = ws.load_index();
  let mut restored = 0;
  let mut eols = Eols::of_worktree(&ws.root);
  for (path, _, old) in found {
    if let Some(entry) = old.filter(|_| under(&path)) {
      index.insert(path.clone(), place_with(store, &ws.root, &path, entry, &mut eols)?);
      restored += 1;
    }
  }
  index.save()?;
  Ok(restored)
}

/// Say the workspace is on `target` without touching its files: what the
/// files then differ by is whatever lies between the two commits.
pub fn move_head(store: &mut Store, ws: &mut Workspace, target: Option<Id>) -> Result<()> {
  let root = |store: &mut Store, id: Option<Id>| match id {
    Some(id) => root_of(store, &id),
    None => Ok(None),
  };
  let (from, to) = (root(store, ws.head)?, root(store, target)?);
  let mut index = ws.load_index();
  for (path, entry) in crate::manifest::diff(store, from, to)? {
    match entry {
      // Known to be this entry in the commit; whether the file still is, is for the next look to say.
      Some(entry) => index.insert(path, Indexed { entry, size: 0, mtime: 0 }),
      None => index.remove(&path),
    }
  }
  (ws.base, ws.head) = (target, target);
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
  clone_tree(from, to).map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}
