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

/// The real Git, for a repository. When nv is itself standing in for `git`
/// on `PATH` (tools/git-shim), `NV_REAL_GIT` says where the real one is.
pub fn git(repo: &Path) -> Command {
  let mut c = Command::new(std::env::var_os("NV_REAL_GIT").unwrap_or_else(|| "git".into()));
  // A partial clone would otherwise fetch what it lacks, one object at a time.
  c.env("GIT_NO_LAZY_FETCH", "1").arg("-C").arg(repo);
  // The repository is the one named. Inside a hook, or a command a rebase runs, the environment
  // names another repository, index or work tree, and Git would obey it over `-C`.
  for name in [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
  ] {
    c.env_remove(name);
  }
  c
}

/// A value of Git's configuration for the repository in `root`: its own
/// `.git/config`, then the user's. Read from the files; no process is
/// started. Includes are not followed.
pub fn config_value(root: &Path, section: &str, key: &str) -> Option<String> {
  let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
  let global = match std::env::var_os("GIT_CONFIG_GLOBAL") {
    Some(file) => vec![std::path::PathBuf::from(file)],
    None => {
      let xdg = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".config")));
      home.iter().map(|h| h.join(".gitconfig")).chain(xdg.map(|x| x.join("git").join("config"))).collect()
    }
  };
  std::iter::once(root.join(".git").join("config")).chain(global).find_map(|file| {
    let text = std::fs::read_to_string(file).ok()?;
    let mut inside = false;
    let mut found = None;
    for line in text.lines().map(str::trim) {
      if let Some(header) = line.strip_prefix('[') {
        inside = header.trim_end_matches(']').trim().eq_ignore_ascii_case(section);
      } else if inside
        && let Some((name, value)) = line.split_once('=')
        && name.trim().eq_ignore_ascii_case(key)
      {
        found = Some(value.trim().trim_matches('"').to_string());
      }
    }
    found
  })
}

/// A name or an address as Git puts it in a commit (`strbuf_addstr_without_crud`): what Git calls
/// crud taken off both ends, and `<`, `>` and line ends taken out, so that what the environment or
/// the configuration says can never make a header Git's fsck, or a server, refuses (third pass).
pub fn ident_part(text: &str) -> String {
  let crud = |c: char| c as u32 <= 32 || matches!(c, ',' | ':' | ';' | '<' | '>' | '"' | '\\' | '\'');
  text.trim_matches(crud).chars().filter(|c| !matches!(c, '\n' | '<' | '>')).collect()
}

