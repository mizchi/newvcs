//! Git in, Git ids out.
//!
//! Import reads a Git repository through the `git` command: the history in
//! topological order with each commit's changes against its first parent,
//! and the blobs those changes name. Every object keeps Git's id next to its
//! own (`Store::map_git`), so nothing has to be hashed with SHA-1 on the way in.
//!
//! The way back is the proof that nothing was lost. A Git tree is written
//! from a manifest directory, a Git commit from the derived root tree, the
//! parents' Git ids and the bytes kept verbatim; the SHA-1 of each must be
//! the id Git gave the original.

use crate::commit::{Commit, root_of};
use crate::manifest::{Entry, MODE_GITLINK, load};
use crate::store::{GitOid, Kind, Store};
use crate::{Id, Result, hex, unhex};
use sha1::{Digest, Sha1};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn git(repo: &Path) -> Command {
  let mut c = Command::new("git");
  // A partial clone would otherwise fetch what it lacks, one object at a time.
  c.env("GIT_NO_LAZY_FETCH", "1").arg("-C").arg(repo);
  c
}

pub fn git_hash(kind: &str, bytes: &[u8]) -> GitOid {
  let mut h = Sha1::new();
  h.update(format!("{kind} {}\0", bytes.len()).as_bytes());
  h.update(bytes);
  h.finalize().into()
}

/// `git cat-file --batch`, kept open.
pub struct Objects {
  child: Child,
  input: ChildStdin,
  output: BufReader<ChildStdout>,
}

impl Objects {
  pub fn open(repo: &Path) -> Result<Objects> {
    let mut child = git(repo)
      .args(["cat-file", "--batch"])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .spawn()
      .map_err(|e| format!("cannot run git: {e}"))?;
    let (input, output) = (child.stdin.take().expect("piped"), BufReader::new(child.stdout.take().expect("piped")));
    Ok(Objects { child, input, output })
  }

  pub fn read(&mut self, oid: &str) -> Result<Vec<u8>> {
    let failed = |e: std::io::Error| format!("cannot read from git: {e}");
    writeln!(self.input, "{oid}").map_err(failed)?;
    let mut header = String::new();
    self.output.read_line(&mut header).map_err(failed)?;
    let size: usize =
      header.split_whitespace().nth(2).and_then(|s| s.parse().ok()).ok_or_else(|| format!("git does not have object {oid}"))?;
    let mut bytes = vec![0; size + 1];
    self.output.read_exact(&mut bytes).map_err(failed)?;
    bytes.pop();
    Ok(bytes)
  }
}

