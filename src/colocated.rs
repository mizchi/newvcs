//! A directory that is a Git repository and an nv workspace at once.
//!
//! There Git is Git. Its commands, index, configuration and hooks are its
//! own: `nv git` in such a directory runs Git, and nothing of nv's stands in
//! Git's way. A hook that stopped Git from moving a branch would stop it
//! after it had rewritten the files, which is worse than letting it move.
//!
//! So nv follows. It writes down the state in which the two last agreed
//! (`.nv/git-synced`), and a command of its own that finds Git elsewhere
//! takes Git's commits in, with their ids, before it does anything. Its
//! snapshots are nobody's business but the workspace's and never reach Git;
//! `nv commit` publishes them by having Git make the commit, from the files,
//! so that what is excluded, filtered, signed or refused is so by Git's
//! rules and nobody else's.
//!
//! The one way nv moves first is by catching up with a server (`nv sync`).
//! Git is then brought along (`mirror`).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::commit::{Commit, root_of};
use crate::git::{git, git_id_of};
use crate::store::Store;
use crate::workspace::{self, Look, Workspace};
use crate::{Id, Result, hex, unhex};

/// What `eval \"$(nv shell-init)\"` defines, for bash and zsh.
pub const SHELL_INIT: &str = r#"# Inside an nv workspace, `git` is nv. Elsewhere, and with NV_GIT=raw, it is Git.
git() {
  if [ "${NV_GIT:-}" != raw ]; then
    _nv_dir=$PWD
    while :; do
      if [ -f "$_nv_dir/.nv/workspace" ]; then
        command nv git "$@"
        _nv_status=$?
        if [ "$_nv_status" -eq 125 ]; then
          echo "nv: to run the real Git here all the same: NV_GIT=raw git $*" >&2
        fi
        unset _nv_dir
        return "$_nv_status"
      fi
      if [ -e "$_nv_dir/.git" ] || [ -z "$_nv_dir" ]; then
        break
      fi
      _nv_dir=${_nv_dir%/*}
    done
    unset _nv_dir
  fi
  command git "$@"
}
"#;

/// Where the two were when they last agreed: Git's commit, the branch, nv's commit.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct Point {
  git: Option<String>,
  branch: Option<String>,
  nv: Option<Id>,
}

fn meta(root: &Path, name: &str) -> PathBuf {
  root.join(".nv").join(name)
}

/// Whether a Git repository has its directory here.
/// Whether what Git is configured to do with files applies here: Git shares the directory, or
/// the workspace is one `nv git` answers for, where the user's own configuration is Git's to them.
pub fn as_git(root: &Path) -> bool {
  shared(root) || root.join(".nv").join("git-head").exists()
}

pub fn shared(root: &Path) -> bool {
  root.join(".git").is_dir()
}

fn read_point(root: &Path) -> Option<Point> {
  let text = std::fs::read_to_string(meta(root, "git-synced")).ok()?;
  let mut fields = text.split_whitespace().map(|f| Some(f).filter(|f| *f != "-"));
  Some(Point {
    git: fields.next()?.map(str::to_string),
    branch: fields.next()?.map(str::to_string),
    nv: fields.next()?.and_then(unhex).and_then(|b| Id::try_from(b).ok()),
  })
}

fn write_point(root: &Path, point: &Point) -> Result<()> {
  let or_dash = |field: Option<String>| field.unwrap_or_else(|| "-".into());
  let text = format!("{} {} {}\n", or_dash(point.git.clone()), or_dash(point.branch.clone()), or_dash(point.nv.map(|id| hex(&id))));
  crate::write_whole(&meta(root, "git-synced"), text)
}

/// Git's branch and commit, read from its files: no process is started.
/// The branch is `None` when Git is on no branch.
fn git_head(root: &Path) -> (Option<String>, Option<String>) {
  let dir = root.join(".git");
  let head = std::fs::read_to_string(dir.join("HEAD")).unwrap_or_default();
  let Some(name) = head.trim().strip_prefix("ref: ") else {
    return (None, Some(head.trim().to_string()).filter(|h| h.len() == 40));
  };
  let branch = name.strip_prefix("refs/heads/").map(str::to_string);
  if let Ok(oid) = std::fs::read_to_string(dir.join(name)) {
    return (branch, Some(oid.trim().to_string()));
  }
  let packed = std::fs::read_to_string(dir.join("packed-refs")).unwrap_or_default();
  let oid = packed.lines().find_map(|line| line.strip_suffix(name).and_then(|oid| oid.strip_suffix(' '))).map(str::to_string);
  (branch, oid)
}

/// Which side has moved since the two last agreed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Drift {
  InStep,
  /// Only nv moved, by catching up with a server: Git can be brought along.
  NvMoved,
  /// Only Git moved, to this commit if it has one: nv takes it in.
  GitMoved(Option<String>),
  BothMoved(Option<String>),
}

/// `None` for a workspace with no Git repository in its directory, or one never joined to it.
/// Reads two small files of each side and starts nothing.
pub fn drift(ws: &Workspace) -> Option<Drift> {
  if !shared(&ws.root) {
    return None;
  }
  let point = read_point(&ws.root)?;
  let (_, oid) = git_head(&ws.root);
  // Which branch Git is on is Git's affair: the two agree when they are on the same commit.
  let git_moved = oid != point.git;
  let nv_moved = ws.base != point.nv;
  Some(match (git_moved, nv_moved) {
    (false, false) => Drift::InStep,
    (false, true) => Drift::NvMoved,
    (true, false) => Drift::GitMoved(oid),
    (true, true) => Drift::BothMoved(oid),
  })
}

fn run_git(root: &Path, args: &[&str]) -> Result<String> {
  let out = git(root).args(args).output().map_err(|e| format!("cannot run git: {e}"))?;
  if !out.status.success() {
    return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
  }
  Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn holds(store: &mut Store, tip: Id, wanted: Id) -> Result<bool> {
  let mut seen = std::collections::HashSet::from([tip]);
  let mut todo = vec![tip];
  while let Some(at) = todo.pop() {
    if at == wanted {
      return Ok(true);
    }
    for parent in Commit::load(store, &at)?.parents {
      if seen.insert(parent) {
        todo.push(parent);
      }
    }
  }
  Ok(false)
}

/// Whether Git is in the middle of something it has to finish itself.
fn busy(root: &Path) -> Option<&'static str> {
  let dir = root.join(".git");
  [
    ("MERGE_HEAD", "a merge"),
    ("rebase-merge", "a rebase"),
    ("rebase-apply", "a rebase"),
    ("CHERRY_PICK_HEAD", "a cherry-pick"),
    ("REVERT_HEAD", "a revert"),
    ("BISECT_LOG", "a bisect"),
  ]
  .into_iter()
  .find(|(file, _)| dir.join(file).exists())
  .map(|(_, what)| what)
}

/// Bring Git to where nv is, after nv caught up with a server: the commits,
/// whatever HEAD names, and Git's index for the paths that changed. Files
/// are not touched; nv wrote them as it caught up.
fn mirror(store: &mut Store, ws: &Workspace, was: Option<Id>) -> Result<()> {
  let Some(head) = ws.base else { return Ok(()) };
  if let Some(what) = busy(&ws.root) {
    return Err(format!("Git is in the middle of {what}: finish or abort it with Git, and Git will be brought along then"));
  }
  // Git's history holds every version of every file; what came from a server came with the
  // latest only. The rest is fetched before Git is written to.
  crate::remote::fetch_for_export(store, ws)?;
  // `HEAD` is the branch Git is on, or HEAD itself when it is on none.
  let (oid, _) = crate::git::export(store, &head, &ws.root, "HEAD")?;
  match was.filter(|was| *was != head) {
    // Told path by path what changed since the two agreed: Git need not compare two trees to find out.
    Some(was) => {
      let (from, to) = (root_of(store, &was)?, root_of(store, &head)?);
      let mut lines = Vec::new();
      for (path, entry) in crate::manifest::diff(store, from, to)? {
        let (mode, oid) = match entry {
          Some(entry) if entry.mode == crate::manifest::MODE_GITLINK => (entry.mode, hex(&entry.id[..20])),
          Some(entry) => (entry.mode, hex(&crate::git::git_blob_of(store, &entry.id, &mut |_, _| Ok(()))?)),
          None => (0, "0".repeat(40)),
        };
        lines.extend_from_slice(format!("{mode:o} {oid}\t").as_bytes());
        lines.extend_from_slice(&path);
        lines.push(0);
      }
      let mut child = git(&ws.root)
        .args(["update-index", "-z", "--index-info"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
      child.stdin.take().expect("piped").write_all(&lines).map_err(|e| e.to_string())?;
      if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("git would not take the changes into its index".into());
      }
    }
    None => {
      run_git(&ws.root, &["reset", "-q", "--no-refresh"])?;
    }
  }
  let (branch, _) = git_head(&ws.root);
  write_point(&ws.root, &Point { git: Some(hex(&oid)), branch, nv: Some(head) })
}

/// Moving nv's head to a commit of Git's, with Git asked which files are as
/// that commit has them where it stores files other than as they are on disk
/// (a filter, `core.autocrlf`): for those, comparing bytes would call every
/// such file changed.
fn move_to(store: &mut Store, ws: &mut Workspace, target: Option<Id>) -> Result<()> {
  let config = std::fs::read_to_string(ws.root.join(".git").join("config")).unwrap_or_default().to_ascii_lowercase();
  if !config.contains("[filter") {
    return workspace::move_head(store, ws, target);
  }
  // The paths Git finds changed against its index, and those it has staged: every other is as committed.
  let mut changed = std::collections::HashSet::new();
  for args in [&["diff-files", "--name-only", "-z"][..], &["diff-index", "--cached", "--name-only", "-z", "HEAD"][..]] {
    let out = git(&ws.root).args(args).output().map_err(|e| format!("cannot run git: {e}"))?;
    changed.extend(out.stdout.split(|&b| b == 0).filter(|path| !path.is_empty()).map(<[u8]>::to_vec));
  }
  workspace::move_head_trusting(store, ws, target, &|path| !changed.contains(path))
}

/// Say how the two stand. The exit code is 0 only when they are in step.
/// Nothing is changed: the next command of nv's that needs to catches up.
pub fn check(root: &Path) -> Result<(i32, String)> {
  let ws = Workspace::open(root)?;
  // The directory given may be inside the workspace: it is the workspace's top that Git has or has not.
  let root = ws.root.as_path();
  if !shared(root) {
    return Ok((0, "no Git repository here: nothing to be out of step with".into()));
  }
  let (branch, oid) = git_head(root);
  let short = |oid: &Option<String>| oid.as_ref().map_or("no commit".to_string(), |o| o.chars().take(7).collect());
  let Some(drift) = drift(&ws) else {
    return Ok((1, "a Git repository and an nv workspace share this directory but were never joined: `nv reconcile .` joins them".into()));
  };
  let mut store = Store::open(&ws.store)?;
  let nv = match ws.base {
    Some(head) => Some(hex(&git_id_of(&mut store, &head)?)),
    None => None,
  };
  let both = format!("Git is at {} on {}, nv at {}", short(&oid), branch.as_deref().unwrap_or("no branch"), short(&nv));
  Ok(match drift {
    Drift::InStep => (0, "in step".into()),
    Drift::NvMoved => (1, format!("nv caught up with a server and Git was not brought along: {both}\n`nv reconcile .` brings it")),
    Drift::GitMoved(_) => (1, format!("Git moved: {both}\nnv takes that in the next time it is used, or now with `nv reconcile .`")),
    Drift::BothMoved(_) => (1, format!("both moved: {both}\n`nv reconcile .` takes Git's commits in and keeps nv's beside them")),
  })
}

/// Take in where Git is. Returns nv's id for Git's commit.
fn take_in(store: &mut Store, root: &Path, oid: &str) -> Result<Id> {
  let bytes = unhex(oid).and_then(|b| <[u8; 20]>::try_from(b).ok()).ok_or("Git's HEAD is not a commit id")?;
  if let Some(id) = store.of_git(&bytes) {
    return Ok(id);
  }
  // What nv took in last is behind this commit as a rule, and then only what is new is asked for.
  let have: Vec<String> = read_point(root)
    .and_then(|point| point.git)
    .filter(|known| unhex(known).and_then(|b| <[u8; 20]>::try_from(b).ok()).is_some_and(|known| store.of_git(&known).is_some()))
    .into_iter()
    .collect();
  crate::git::import_beyond(store, root, oid, None, &have)?;
  store.of_git(&bytes).ok_or_else(|| format!("commit {oid} did not import"))
}

/// Put the two in step, whichever moved, and say what was done if anything
/// was. Nothing either side committed is dropped. Every command of nv's
/// that builds on where the workspace stands does this first.
pub fn follow(store: &mut Store, ws: &mut Workspace) -> Result<Option<String>> {
  repair(&ws.root)?;
  let Some(state) = drift(ws) else { return Ok(None) };
  let root = ws.root.clone();
  let (branch, _) = git_head(&root);
  let was = read_point(&root).and_then(|point| point.nv);
  match state {
    Drift::InStep => Ok(None),
    Drift::NvMoved => mirror(store, ws, was).map(|()| Some("Git brought along to where nv is".to_string())),
    Drift::GitMoved(oid) | Drift::BothMoved(oid) => {
      let id = match &oid {
        Some(oid) => Some(take_in(store, &root, oid)?),
        None => None,
      };
      // What nv has that Git's commit does not hold came from a server. It is given to Git under a name.
      let mut kept = None;
      if let (Some(mine), Some(theirs)) = (ws.base, id)
        && Some(mine) != was
        && !holds(store, theirs, mine)?
      {
        let name = format!("refs/heads/nv-kept-{}", &hex(&mine)[..8]);
        crate::git::export(store, &mine, &root, &name)?;
        kept = Some(name);
      }
      // What nv had recorded and not published is, from here on, a difference between the files
      // and Git's commit like any other. A command that trusts reported writes has to be told
      // of those paths again, or it would never look at them.
      if ws.head != ws.base {
        let root_of = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
        let (from, to) = (root_of(store, ws.base)?, root_of(store, ws.head)?);
        for (path, _) in crate::manifest::diff(store, from, to)? {
          let _ = ws.record(&path);
        }
      }
      // Git wrote the files as it moved, so only what nv believes has to change.
      move_to(store, ws, id)?;
      store.flush()?;
      write_point(&root, &Point { git: oid.clone(), branch, nv: id })?;
      let at = oid.map_or("no commit".to_string(), |oid| oid[..7].to_string());
      Ok(Some(match kept {
        Some(name) => format!("took in Git's commits: nv is at {at}. What nv had from the server is in Git as {name}: merge it with Git"),
        None => format!("took in Git's commits: nv is at {at}"),
      }))
    }
  }
}

/// `follow`, as a command: the exit code says whether they are in step now.
pub fn reconcile(root: &Path) -> Result<(i32, String)> {
  let mut ws = Workspace::open(root)?;
  let top = ws.root.clone();
  let root = top.as_path();
  if !shared(root) {
    return Err("there is no Git repository in this directory".into());
  }
  let mut store = Store::open(&ws.store)?;
  if read_point(root).is_none() {
    write_point(root, &Point::default())?;
  }
  Ok((0, follow(&mut store, &mut ws)?.unwrap_or_else(|| "in step".to_string())))
}

/// Publish the workspace's snapshots as one commit made by Git: the paths
/// that differ are added to Git's index from the files and committed. Git's
/// rules apply to what goes in, Git's identity signs it, Git's hooks may
/// change or refuse it. `None` when there is nothing to commit.
///
/// What the user had staged is the user's. If no commit comes of this, Git's
/// index is put back exactly as it was found.
pub fn commit(store: &mut Store, ws: &mut Workspace, author: &str, message: &str, look: Look) -> Result<Option<String>> {
  // A merge or a rebase that stopped is Git's to conclude: its index says what is settled and what is
  // not, and a commit made around it would lose that.
  if let Some(what) = busy(&ws.root) {
    return Err(format!(
      "Git is in the middle of {what}: conclude it or abort it with Git first. Nothing was touched, and what nv recorded is kept."
    ));
  }
  workspace::snapshot(store, ws, author, look)?;
  if ws.head == ws.base {
    return Ok(None);
  }
  // A file nv has recorded and that is no longer there, its going not looked at (nobody
  // reported it): Git cannot add what is not there. Its path is looked at now.
  {
    let tree = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
    let (from, to) = (tree(store, ws.base)?, tree(store, ws.head)?);
    let gone: Vec<Vec<u8>> = crate::manifest::diff(store, from, to)?
      .into_iter()
      .filter(|(path, entry)| {
        entry.is_some_and(|e| !e.is_dir()) && std::fs::symlink_metadata(ws.root.join(workspace::to_path(path))).is_err()
      })
      .map(|(path, _)| path)
      .collect();
    if !gone.is_empty() {
      ws.note(&gone)?;
      workspace::snapshot(store, ws, author, Look::Recorded)?;
      if ws.head == ws.base {
        return Ok(None);
      }
    }
  }
  let root = |store: &mut Store, id: Option<Id>| match id {
    Some(id) => root_of(store, &id),
    None => Ok(None),
  };
  let (from, to) = (root(store, ws.base)?, root(store, ws.head)?);
  let changed = crate::manifest::diff(store, from, to)?;
  if changed.is_empty() {
    // The snapshots undid each other.
    ws.head = ws.base;
    ws.save()?;
    return Ok(None);
  }
  let dir = ws.root.clone();
  let listed = |paths: &[&Vec<u8>]| -> Vec<u8> { paths.iter().flat_map(|path| git_name(&dir, path).into_iter().chain([0])).collect() };
  let said = |out: &std::process::Output| {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string()
  };

  // The commit is built in an index of nv's own, a copy of Git's: Git's index is not touched until
  // a commit has come of this, and then only to be brought up to it. A process killed at any point,
  // and the Git it started going on without it, leave the user's staging exactly the user's.
  // A commit an earlier publishing started, and did not live to see made, is waited for: one at a time.
  drop(step_lock(&dir, true));
  repair(&dir)?;
  let index = dir.join(".git").join("index");
  // By its whole path: Git is started inside the directory, and would read a relative one from there.
  // Under a name of this command's own: a Git that an earlier, killed publishing started may still
  // be writing the index that one built.
  let nv_dir = std::fs::canonicalize(&dir).map_err(|e| e.to_string())?.join(".nv");
  for left in std::fs::read_dir(&nv_dir).into_iter().flatten().flatten() {
    if left.file_name().to_string_lossy().starts_with("git-index-publishing") {
      let _ = std::fs::remove_file(left.path());
    }
  }
  let own = nv_dir.join(format!("git-index-publishing.{}", std::process::id()));
  let fail = |what: &str, e: std::io::Error| format!("cannot {what} {}: {e}", own.display());
  match std::fs::copy(&index, &own) {
    Ok(_) => {}
    // No index yet: a repository nothing was ever added in.
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      let _ = std::fs::remove_file(&own);
    }
    Err(e) => return Err(fail("write", e)),
  }
  let with_own = |args: &[&str], paths: &[u8]| -> Result<std::process::Output> {
    let mut child = git(&dir)
      .env("GIT_LITERAL_PATHSPECS", "1")
      .env("GIT_INDEX_FILE", &own)
      .args(args)
      .args(["--pathspec-from-file=-", "--pathspec-file-nul"])
      .stdin(std::process::Stdio::piped())
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::piped())
      .spawn()
      .map_err(|e| format!("cannot run git: {e}"))?;
    child.stdin.take().expect("piped").write_all(paths).map_err(|e| e.to_string())?;
    child.wait_with_output().map_err(|e| e.to_string())
  };
  // Whether the user has anything staged of their own. If not, the commit is the plain one, of the
  // whole index: a hook that changes a file and adds it again then leaves the index as the commit.
  let only_ours = git(&dir).args(["diff", "--cached", "--quiet"]).status().is_ok_and(|status| status.success());
  // A file the user has staged in part is two decisions in one file: which lines go in is theirs
  // to say, and committing the whole file, or only what they staged, would each unsay one of them.
  if !only_ours {
    let names = |args: &[&str]| -> Result<std::collections::HashSet<Vec<u8>>> {
      let out = git(&dir).args(args).output().map_err(|e| format!("cannot run git: {e}"))?;
      Ok(out.stdout.split(|&b| b == 0).filter(|path| !path.is_empty()).map(<[u8]>::to_vec).collect())
    };
    let (staged, unstaged) = (names(&["diff", "--cached", "--name-only", "-z"])?, names(&["diff", "--name-only", "-z"])?);
    let partly: Vec<String> = changed
      .iter()
      .map(|(path, _)| git_name(&dir, path))
      .filter(|path| staged.contains(path) && unstaged.contains(path))
      .map(|path| String::from_utf8_lossy(&path).into_owned())
      .collect();
    if !partly.is_empty() {
      return Err(format!(
        "these are staged in part in Git's index, and the rest of each is not: {}\nCommit them with Git, or stage or unstage each as a whole, and `nv commit` again. Nothing was touched.",
        partly.join(", ")
      ));
    }
  }

  // What Git's index has of the paths that changed. A path gone from the disk that the index does
  // not have either (the user removed it with Git already) is nothing to add.
  let mut in_index = std::collections::HashSet::new();
  // `ls-files` reads no list of paths from a file: they are given as arguments, some at a time.
  for some in changed.chunks(256) {
    let names: Vec<std::ffi::OsString> =
      some.iter().map(|(path, _)| <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(git_name(&dir, path))).collect();
    let known = git(&dir)
      .env("GIT_LITERAL_PATHSPECS", "1")
      .args(["ls-files", "-z", "--"])
      .args(&names)
      .output()
      .map_err(|e| format!("cannot run git: {e}"))?;
    if !known.status.success() {
      return Err(format!("Git would not say what its index has:\n{}", said(&known)));
    }
    in_index.extend(known.stdout.split(|&b| b == 0).filter(|path| !path.is_empty()).map(<[u8]>::to_vec));
  }
  // A file Git does not track and its rules leave out is not committed, though nv has it recorded:
  // the user took it out of Git and wrote the rule since.
  let unknown: Vec<Vec<u8>> = changed
    .iter()
    .filter(|(_, entry)| entry.is_some())
    .map(|(path, _)| git_name(&dir, path))
    .filter(|path| !in_index.contains(path))
    .collect();
  let mut left_out = std::collections::HashSet::new();
  if !unknown.is_empty() {
    let mut child = git(&dir)
      .args(["check-ignore", "-z", "--stdin"])
      .stdin(std::process::Stdio::piped())
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::null())
      .spawn()
      .map_err(|e| format!("cannot run git: {e}"))?;
    let listing: Vec<u8> = unknown.iter().flat_map(|path| path.iter().copied().chain([0])).collect();
    child.stdin.take().expect("piped").write_all(&listing).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    left_out.extend(out.stdout.split(|&b| b == 0).filter(|path| !path.is_empty()).map(<[u8]>::to_vec));
  }
  let changed: Vec<&(Vec<u8>, Option<crate::manifest::Entry>)> =
    changed.iter().filter(|(path, _)| !left_out.contains(&git_name(&dir, path))).collect();
  if changed.is_empty() {
    return Ok(None);
  }
  let to_add: Vec<&Vec<u8>> =
    changed.iter().filter(|(path, entry)| entry.is_some() || in_index.contains(&git_name(&dir, path))).map(|(path, _)| path).collect();
  let all: Vec<&Vec<u8>> = changed.iter().map(|(path, _)| path).collect();
  let (_, before) = git_head(&dir);
  let before_text = before.clone().unwrap_or_else(|| "-".to_string());
  // What is about to be committed, on what, and whether all of the index is nv's to bring up to
  // the commit: should nobody live to do that, the next command does, from this.
  let mut note = format!("{before_text} {} {}\n", if only_ours { "all" } else { "some" }, hex(message.trim().as_bytes())).into_bytes();
  note.extend(
    std::fs::read(meta(&dir, "git-publishing"))
      .ok()
      .filter(|was| was.starts_with(before_text.as_bytes()))
      .and_then(|was| was.splitn(2, |&b| b == b'\n').nth(1).map(<[u8]>::to_vec))
      .unwrap_or_default(),
  );
  note.extend(listed(&all));
  crate::write_whole(&meta(&dir, "git-publishing"), note)?;
  if !to_add.is_empty() {
    let added = with_own(&["add", "-A", "--ignore-errors"], &listed(&to_add))?;
    // A path Git ignores is not an error here: it is simply not committed.
    if !added.status.success() && !said(&added).contains("ignored by one of your .gitignore files") {
      let _ = std::fs::remove_file(meta(&dir, "git-publishing"));
      let _ = std::fs::remove_file(&own);
      return Err(format!("Git would not add the changes, and its index is as it was:\n{}", said(&added)));
    }
  }
  // The commit, and Git's index brought up to it, are made by a process of their own, in a session
  // of its own: whoever kills this command (a tool's time limit) does not leave a commit made and
  // an index that has not heard of it.
  let committed = {
    use std::os::unix::process::CommandExt;
    let mut step = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    step
      .arg("publish-step")
      .arg(std::fs::canonicalize(&dir).map_err(|e| e.to_string())?)
      .arg(&own)
      .arg(if only_ours { "all" } else { "some" })
      .arg(&before_text)
      .arg(message)
      .stdin(std::process::Stdio::piped())
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::piped());
    // SAFETY: `setsid` is safe to call between fork and exec.
    unsafe { step.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }) };
    let mut child = step.spawn().map_err(|e| format!("cannot start the commit: {e}"))?;
    child.stdin.take().expect("piped").write_all(&listed(&all)).map_err(|e| e.to_string())?;
    child.wait_with_output().map_err(|e| e.to_string())?
  };
  let (branch, after) = git_head(&dir);
  if after == before {
    // No commit came, and the step that could have made one has ended: nothing is half done, and
    // a note left here would be read, after a commit of the user's own, as nv's to finish.
    let _ = std::fs::remove_file(meta(&dir, "git-publishing"));
    let _ = std::fs::remove_file(&own);
    // Git found nothing in those paths to commit, or one of its hooks said no.
    if committed.status.success()
      || said(&committed).contains("nothing to commit")
      || said(&committed).contains("no changes added to commit")
    {
      return Ok(None);
    }
    return Err(format!("Git did not make the commit. Nothing recorded was lost, and Git's index is as it was:\n{}", said(&committed)));
  }
  repair(&dir)?;
  let oid = after.ok_or("Git has no commit after committing")?;
  let id = take_in(store, &dir, &oid)?;
  move_to(store, ws, Some(id))?;
  store.flush()?;
  write_point(&dir, &Point { git: Some(oid.clone()), branch, nv: Some(id) })?;
  ws.log("commit", &id)?;
  Ok(Some(oid))
}

/// The lock the committing step of a publishing holds while it lives.
fn step_lock(root: &Path, wait: bool) -> Option<std::fs::File> {
  use std::os::fd::AsRawFd;
  let file = std::fs::File::options().create(true).append(true).open(meta(root, "publishing.lock")).ok()?;
  if wait {
    // A publishing still making its commit is waited for, as long as a lock is.
    return crate::limits::lock(&file, "an earlier publishing's commit").ok().map(|()| file);
  }
  // SAFETY: the descriptor is open for as long as `file` is.
  (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

/// The step of publishing that must not be left half done: Git's commit, from the index nv built
/// for it, and then Git's own index brought up to that commit. Run as a process of its own; the
/// paths committed come on its input when only some of the index is nv's.
pub fn publish_step(args: &[String]) -> i32 {
  let [dir, own, how, before, message] = args else { return 2 };
  let (dir, own) = (Path::new(dir), Path::new(own));
  // Held for as long as this step lives: the next publishing waits for it.
  let _held = step_lock(dir, true);
  let mut commit = git(dir);
  // A hook of this commit that runs nv on this workspace is told so, instead of waiting for ever
  // for the lock this publishing holds.
  commit.env("GIT_INDEX_FILE", own).env("NV_PUBLISHING", dir).args(["commit", "-q", "-m", message]);
  if how != "all" {
    commit.env("GIT_LITERAL_PATHSPECS", "1").args(["--only", "--pathspec-from-file=-", "--pathspec-file-nul"]);
  } else {
    commit.stdin(std::process::Stdio::null());
  }
  // Git's hooks are the user's programs: one that does not return is stopped, with Git, after a while.
  let code = match crate::limits::run(&mut commit, crate::limits::git_commit(), "Git's commit, hooks included") {
    Ok(status) => status.code().unwrap_or(2),
    Err(why) => {
      eprintln!("{why}");
      2
    }
  };
  let (_, after) = git_head(dir);
  if after.as_deref().unwrap_or("-") != before {
    // All of the index was nv's to commit: the one the commit was made from is the index now.
    if how == "all" && own.exists() && std::fs::rename(own, dir.join(".git").join("index")).is_ok() {
      let _ = std::fs::remove_file(meta(dir, "git-publishing"));
    } else {
      // This step made the commit (HEAD moved while it ran, from the commit it was told of): it is
      // nv's, whatever a hook did to its message.
      let _ = bring_index_up(dir, true);
      let _ = std::fs::remove_file(own);
    }
  }
  code
}

/// Bring Git's index up to a commit that nv published and did not live to bring it up to: for the
/// paths that were being committed, the index gets what the commit has. Nothing to do when no
/// publishing was cut short, or when no commit came of the one that was.
fn repair(root: &Path) -> Result<()> {
  bring_index_up(root, false)
}

/// `repair`; with `ours`, the caller knows the commit Git is on to be the one nv's publishing made.
fn bring_index_up(root: &Path, ours: bool) -> Result<()> {
  let note = meta(root, "git-publishing");
  let Ok(bytes) = std::fs::read(&note) else { return Ok(()) };
  let mut parts = bytes.splitn(2, |&b| b == b'\n');
  let first = parts.next().map(|line| String::from_utf8_lossy(line).into_owned()).unwrap_or_default();
  let mut words = first.split(' ');
  let (before, how, said) = (words.next().unwrap_or_default(), words.next().unwrap_or("some"), words.next().unwrap_or_default());
  // When all of the index was nv's to commit, all of it is brought up to the commit: a hook may
  // have added paths of its own to what was committed.
  let paths: &[u8] = if how == "all" { b"" } else { parts.next().unwrap_or_default() };
  let whole = how == "all";
  let (_, now) = git_head(root);
  // Whether a committing step is still alive: only one of those can still make the commit.
  let idle = step_lock(root, false).is_some();
  // Git's HEAD has not moved: no commit was made. While a step lives the note stays, for the
  // commit that may still come; with none alive there is nothing left to come.
  if now.as_deref().unwrap_or("-") == before {
    if idle {
      let _ = std::fs::remove_file(&note);
    }
    return Ok(());
  }
  // Git's HEAD moved, and not by a commit made on where it was: the user moved it (a reset, a
  // switch). Nothing of nv's is half done, and the index is the user's.
  let parent = git(root).args(["rev-parse", "--verify", "-q", "HEAD^"]).output().map_err(|e| format!("cannot run git: {e}"))?;
  // Otherwise nv's commit is known by its message: as nv gave it, or with what a hook added after it.
  let message = git(root).args(["log", "-1", "--format=%B"]).output().map_err(|e| format!("cannot run git: {e}"))?;
  let given = unhex(said).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
  let ours =
    ours || said.is_empty() || given.is_some_and(|given| String::from_utf8_lossy(&message.stdout).trim().starts_with(given.as_str()));
  if !ours || (String::from_utf8_lossy(&parent.stdout).trim() != before && !(before == "-" && !parent.status.success())) {
    if idle {
      let _ = std::fs::remove_file(&note);
    }
    return Ok(());
  }
  if root.join(".git").join("index.lock").exists() {
    return Ok(());
  }
  if whole {
    let _ = git(root).args(["reset", "-q"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
  } else if !paths.is_empty() {
    let mut child = git(root)
      .env("GIT_LITERAL_PATHSPECS", "1")
      .args(["reset", "-q", "--pathspec-from-file=-", "--pathspec-file-nul"])
      .stdin(std::process::Stdio::piped())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .spawn()
      .map_err(|e| format!("cannot run git: {e}"))?;
    child.stdin.take().expect("piped").write_all(paths).map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
  }
  let _ = std::fs::remove_file(note);
  Ok(())
}

/// A path as Git names it. On macOS Git composes the characters of a name
/// the file system gave it decomposed (`core.precomposeunicode`), and a path
/// given to Git has to be the composed one.
fn git_name(root: &Path, path: &[u8]) -> Vec<u8> {
  if path.is_ascii() || !cfg!(target_os = "macos") {
    return path.to_vec();
  }
  static COMPOSES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  let composes = *COMPOSES.get_or_init(|| crate::git::config_value(root, "core", "precomposeunicode").as_deref() != Some("false"));
  match std::str::from_utf8(path) {
    Ok(text) if composes => crate::workspace::composed(text).into_bytes(),
    _ => path.to_vec(),
  }
}

/// After a command of nv's own published a commit in a workspace with no
/// Git repository: the branch nv's Git commands know is where the commit is.
pub fn published(store: &mut Store, ws: &Workspace) -> Result<()> {
  let branch =
    std::fs::read_to_string(meta(&ws.root, "git-head")).ok().and_then(|text| text.trim().strip_prefix("ref: ").map(str::to_string));
  if let (Some(base), Some(branch)) = (ws.base, branch) {
    store.set_ref(&format!("heads/{branch}"), base)?;
  }
  Ok(())
}

/// The paths Git has in its index and keeps out of the working tree on purpose (a sparse
/// checkout, `skip-worktree`): absent from the disk, and not removed. `None` where Git is
/// not asked to keep anything out, which is nearly everywhere, and then no process is started.
pub fn kept_out(root: &Path) -> Option<std::collections::HashSet<Vec<u8>>> {
  if !shared(root) {
    return None;
  }
  let sparse = root.join(".git").join("info").join("sparse-checkout").is_file()
    || crate::git::config_value(root, "core", "sparsecheckout").is_some_and(|value| value == "true");
  if !sparse {
    return None;
  }
  let out = git(root).args(["ls-files", "-t", "-z"]).output().ok()?;
  Some(out.stdout.split(|&b| b == 0).filter_map(|line| line.strip_prefix(b"S ")).map(<[u8]>::to_vec).collect())
}

const EXCLUDED: &str = ".nv/";

/// Make an nv workspace of a Git repository's directory, in place: the
/// history of the branch it is on is imported, its files are left as they
/// are, and Git is left as it is.
pub fn adopt(root: &Path) -> Result<String> {
  if !shared(root) {
    return Err(format!("{} is not the top of a Git repository", root.display()));
  }
  // An adoption that was cut short is finished here; one that was finished is not done twice.
  if meta(root, "workspace").exists() && read_point(root).is_some() {
    return Err("this directory is an nv workspace already: `nv check .` says how it stands with Git".into());
  }
  // First of all, Git is told not to see nv's directory: whatever happens next, `git add -A` must not take it.
  let exclude = root.join(".git").join("info").join("exclude");
  let listed = std::fs::read_to_string(&exclude).unwrap_or_default();
  // How the file was found, so that taking nv out again can leave it exactly so.
  let mut found = None;
  if !listed.lines().any(|line| line.trim() == EXCLUDED) {
    let info = exclude.parent().expect("in .git");
    found = Some(match (info.is_dir(), exclude.exists()) {
      (false, _) => "no directory".to_string(),
      (true, false) => "no file".to_string(),
      (true, true) => format!("{} bytes", listed.len()),
    });
    std::fs::create_dir_all(info).map_err(|e| e.to_string())?;
    let sep = if listed.is_empty() || listed.ends_with('\n') { "" } else { "\n" };
    crate::write_whole(&exclude, format!("{listed}{sep}{EXCLUDED}\n"))?;
  } else if !meta(root, "workspace").exists() {
    found = Some("the line was there".to_string());
  }
  let mut ws = if meta(root, "workspace").exists() { Workspace::open(root)? } else { workspace::init(root)? };
  if let Some(found) = found {
    crate::write_whole(&meta(root, "git-exclude-before"), found)?;
  }
  let mut store = Store::open(&ws.store)?;
  let (branch, oid) = git_head(root);
  // The commit Git is on, and what is behind it. The rest is taken in when Git goes there.
  let head = match &oid {
    Some(oid) => Some(take_in(&mut store, root, oid)?),
    None => None,
  };
  move_to(&mut store, &mut ws, head)?;
  ws.start = head;
  ws.save()?;
  store.flush()?;
  write_point(root, &Point { git: oid, branch: branch.clone(), nv: head })?;
  // One look at every file, so that the next is as quick as any.
  let (differences, looked) = workspace::status(&ws, Look::Everywhere)?;
  Ok(format!(
    "adopted: on {}, {} files, {} that differ from the commit",
    branch.as_deref().unwrap_or("no branch"),
    looked.stats,
    differences.len()
  ))
}

/// Take nv out of a Git repository's directory again. Git is as it was:
/// nothing of nv's was ever in it but one line of an exclude file.
pub fn unadopt(root: &Path) -> Result<String> {
  if !shared(root) || !meta(root, "workspace").is_file() {
    return Err(format!("{} is not a Git repository that nv was added to", root.display()));
  }
  let _ = crate::watch::stop(root);
  let exclude = root.join(".git").join("info").join("exclude");
  if let Ok(listed) = std::fs::read_to_string(&exclude) {
    let found = std::fs::read_to_string(meta(root, "git-exclude-before")).unwrap_or_default();
    let ours = format!("{EXCLUDED}\n");
    // As it was found, to the byte, when nothing but nv's line was added since.
    let before = found
      .strip_suffix(" bytes")
      .and_then(|n| n.parse::<usize>().ok())
      .filter(|n| listed.get(*n..).is_some_and(|added| added == ours || added.strip_prefix('\n') == Some(ours.as_str())));
    match (found.as_str(), before) {
      ("the line was there", _) => {}
      (_, Some(length)) => crate::write_whole(&exclude, &listed[..length])?,
      ("no file" | "no directory", _) if listed == ours => {
        let _ = std::fs::remove_file(&exclude);
        if found == "no directory" {
          let _ = std::fs::remove_dir(exclude.parent().expect("in .git"));
        }
      }
      _ => {
        let kept: String = listed.lines().filter(|line| line.trim() != EXCLUDED).map(|line| format!("{line}\n")).collect();
        crate::write_whole(&exclude, kept)?;
      }
    }
  }
  std::fs::remove_dir_all(root.join(".nv")).map_err(|e| format!("cannot remove {}: {e}", root.join(".nv").display()))?;
  Ok("nv is out of this directory: the files and the Git repository are as they were, and what nv had recorded is gone".into())
}