/// A date as Git takes one from `GIT_AUTHOR_DATE` and the like, written as a commit has it
/// (`seconds zone`): seconds since 1970 (nine digits or more, before 2100, or any after an `@`),
/// or ISO 8601, with a zone (`+hhmm` of under 24 hours and 60 minutes) or this machine's where
/// none is given or the one given is none. `None` where Git refuses the date (fourth pass: `0`
/// and `-5` were taken, and a commit Git's fsck refuses pushed; `+9999` was kept).
pub fn stamp(date: &str) -> Option<String> {
  let date = date.trim();
  let zone_of = |zone: &str| -> Option<String> {
    let bytes = zone.as_bytes();
    if bytes.len() != 5 || !(bytes[0] == b'+' || bytes[0] == b'-') || !bytes[1..].iter().all(u8::is_ascii_digit) {
      return None;
    }
    let (hours, minutes): (u32, u32) = (zone[1..3].parse().ok()?, zone[3..5].parse().ok()?);
    (hours < 24 && minutes < 60).then(|| zone.to_string())
  };
  let mut words = date.split_whitespace();
  let first = words.next()?;
  let seconds = match first.strip_prefix('@') {
    Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => digits.parse::<i64>().ok(),
    None if first.bytes().all(|b| b.is_ascii_digit()) => first.parse::<i64>().ok().filter(|s| (100_000_000..4_102_444_800).contains(s)),
    _ => None,
  };
  if first.bytes().all(|b| b.is_ascii_digit()) || first.starts_with('@') {
    let seconds = seconds?;
    let zone = words.next().and_then(zone_of).unwrap_or_else(|| local_zone(seconds));
    return Some(format!("{seconds} {zone}"));
  }
  let (day, rest) = date.split_at_checked(10)?;
  let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
  let (time, zone) = rest.split_at_checked(8)?;
  let number = |text: &str| text.parse::<i64>().ok();
  let (year, month, date_of) = (number(day.get(..4)?)?, number(day.get(5..7)?)?, number(day.get(8..10)?)?);
  let (hour, minute, second) = (number(time.get(..2)?)?, number(time.get(3..5)?)?, number(time.get(6..8)?)?);
  if !(1..=12).contains(&month) || !(1..=31).contains(&date_of) || hour > 23 || minute > 59 || second > 60 {
    return None;
  }
  // Days since the epoch, by the civil calendar.
  let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
  let era = y.div_euclid(400);
  let of_era = y.rem_euclid(400);
  let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + (153 * m + 2) / 5 + date_of - 1 - 719_468;
  let wall = days * 86_400 + hour * 3600 + minute * 60 + second;
  let zone = match zone.trim() {
    "Z" => "+0000".to_string(),
    "" => local_zone(wall - offset_of(&local_zone(wall))),
    zone => zone_of(&zone.replace(':', ""))?,
  };
  Some(format!("{} {zone}", wall - offset_of(&zone)))
}

/// Seconds east of UTC that a zone written `+hhmm` is.
fn offset_of(zone: &str) -> i64 {
  let number = |text: &str| text.parse::<i64>().unwrap_or(0);
  (number(&zone[1..3]) * 3600 + number(&zone[3..5]) * 60) * if zone.starts_with('-') { -1 } else { 1 }
}

/// This machine's zone at a moment, as `+hhmm`.
pub fn local_zone(seconds: i64) -> String {
  // SAFETY: `localtime_r` writes into the struct it is handed and nothing else.
  let east = unsafe {
    let mut tm: libc::tm = std::mem::zeroed();
    let at: libc::time_t = seconds as libc::time_t;
    libc::localtime_r(&at, &mut tm);
    tm.tm_gmtoff
  };
  let (sign, east) = if east < 0 { ('-', -east) } else { ('+', east) };
  format!("{sign}{:02}{:02}", east / 3600, east / 60 % 60)
}

/// What the environment says of a commit's author or committer (`role`): a variable set is taken
/// even when empty, as Git takes it (fourth pass: an empty one was read as unset).
pub fn ident_env(role: &str, key: &str) -> Option<String> {
  std::env::var(format!("GIT_{role}_{key}")).ok()
}

/// The file of patterns Git ignores in every repository of this user's.
pub fn global_excludes(root: &Path) -> Option<std::path::PathBuf> {
  let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
  match config_value(root, "core", "excludesfile") {
    Some(path) => Some(match (path.strip_prefix("~/"), &home) {
      (Some(rest), Some(home)) => home.join(rest),
      _ => path.into(),
    }),
    None => {
      let xdg = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| home.map(|h| h.join(".config")))?;
      Some(xdg.join("git").join("ignore"))
    }
  }
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

/// A commit as `git log --raw` gives it: its id, its parents' ids, and what it changed.
type Logged = (String, Vec<String>, Vec<RawChange>);

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
  import_beyond(store, repo, rev, limit, &[])
}

/// `import`, of the commits `rev` has that the commits `have` do not: for a
/// store that is known to hold those already. Git is then asked about the
/// new commits only, however long the history behind them.
pub fn import_beyond(store: &mut Store, repo: &Path, rev: &str, limit: Option<usize>, have: &[String]) -> Result<Imported> {
  import_tips(store, repo, &[rev], limit, have)
}