impl Drop for Objects {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

fn oid(hex_text: &str) -> Result<GitOid> {
  unhex(hex_text).and_then(|b| b.try_into().ok()).ok_or_else(|| format!("`{hex_text}` is not a git object id"))
}

fn padded(oid: GitOid) -> Id {
  let mut id = [0u8; 32];
  id[..20].copy_from_slice(&oid);
  id
}

/// Everything of a commit object after its `tree` and `parent` lines, and
/// the tree and parents those lines named.
fn split_commit(raw: &[u8]) -> Result<(GitOid, Vec<GitOid>, &[u8])> {
  let mut rest = raw;
  let mut tree = None;
  let mut parents = Vec::new();
  loop {
    let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
    let line = std::str::from_utf8(&rest[..end]).unwrap_or("");
    if let Some(t) = line.strip_prefix("tree ").filter(|_| tree.is_none() && parents.is_empty()) {
      tree = Some(oid(t)?);
    } else if let Some(p) = line.strip_prefix("parent ").filter(|_| tree.is_some()) {
      parents.push(oid(p)?);
    } else {
      break;
    }
    rest = &rest[(end + 1).min(rest.len())..];
  }
  Ok((tree.ok_or("a commit without a tree line")?, parents, rest))
}

/// A change as `git log --raw` prints it: the modes, ids and status, and the path.
type RawChange = (String, Vec<u8>);

#[derive(Default, Debug)]
pub struct Imported {
  pub commits: usize,
  pub blobs: usize,
  pub already: usize,
  /// Commits whose tree or commit object did not come back with Git's id.
  pub mismatches: Vec<String>,
}

/// Import the history reachable from `rev`, oldest first, checking as it
/// goes that each commit maps back to the id Git gave it.
pub fn import(store: &mut Store, repo: &Path, rev: &str, limit: Option<usize>) -> Result<Imported> {
  let mut log = git(repo);
  log.args([
    "log",
    "--reverse",
    "--topo-order",
    "--raw",
    "--no-renames",
    "--abbrev=40",
    "--diff-merges=first-parent",
    "--root",
    "-z",
    "--format=C %H",
  ]);
  log.arg(rev);
  let out = log.output().map_err(|e| format!("cannot run git: {e}"))?;
  if !out.status.success() {
    return Err(format!("git log failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
  }
  // `C <hash>`, then per change a `:modes ids status` record and a path record.
  let mut history: Vec<(String, Vec<RawChange>)> = Vec::new();
  let mut records = out.stdout.split(|&b| b == 0);
  while let Some(record) = records.next() {
    let record = match record.iter().position(|&b| b != b'\n') {
      Some(start) => &record[start..],
      None => continue,
    };
    if let Some(hash) = record.strip_prefix(b"C ") {
      history.push((String::from_utf8_lossy(hash).into_owned(), Vec::new()));
    } else if let Some(meta) = record.strip_prefix(b":") {
      let path = records.next().ok_or("a change without a path")?.to_vec();
      history.last_mut().ok_or("a change before any commit")?.1.push((String::from_utf8_lossy(meta).into_owned(), path));
    }
  }
  if let Some(limit) = limit {
    history.truncate(limit);
  }

  let mut objects = Objects::open(repo)?;
  let mut done = Imported::default();
  let mut memo = TreeMemo::default();
  // The last blob seen under each file name: a moved or copied file is a
  // new path to Git, and its old self is still the best base for a delta.
  let mut by_name: std::collections::HashMap<Vec<u8>, Id> = std::collections::HashMap::new();
  for (hash, raw_changes) in history {
    let commit_oid = oid(&hash)?;
    if store.of_git(&commit_oid).is_some() {
      done.already += 1;
      continue;
    }
    let mut changes = Vec::with_capacity(raw_changes.len());
    for (meta, path) in raw_changes {
      let f: Vec<&str> = meta.split(' ').collect();
      let [_, new_mode, old, new, status] = f[..] else { return Err(format!("unexpected change record `{meta}`")) };
      if status.starts_with('D') {
        changes.push((path, None));
        continue;
      }
      let mode = u32::from_str_radix(new_mode, 8).map_err(|_| format!("unexpected mode `{new_mode}`"))?;
      let new_oid = oid(new)?;
      let id = if mode == MODE_GITLINK {
        padded(new_oid)
      } else if let Some(id) = store.of_git(&new_oid) {
        id
      } else {
        let bytes = objects.read(new)?;
        // The version this one replaces is the best guess at something similar.
        let name = path.rsplit(|&b| b == b'/').next().unwrap_or(&path).to_vec();
        let base = oid(old).ok().and_then(|o| store.of_git(&o)).or_else(|| by_name.get(&name).copied());
        let id = store.put(Kind::Blob, &bytes, base)?;
        by_name.insert(name, id);
        store.map_git(Kind::Blob, id, new_oid)?;
        done.blobs += 1;
        id
      };
      changes.push((path, Some(Entry { mode, id })));
    }

    let raw = objects.read(&hash)?;
    let (tree_oid, parent_oids, rest) = split_commit(&raw)?;
    let mut parents = Vec::with_capacity(parent_oids.len());
    for p in &parent_oids {
      parents.push(store.of_git(p).ok_or_else(|| format!("parent {} of {hash} was not imported", hex(p)))?);
    }
    let id = Commit { parents, rest: rest.to_vec(), changes }.store(store)?;
    store.map_git(Kind::Commit, id, commit_oid)?;
    done.commits += 1;

    // The way back, at once: the derived tree and the rebuilt commit must have Git's ids.
    let root = root_of(store, &id)?;
    let tree = git_tree_of(store, root, &mut memo, &mut |_, _| Ok(()))?;
    if tree != tree_oid {
      done.mismatches.push(format!("{hash}: tree {} instead of {}", hex(&tree), hex(&tree_oid)));
    } else if git_hash("commit", &git_commit_bytes(&tree, &parent_oids, rest)) != commit_oid {
      done.mismatches.push(format!("{hash}: commit object differs"));
    }
  }
  store.checkpoint()?;
  Ok(done)
}

pub fn git_commit_bytes(tree: &GitOid, parents: &[GitOid], rest: &[u8]) -> Vec<u8> {
  let mut out = format!("tree {}\n", hex(tree)).into_bytes();
  for p in parents {
    out.extend_from_slice(format!("parent {}\n", hex(p)).as_bytes());
  }
  out.extend_from_slice(rest);
  out
}

/// Git tree ids already worked out in this run, by manifest directory.
#[derive(Default)]
pub struct TreeMemo {
  known: std::collections::HashMap<Id, GitOid>,
  /// Work every tree out again instead of taking the store's record of it:
  /// for checking the store against itself.
  from_scratch: bool,
}

/// Git's empty tree.
fn empty_tree() -> GitOid {
  git_hash("tree", b"")
}

/// The Git tree id of a manifest directory. `emit` is handed every Git
/// object that had to be built, for a caller that wants to write them out.
pub fn git_tree_of(
  store: &mut Store,
  root: Option<Id>,
  memo: &mut TreeMemo,
  emit: &mut dyn FnMut(&str, &[u8]) -> Result<()>,
) -> Result<GitOid> {
  let Some(id) = root else { return Ok(empty_tree()) };
  if let Some(known) = memo.known.get(&id) {
    return Ok(*known);
  }
  // A directory Git has been given before needs no rebuilding, and no writing out.
  if !memo.from_scratch
    && let Some(known) = store.git_of(&id)
  {
    return Ok(known);
  }
  let dir = load(store, &id)?;
  // Git orders a tree's entries as if directory names ended in a slash.
  let mut entries: Vec<(Vec<u8>, Entry)> = dir.into_iter().collect();
  entries.sort_by_cached_key(|(name, e)| {
    let mut key = name.clone();
    if e.is_dir() {
      key.push(b'/');
    }
    key
  });
  let mut bytes = Vec::new();
  for (name, entry) in entries {
    let oid: GitOid = if entry.is_dir() {
      git_tree_of(store, Some(entry.id), memo, emit)?
    } else if entry.mode == MODE_GITLINK {
      entry.id[..20].try_into().expect("twenty bytes")
    } else {
      git_blob_of(store, &entry.id, emit)?
    };
    bytes.extend_from_slice(format!("{:o} ", entry.mode).as_bytes());
    bytes.extend_from_slice(&name);
    bytes.push(0);
    bytes.extend_from_slice(&oid);
  }
  let oid = git_hash("tree", &bytes);
  emit("tree", &bytes)?;
  memo.known.insert(id, oid);
  if !memo.from_scratch {
    store.map_git(Kind::Tree, id, oid)?;
  }
  Ok(oid)
}

/// Git's id of a blob: known from import, or hashed now and remembered.
fn git_blob_of(store: &mut Store, id: &Id, emit: &mut dyn FnMut(&str, &[u8]) -> Result<()>) -> Result<GitOid> {
  if let Some(oid) = store.git_of(id) {
    return Ok(oid);
  }
  let bytes = store.get(id)?.ok_or_else(|| format!("blob {} is not in the store", hex(id)))?;
  let oid = git_hash("blob", &bytes);
  emit("blob", &bytes)?;
  store.map_git(Kind::Blob, *id, oid)?;
  Ok(oid)
}

/// Git's id of a commit, with every Git object it needs that Git has not
/// been told of handed to `emit`: parents first.
pub fn git_commit_of(store: &mut Store, id: &Id, memo: &mut TreeMemo, emit: &mut dyn FnMut(&str, &[u8]) -> Result<()>) -> Result<GitOid> {
  if let Some(oid) = store.git_of(id) {
    return Ok(oid);
  }
  let commit = Commit::load(store, id)?;
  let mut parents = Vec::with_capacity(commit.parents.len());
  for p in &commit.parents {
    parents.push(git_commit_of(store, p, memo, emit)?);
  }
  let root = root_of(store, id)?;
  let tree = git_tree_of(store, root, memo, emit)?;
  let bytes = git_commit_bytes(&tree, &parents, &commit.rest);
  let oid = git_hash("commit", &bytes);
  emit("commit", &bytes)?;
  store.map_git(Kind::Commit, *id, oid)?;
  Ok(oid)
}

/// Recompute the Git id of every imported commit from the store alone.
/// Returns `(commits checked, the ones that differ)`.
pub fn verify(store: &mut Store) -> Result<(usize, Vec<String>)> {
  let commits = store.with_git_ids();
  let mut memo = TreeMemo { from_scratch: true, ..TreeMemo::default() };
  let (mut checked, mut wrong) = (0, Vec::new());
  for (id, expected) in commits {
    // The map holds blobs too; only commits decode as commits with a root.
    let Some(bytes) = store.get(&id)? else { continue };
    if crate::store::object_id(Kind::Commit, &bytes) != id {
      continue;
    }
    let commit = Commit::decode(&bytes)?;
    let mut parents = Vec::new();
    for p in &commit.parents {
      parents.push(store.git_of(p).ok_or("a parent without a git id")?);
    }
    let root = root_of(store, &id)?;
    let tree = git_tree_of(store, root, &mut memo, &mut |_, _| Ok(()))?;
    checked += 1;
    if git_hash("commit", &git_commit_bytes(&tree, &parents, &commit.rest)) != expected {
      wrong.push(hex(&expected));
    }
  }
  Ok((checked, wrong))
}

/// Write a commit and what it needs into a Git repository, and point a ref at it.
pub fn export(store: &mut Store, id: &Id, repo: &Path, refname: &str) -> Result<(GitOid, usize)> {
  let mut written = 0;
  let mut memo = TreeMemo::default();
  let mut emit = |kind: &str, bytes: &[u8]| -> Result<()> {
    let mut child = git(repo)
      .args(["hash-object", "-w", "-t", kind, "--stdin"])
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .spawn()
      .map_err(|e| format!("cannot run git: {e}"))?;
    child.stdin.take().expect("piped").write_all(bytes).map_err(|e| e.to_string())?;
    if !child.wait().map_err(|e| e.to_string())?.success() {
      return Err(format!("git refused a {kind} object"));
    }
    written += 1;
    Ok(())
  };
  let oid = git_commit_of(store, id, &mut memo, &mut emit)?;
  let status = git(repo).args(["update-ref", refname, &hex(&oid)]).status().map_err(|e| e.to_string())?;
  if !status.success() {
    return Err(format!("git could not set {refname}: is every object of {} in that repository?", hex(&oid)));
  }
  Ok((oid, written))
}