/// `import_beyond`, of everything behind several commits at once: one question to Git for all
/// of them, where a repository's hundreds of tags would otherwise each ask for the whole history.
pub fn import_tips(store: &mut Store, repo: &Path, revs: &[&str], limit: Option<usize>, have: &[String]) -> Result<Imported> {
  // `C <hash> <parents>`, then per change a `:modes ids status` record and a path record.
  let logged = |tail: &[String]| -> Result<Vec<Logged>> {
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
      "--format=C %H %P",
    ]);
    log.args(revs).args(tail);
    let out = log.output().map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
      return Err(format!("git log failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut history: Vec<Logged> = Vec::new();
    let mut records = out.stdout.split(|&b| b == 0);
    while let Some(record) = records.next() {
      let record = match record.iter().position(|&b| b != b'\n') {
        Some(start) => &record[start..],
        None => continue,
      };
      if let Some(line) = record.strip_prefix(b"C ") {
        let line = String::from_utf8_lossy(line);
        let mut ids = line.split_whitespace().map(str::to_string);
        let hash = ids.next().ok_or("a commit without its id")?;
        history.push((hash, ids.collect(), Vec::new()));
      } else if let Some(meta) = record.strip_prefix(b":") {
        let path = records.next().ok_or("a change without a path")?.to_vec();
        history.last_mut().ok_or("a change before any commit")?.2.push((String::from_utf8_lossy(meta).into_owned(), path));
      }
    }
    Ok(history)
  };
  // One commit, made on the very commit already had: by far the usual case, a commit just made.
  // Asked for by itself, its parents with it. Asked for as "everything behind it that is not
  // behind the other", Git sorts history to answer, and without a commit-graph that is a walk of
  // all of it; that is asked only when the commit stands on something else.
  let alone = match (revs, have) {
    ([_], [had]) => Some(logged(&["--no-walk".to_string()])?).filter(|one| one.len() == 1 && one[0].1 == [had.clone()]),
    _ => None,
  };
  let history: Vec<(String, Vec<RawChange>)> = match alone {
    Some(one) => one,
    None => logged(&have.iter().map(|oid| format!("^{oid}")).collect::<Vec<_>>())?,
  }
  .into_iter()
  .map(|(hash, _, changes)| (hash, changes))
  .collect();
  let mut history = history;
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

    // The way back, before the commit is stored: the tree its changes give must have Git's id.
    let parent_root = match parents.first() {
      Some(parent) => root_of(store, parent)?,
      None => None,
    };
    let mut root = crate::manifest::apply(store, parent_root, &changes)?;
    let mut tree = git_tree_of(store, root, &mut memo, &mut |_, _| Ok(()))?;
    if tree != tree_oid {
      // `git log --raw` reports modes as Git would write them today. Trees
      // from Git's first weeks hold others (100664), and the id is over what
      // they hold. The tree objects themselves say.
      let mut fixes = Vec::new();
      true_modes(store, &mut objects, &tree_oid, root, &mut Vec::new(), &mut memo, &mut fixes)?;
      if !fixes.is_empty() {
        for (path, entry) in fixes {
          match changes.iter_mut().find(|(p, _)| *p == path) {
            Some(change) => change.1 = entry,
            None => changes.push((path, entry)),
          }
        }
        root = crate::manifest::apply(store, parent_root, &changes)?;
        tree = git_tree_of(store, root, &mut memo, &mut |_, _| Ok(()))?;
      }
    }
    let id = Commit { parents, rest: rest.to_vec(), changes }.store(store)?;
    store.set_root(id, root)?;
    store.map_git(Kind::Commit, id, commit_oid)?;
    done.commits += 1;
    if tree != tree_oid {
      done.mismatches.push(format!("{hash}: tree {} instead of {}", hex(&tree), hex(&tree_oid)));
    } else if git_hash("commit", &git_commit_bytes(&tree, &parent_oids, rest)) != commit_oid {
      done.mismatches.push(format!("{hash}: commit object differs"));
    }
  }
  // A history's worth of new objects is worth a new index of them at once; a few commits' worth is not.
  if done.commits + done.blobs >= 4096 {
    store.checkpoint()?;
  } else {
    store.flush()?;
  }
  Ok(done)
}

/// Where a Git tree object and the manifest directory meant to equal it
/// disagree on a file's mode only, the mode Git has: as changes to apply.
/// Directories whose Git ids already agree are not opened.
fn true_modes(
  store: &mut Store,
  objects: &mut Objects,
  git_tree: &GitOid,
  dir: Option<Id>,
  at: &mut Vec<u8>,
  memo: &mut TreeMemo,
  out: &mut Vec<crate::manifest::Change>,
) -> Result<()> {
  let Some(dir) = dir else { return Ok(()) };
  let ours = load(store, &dir)?;
  let raw = objects.read(&hex(git_tree))?;
  let mut rest = raw.as_slice();
  while !rest.is_empty() {
    let bad = || "a git tree object is cut short".to_string();
    let space = rest.iter().position(|&b| b == b' ').ok_or_else(bad)?;
    let nul = rest.iter().position(|&b| b == 0).ok_or_else(bad)?;
    let mode = std::str::from_utf8(&rest[..space]).ok().and_then(|m| u32::from_str_radix(m, 8).ok()).ok_or_else(bad)?;
    let name = &rest[space + 1..nul];
    let oid: GitOid = rest.get(nul + 1..nul + 21).ok_or_else(bad)?.try_into().unwrap();
    rest = &rest[nul + 21..];
    let Some(entry) = ours.get(name) else { continue };
    let len = at.len();
    if len > 0 {
      at.push(b'/');
    }
    at.extend_from_slice(name);
    if entry.is_dir() {
      if mode == crate::manifest::MODE_DIR && git_tree_of(store, Some(entry.id), memo, &mut |_, _| Ok(()))? != oid {
        true_modes(store, objects, &oid, Some(entry.id), at, memo, out)?;
      }
    } else if entry.mode != mode && mode != crate::manifest::MODE_DIR {
      out.push((at.clone(), Some(Entry { mode, id: entry.id })));
    }
    at.truncate(len);
  }
  Ok(())
}

/// An annotated tag as stored: what it names, and Git's tag object after its `object` line.
pub fn tag_parts(bytes: &[u8]) -> Result<(Id, &[u8])> {
  let (target, rest) = bytes.split_at_checked(32).ok_or("a tag object is cut short")?;
  Ok((Id::try_from(target).map_err(|_| "a tag object is cut short")?, rest))
}

pub fn git_tag_bytes(target: &GitOid, rest: &[u8]) -> Vec<u8> {
  [format!("object {}\n", hex(target)).as_bytes(), rest].concat()
}

/// Store an annotated tag of `target`; `rest` is Git's tag object after its first line.
pub fn put_tag(store: &mut Store, target: &Id, rest: &[u8]) -> Result<Id> {
  store.put(Kind::Tag, &[target.as_slice(), rest].concat(), None)
}

/// Git's id for a commit or an annotated tag.
pub fn git_id_of(store: &mut Store, id: &Id) -> Result<GitOid> {
  if store.kind_of(id)? != Some(Kind::Tag) {
    return git_commit_of(store, id, &mut TreeMemo::default(), &mut |_, _| Ok(()));
  }
  if let Some(oid) = store.git_of(id) {
    return Ok(oid);
  }
  let bytes = store.get(id)?.ok_or("a tag is missing from the store")?;
  let (target, rest) = tag_parts(&bytes)?;
  let oid = git_hash("tag", &git_tag_bytes(&git_id_of(store, &target)?, rest));
  store.map_git(Kind::Tag, *id, oid)?;
  Ok(oid)
}

/// The commit a ref's object stands for: itself, or what a tag names.
pub fn peel(store: &mut Store, id: Id) -> Result<Id> {
  let mut at = id;
  while store.kind_of(&at)? == Some(Kind::Tag) {
    let bytes = store.get(&at)?.ok_or("a tag is missing from the store")?;
    at = tag_parts(&bytes)?.0;
  }
  Ok(at)
}

/// What a Git repository's refs point at, after importing the history behind each.
pub struct Refs {
  /// `(name, commit)` for branches; for tags, the commit or the annotated tag's own object.
  pub heads: Vec<(String, Id)>,
  pub tags: Vec<(String, Id)>,
  /// The branch Git's HEAD is on, if it is on one.
  pub current: Option<String>,
}

/// Import everything a Git repository's branches and tags reach.
pub fn import_refs(store: &mut Store, repo: &Path) -> Result<Refs> {
  let run = |args: &[&str]| -> Result<String> {
    let out = git(repo).args(args).output().map_err(|e| format!("cannot run git: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
  };
  if run(&["rev-parse", "--git-dir"])?.trim().is_empty() {
    return Err(format!("{} is not a git repository", repo.display()));
  }
  let mut refs = Refs { heads: Vec::new(), tags: Vec::new(), current: None };
  let current = run(&["symbolic-ref", "-q", "--short", "HEAD"])?;
  refs.current = Some(current.trim().to_string()).filter(|c| !c.is_empty());
  let listed = run(&["for-each-ref", "--format=%(refname) %(objectname) %(*objectname)", "refs/heads", "refs/tags"])?;
  // The commits the refs name (through an annotated tag, the commit it names), all asked for at once.
  let named = |line: &str| -> Option<String> {
    let mut fields = line.split(' ').skip(1);
    let object = fields.next()?;
    Some(fields.next().filter(|peeled| !peeled.is_empty()).unwrap_or(object).to_string())
  };
  let mut tips: Vec<String> = listed.lines().filter_map(named).collect();
  tips.sort();
  tips.dedup();
  tips.retain(|tip| oid(tip).is_ok_and(|tip| store.of_git(&tip).is_none()));
  if !tips.is_empty() {
    import_tips(store, repo, &tips.iter().map(String::as_str).collect::<Vec<_>>(), None, &[])?;
  }
  for line in listed.lines() {
    let mut fields = line.split(' ');
    let (Some(name), Some(object)) = (fields.next(), fields.next()) else { continue };
    // The commit an annotated tag names, if this is one.
    let commit = fields.next().filter(|peeled| !peeled.is_empty()).unwrap_or(object);
    let Some(mut id) = store.of_git(&oid(commit)?) else { continue };
    if commit != object {
      // An annotated tag is an object of its own, kept as Git has it.
      let out = git(repo).args(["cat-file", "tag", object]).output().map_err(|e| format!("cannot run git: {e}"))?;
      let line = out.stdout.iter().position(|&b| b == b'\n').map_or(0, |at| at + 1);
      // A tag of a tag is taken as a tag of the commit.
      if out.status.success() && out.stdout.starts_with(format!("object {commit}\n").as_bytes()) {
        id = put_tag(store, &id, &out.stdout[line..])?;
        store.map_git(Kind::Tag, id, oid(object)?)?;
      }
    }
    if let Some(branch) = name.strip_prefix("refs/heads/") {
      refs.heads.push((branch.to_string(), id));
    } else if let Some(tag) = name.strip_prefix("refs/tags/") {
      refs.tags.push((tag.to_string(), id));
    }
  }
  Ok(refs)
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
pub fn git_blob_of(store: &mut Store, id: &Id, emit: &mut dyn FnMut(&str, &[u8]) -> Result<()>) -> Result<GitOid> {
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
    // The map holds blobs and trees too.
    if store.kind_of(&id)? != Some(Kind::Commit) {
      continue;
    }
    let Some(bytes) = store.get(&id)? else { continue };
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

/// Asks a Git repository which objects it has: `git cat-file --batch-check`, kept open.
struct Has {
  child: Child,
  input: ChildStdin,
  output: BufReader<ChildStdout>,
}

impl Has {
  fn open(repo: &Path) -> Result<Has> {
    let mut child = git(repo)
      .args(["cat-file", "--batch-check"])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .spawn()
      .map_err(|e| format!("cannot run git: {e}"))?;
    let (input, output) = (child.stdin.take().expect("piped"), BufReader::new(child.stdout.take().expect("piped")));
    Ok(Has { child, input, output })
  }

  fn object(&mut self, oid: &GitOid) -> Result<bool> {
    let failed = |e: std::io::Error| format!("cannot ask git: {e}");
    writeln!(self.input, "{}", hex(oid)).map_err(failed)?;
    let mut answer = String::new();
    self.output.read_line(&mut answer).map_err(failed)?;
    Ok(!answer.trim_end().ends_with("missing"))
  }
}

impl Drop for Has {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

/// Where a repository keeps its loose objects, when that is where one would guess.
fn objects_dir(repo: &Path) -> Option<std::path::PathBuf> {
  [repo.join(".git").join("objects"), repo.join("objects")]
    .into_iter()
    .find(|dir| dir.is_dir() && dir.parent().is_some_and(|git| git.join("HEAD").is_file()))
}

/// A loose object, written as Git writes one: no process is started for it.
fn write_loose(dir: &Path, kind: &str, bytes: &[u8]) -> std::io::Result<()> {
  use std::os::unix::fs::PermissionsExt;
  let name = hex(&git_hash(kind, bytes));
  let (fan, path) = (dir.join(&name[..2]), dir.join(&name[..2]).join(&name[2..]));
  if path.exists() {
    return Ok(());
  }
  std::fs::create_dir_all(&fan)?;
  let mut packed = flate2::write::ZlibEncoder::new(Vec::with_capacity(bytes.len() / 2 + 64), flate2::Compression::fast());
  packed.write_all(format!("{kind} {}\0", bytes.len()).as_bytes())?;
  packed.write_all(bytes)?;
  // Under another name first: Git must never find half an object under its id.
  let temp = fan.join(format!("tmp_obj_nv{}", std::process::id()));
  std::fs::write(&temp, packed.finish()?)?;
  std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o444))?;
  std::fs::rename(&temp, &path)
}

fn write_object(repo: &Path, kind: &str, bytes: &[u8]) -> Result<()> {
  if let Some(dir) = objects_dir(repo) {
    return write_loose(&dir, kind, bytes).map_err(|e| format!("cannot write a {kind} object into {}: {e}", dir.display()));
  }
  let mut child = git(repo)
    .args(["hash-object", "-w", "-t", kind, "--stdin"])
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .spawn()
    .map_err(|e| format!("cannot run git: {e}"))?;
  child.stdin.take().expect("piped").write_all(bytes).map_err(|e| e.to_string())?;
  if child.wait().map_err(|e| e.to_string())?.success() { Ok(()) } else { Err(format!("git refused a {kind} object")) }
}

/// Write a directory into a Git repository as a tree, with the trees and
/// blobs under it that the repository lacks. A tree it has is not opened:
/// having a tree means having everything below it.
fn export_tree(store: &mut Store, repo: &Path, has: &mut Has, dir: Option<Id>, memo: &mut TreeMemo, written: &mut usize) -> Result<GitOid> {
  let oid = git_tree_of(store, dir, memo, &mut |_, _| Ok(()))?;
  // Git answers that it has the empty tree whether or not the object is there, and `git fsck`
  // then finds it missing: that one is written without asking.
  if oid != empty_tree() && has.object(&oid)? {
    return Ok(oid);
  }
  let mut entries: Vec<(Vec<u8>, Entry)> = match dir {
    Some(dir) => load(store, &dir)?.into_iter().collect(),
    None => Vec::new(),
  };
  entries.sort_by_cached_key(|(name, e)| [name.as_slice(), if e.is_dir() { b"/" } else { b"" }].concat());
  let mut bytes = Vec::new();
  for (name, entry) in entries {
    let child: GitOid = if entry.is_dir() {
      export_tree(store, repo, has, Some(entry.id), memo, written)?
    } else if entry.mode == MODE_GITLINK {
      entry.id[..20].try_into().expect("twenty bytes")
    } else {
      let blob = git_blob_of(store, &entry.id, &mut |_, _| Ok(()))?;
      if !has.object(&blob)? {
        let content = store.get(&entry.id)?.ok_or_else(|| format!("blob {} is not in the store", hex(&entry.id)))?;
        write_object(repo, "blob", &content)?;
        *written += 1;
      }
      blob
    };
    bytes.extend_from_slice(format!("{:o} ", entry.mode).as_bytes());
    bytes.extend_from_slice(&name);
    bytes.push(0);
    bytes.extend_from_slice(&child);
  }
  write_object(repo, "tree", &bytes)?;
  *written += 1;
  Ok(oid)
}

/// Write a commit into a Git repository, with every commit, tree and blob
/// behind it that the repository lacks, and point a ref at it. What the
/// repository has is asked of the repository, not assumed: a commit made
/// here has a Git id from the moment it is printed, and is in no Git
/// repository until it is written to one.
pub fn export(store: &mut Store, id: &Id, repo: &Path, refname: &str) -> Result<(GitOid, usize)> {
  let mut has = Has::open(repo)?;
  let mut memo = TreeMemo::default();
  let mut written = 0;
  // The commits to write, found walking back until the repository has one: parents before children.
  let mut missing: Vec<Id> = Vec::new();
  let mut seen = std::collections::HashSet::new();
  let mut todo = vec![*id];
  while let Some(at) = todo.pop() {
    if !seen.insert(at) {
      continue;
    }
    let oid = git_commit_of(store, &at, &mut memo, &mut |_, _| Ok(()))?;
    if has.object(&oid)? {
      continue;
    }
    missing.push(at);
    todo.extend(Commit::load(store, &at)?.parents);
  }
  // Reached later means older, or beside: written last to first, a commit's parents are there before it.
  for at in missing.iter().rev() {
    let commit = Commit::load(store, at)?;
    let root = root_of(store, at)?;
    let tree = export_tree(store, repo, &mut has, root, &mut memo, &mut written)?;
    let mut parents = Vec::with_capacity(commit.parents.len());
    for p in &commit.parents {
      parents.push(git_commit_of(store, p, &mut memo, &mut |_, _| Ok(()))?);
    }
    write_object(repo, "commit", &git_commit_bytes(&tree, &parents, &commit.rest))?;
    written += 1;
  }
  let oid = git_commit_of(store, id, &mut memo, &mut |_, _| Ok(()))?;
  let status = git(repo).args(["update-ref", refname, &hex(&oid)]).status().map_err(|e| e.to_string())?;
  if !status.success() {
    return Err(format!("git could not set {refname} to {}", hex(&oid)));
  }
  Ok((oid, written))
}

#[cfg(test)]
mod ident_tests {
  use super::ident_part;

  #[test]
  fn names_and_addresses_are_cleaned_as_git_cleans_them() {
    assert_eq!(ident_part("A <b>"), "A b");
    assert_eq!(ident_part("A>"), "A");
    assert_eq!(ident_part("<a@b>"), "a@b");
    assert_eq!(ident_part(" A\nB, "), "AB");
    assert_eq!(ident_part("\"a@b\";"), "a@b");
    assert_eq!(ident_part("J. R. Writer."), "J. R. Writer.");
  }
}
