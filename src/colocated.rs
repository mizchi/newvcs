//! A directory that is a Git repository and an nv workspace at once.
//!
//! Both keep a head, and nothing in Git tells nv when Git's moves. Git's is
//! compared with nv's last published commit: snapshots are nobody's business
//! but the workspace's, and never reach Git. So nv
//! writes down the state in which the two last agreed (`.nv/git-synced`),
//! and from that can tell which side has moved since:
//!
//! - nv moved: Git is brought along (`mirror`), which nv's own Git commands
//!   do after every command that moves the head.
//! - Git moved: nv takes Git's commits in, with their ids (`reconcile`).
//! - both moved: Git's commit is taken in under the name `git/<branch>`,
//!   and nv's branch has to be merged with it before Git is brought along.
//!
//! Two hooks stop Git from moving a branch on its own in the first place,
//! and a shell function sends `git` typed inside a workspace to nv. Neither
//! can be relied on to always be there, which is why the state is checked.

use std::path::{Path, PathBuf};

use crate::commit::Commit;
use crate::git::{git, git_id_of, import};
use crate::store::Store;
use crate::workspace::{self, Look, Workspace};
use crate::{Id, Result, hex, unhex};

/// Set for every Git process nv starts: the hooks let those through.
pub const WRITING: &str = "NV_WRITING";

const MARK: &str = "# installed by nv";
const STOPPED: &str = "\
nv: this repository is managed by nv, and Git must not move its branches on its own.
nv: use `nv git <command>`, or the shell function that `nv shell-init` prints.
nv: if Git has moved already, `nv check .` says how and `nv reconcile .` puts it right.";

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
  std::fs::write(meta(root, "git-synced"), text).map_err(|e| format!("cannot note where Git is: {e}"))
}

/// Git's branch and commit, read from its files: no process is started.
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

fn nv_branch(root: &Path) -> Option<String> {
  std::fs::read_to_string(meta(root, "git-head")).ok()?.trim().strip_prefix("ref: ").map(str::to_string)
}

/// Which side has moved since the two last agreed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Drift {
  InStep,
  /// Only nv moved: Git can be brought along.
  NvMoved,
  /// Only Git moved, to this commit: nv has to take it in.
  GitMoved(String),
  BothMoved(String),
}

/// `None` for a workspace with no Git repository in its directory, or one never joined to it.
/// Reads two small files of each side and starts nothing.
pub fn drift(ws: &Workspace) -> Option<Drift> {
  if !ws.root.join(".git").is_dir() {
    return None;
  }
  let point = read_point(&ws.root)?;
  let (branch, oid) = git_head(&ws.root);
  let git_moved = (branch.as_ref(), oid.as_ref()) != (point.branch.as_ref(), point.git.as_ref());
  let nv_moved = ws.base != point.nv || nv_branch(&ws.root) != point.branch;
  let at = oid.unwrap_or_else(|| "no commit".into());
  Some(match (git_moved, nv_moved) {
    (false, false) => Drift::InStep,
    (false, true) => Drift::NvMoved,
    (true, false) => Drift::GitMoved(at),
    (true, true) => Drift::BothMoved(at),
  })
}

/// What to tell someone about a drift that nv's Git commands will not work across.
pub fn warning(drift: &Drift) -> Option<String> {
  let short = |oid: &str| oid.chars().take(7).collect::<String>();
  match drift {
    Drift::GitMoved(oid) => Some(format!("nv: Git moved to {} without nv. Run `nv reconcile .` to take that in.", short(oid))),
    Drift::BothMoved(oid) => Some(format!("nv: Git moved to {} without nv, and nv moved too. Run `nv reconcile .`.", short(oid))),
    _ => None,
  }
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

/// A commit of Git's that nv's branch has yet to be merged with, if there is one.
fn unjoined(root: &Path) -> Option<Id> {
  std::fs::read_to_string(meta(root, "git-unjoined")).ok().and_then(|t| unhex(t.trim())).and_then(|b| Id::try_from(b).ok())
}

/// Bring Git to where nv is: the commits, the branch, HEAD and Git's index.
/// Refused while Git has a commit nv's branch does not hold.
pub fn mirror(store: &mut Store, ws: &Workspace) -> Result<()> {
  let (Some(head), Some(branch)) = (ws.base, nv_branch(&ws.root)) else { return Ok(()) };
  if let Some(theirs) = unjoined(&ws.root) {
    if !holds(store, head, theirs)? {
      return Err(apart(&branch));
    }
    let _ = std::fs::remove_file(meta(&ws.root, "git-unjoined"));
  }
  // nv's commit when the two last agreed, while Git has not moved since.
  let was = read_point(&ws.root).filter(|point| git_head(&ws.root) == (point.branch.clone(), point.git.clone())).and_then(|point| point.nv);
  let name = format!("refs/heads/{branch}");
  let (oid, _) = crate::git::export(store, &head, &ws.root, &name)?;
  if git_head(&ws.root).0.as_deref() != Some(branch.as_str()) {
    run_git(&ws.root, &["symbolic-ref", "HEAD", &name])?;
  }
  // Git's index is made to say what the commit says. Files are not looked at, and not touched.
  match was.filter(|was| *was != head) {
    // Told path by path what changed since the two agreed: Git need not compare two trees to find out.
    Some(was) => {
      let (from, to) = (crate::commit::root_of(store, &was)?, crate::commit::root_of(store, &head)?);
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
      std::io::Write::write_all(&mut child.stdin.take().expect("piped"), &lines).map_err(|e| e.to_string())?;
      if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("git would not take the changes into its index".into());
      }
    }
    None => {
      run_git(&ws.root, &["reset", "-q", "--no-refresh"])?;
    }
  }
  write_point(&ws.root, &Point { git: Some(hex(&oid)), branch: Some(branch), nv: Some(head) })
}

fn apart(branch: &str) -> String {
  format!(
    "nv and Git each have a commit the other's branch does not hold.\nGit's is in nv as `git/{branch}`. Join the two, then reconcile again:\n  nv git merge git/{branch}\n  nv reconcile ."
  )
}

/// Say how the two stand. The exit code is 0 only when they are in step.
pub fn check(root: &Path) -> Result<(i32, String)> {
  let ws = Workspace::open(root)?;
  if !root.join(".git").is_dir() {
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
  let both = format!(
    "Git is at {} on {}, nv at {} on {}",
    short(&oid),
    branch.as_deref().unwrap_or("no branch"),
    short(&nv),
    nv_branch(root).as_deref().unwrap_or("no branch")
  );
  Ok(match drift {
    Drift::InStep => (0, "in step".into()),
    Drift::NvMoved if unjoined(root).is_some() => (1, format!("apart: {both}\n{}", apart(branch.as_deref().unwrap_or("HEAD")))),
    Drift::NvMoved => (1, format!("nv moved, Git did not: {both}\n`nv reconcile .` brings Git along")),
    Drift::GitMoved(_) => (1, format!("Git moved, nv did not: {both}\n`nv reconcile .` takes Git's commits in")),
    Drift::BothMoved(_) => (1, format!("both moved: {both}\n`nv reconcile .` takes Git's commits in and says what is left to do")),
  })
}

/// Take in where Git is. Returns nv's id for Git's commit.
fn take_in(store: &mut Store, root: &Path, oid: &str) -> Result<Id> {
  import(store, root, oid, None)?;
  let bytes = unhex(oid).and_then(|b| <[u8; 20]>::try_from(b).ok()).ok_or("Git's HEAD is not a commit id")?;
  store.of_git(&bytes).ok_or_else(|| format!("commit {oid} did not import"))
}

/// Put the two back in step, whichever moved. Nothing either side committed is dropped.
pub fn reconcile(root: &Path) -> Result<(i32, String)> {
  let mut ws = Workspace::open(root)?;
  if !root.join(".git").is_dir() {
    return Err("there is no Git repository in this directory".into());
  }
  let mut store = Store::open(&ws.store)?;
  // nv's own commands move the workspace's head and know nothing of branches: the branch is where the head is.
  if let (Some(head), Some(branch)) = (ws.base, nv_branch(root))
    && store.read_ref(&format!("heads/{branch}")) != Some(head)
  {
    store.set_ref(&format!("heads/{branch}"), head)?;
  }
  let (branch, oid) = git_head(root);
  let Some(branch) = branch else {
    return Err("Git's HEAD is on no branch, which nv does not follow: check a branch out with Git first".into());
  };
  let state = match drift(&ws) {
    Some(state) => state,
    // Never joined: in step if they are on the same commit already, and otherwise as if both had moved.
    None => Drift::BothMoved(oid.clone().unwrap_or_default()),
  };
  match (state, oid) {
    (Drift::InStep, _) => Ok((0, "in step".into())),
    (Drift::NvMoved, _) => {
      mirror(&mut store, &ws).map(|()| (0, "Git brought along to where nv is".to_string())).or_else(|why| Ok((1, why)))
    }
    (Drift::GitMoved(_), Some(oid)) => {
      // Git wrote the files as it moved, so only what nv believes has to change.
      let id = take_in(&mut store, root, &oid)?;
      workspace::move_head(&mut store, &mut ws, Some(id))?;
      store.set_ref(&format!("heads/{branch}"), id)?;
      store.flush()?;
      std::fs::write(meta(root, "git-head"), format!("ref: {branch}\n")).map_err(|e| e.to_string())?;
      // What was added through nv was added against a head that is no longer the head.
      for stale in ["git-staged", "git-merge", "git-unmerged"] {
        let _ = std::fs::remove_file(meta(root, stale));
      }
      write_point(root, &Point { git: Some(oid.clone()), branch: Some(branch), nv: Some(id) })?;
      Ok((0, format!("took in Git's commits: nv is at {}", &oid[..7])))
    }
    (Drift::BothMoved(_), Some(oid)) => {
      let id = take_in(&mut store, root, &oid)?;
      store.set_ref(&format!("remotes/git/{branch}"), id)?;
      store.flush()?;
      if ws.base.is_none() {
        // nv has nothing of its own: this is the first joining.
        workspace::move_head(&mut store, &mut ws, Some(id))?;
        store.set_ref(&format!("heads/{branch}"), id)?;
        store.flush()?;
        std::fs::write(meta(root, "git-head"), format!("ref: {branch}\n")).map_err(|e| e.to_string())?;
      }
      // Git's side is known now; what remains is nv being ahead of it, or apart from it.
      let nv_branch = nv_branch(root);
      if ws.base != Some(id) {
        std::fs::write(meta(root, "git-unjoined"), hex(&id)).map_err(|e| e.to_string())?;
      }
      write_point(root, &Point { git: Some(oid), branch: Some(branch), nv: (ws.base == Some(id) && nv_branch.is_some()).then_some(id) })?;
      if ws.base == Some(id) {
        return Ok((0, "in step".into()));
      }
      mirror(&mut store, &ws).map(|()| (0, "Git brought along to where nv is".to_string())).or_else(|why| Ok((1, why)))
    }
    (_, None) => Err("Git has no commit to take in".into()),
  }
}

/// Before a command of nv's own publishes: refused where Git has moved and nv has not caught up.
pub fn guard(ws: &Workspace) -> Result<()> {
  match drift(ws).as_ref().and_then(warning) {
    Some(warning) => Err(warning),
    None => Ok(()),
  }
}

/// After a command of nv's own published a commit: the branch nv's Git
/// commands know is where the commit is, and a Git repository in the same
/// directory is brought along.
pub fn published(store: &mut Store, ws: &Workspace) -> Result<()> {
  if let (Some(base), Some(branch)) = (ws.base, nv_branch(&ws.root)) {
    store.set_ref(&format!("heads/{branch}"), base)?;
  }
  if drift(ws) == Some(Drift::NvMoved) {
    mirror(store, ws)?;
  }
  Ok(())
}

/// Stop Git from moving a branch on its own, and say why. A hook that is
/// someone else's is left as it is.
pub fn install_hooks(root: &Path) -> Result<Vec<String>> {
  let hooks = root.join(".git").join("hooks");
  std::fs::create_dir_all(&hooks).map_err(|e| e.to_string())?;
  let message: String = STOPPED.lines().map(|line| format!("  echo '{line}' >&2\n")).collect();
  let scripts = [
    // Before a commit is even written. `--no-verify` skips this one.
    ("pre-commit", format!("#!/bin/sh\n{MARK}\nif [ -z \"${{{WRITING}:-}}\" ]; then\n{message}  exit 1\nfi\n")),
    // Before any ref changes, whatever the command and its options.
    (
      "reference-transaction",
      format!(
        "#!/bin/sh\n{MARK}\n[ -n \"${{{WRITING}:-}}\" ] && exit 0\n[ \"$1\" = prepared ] || exit 0\nwhile read -r old new ref; do\n  case \"$ref\" in\n  HEAD | refs/heads/*)\n    [ \"$old\" = \"$new\" ] && continue\n{message}    exit 1\n    ;;\n  esac\ndone\nexit 0\n"
      ),
    ),
  ];
  let mut installed = Vec::new();
  for (name, script) in scripts {
    let path = hooks.join(name);
    if std::fs::read_to_string(&path).is_ok_and(|existing| !existing.contains(MARK)) {
      continue;
    }
    std::fs::write(&path, script).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    installed.push(name.to_string());
  }
  Ok(installed)
}

/// Make an nv workspace of a Git repository's directory, in place: its
/// history is imported, its files are left as they are, and Git is told not
/// to move on its own.
pub fn adopt(root: &Path) -> Result<String> {
  if !root.join(".git").is_dir() {
    return Err(format!("{} is not the top of a Git repository", root.display()));
  }
  if meta(root, "workspace").exists() {
    return Err("this directory is an nv workspace already: `nv check .` says how it stands with Git".into());
  }
  let mut ws = workspace::init(root)?;
  let mut store = Store::open(&ws.store)?;
  let (branch, oid) = git_head(root);
  let Some(branch) = branch else { return Err("Git's HEAD is on no branch: check a branch out first".into()) };
  // The branch Git is on, and no other: the rest are taken in when Git goes to them.
  let head = match &oid {
    Some(oid) => Some(take_in(&mut store, root, oid)?),
    None => None,
  };
  if let Some(head) = head {
    store.set_ref(&format!("heads/{branch}"), head)?;
  }
  workspace::move_head(&mut store, &mut ws, head)?;
  store.flush()?;
  std::fs::write(meta(root, "git-head"), format!("ref: {branch}\n")).map_err(|e| e.to_string())?;
  // Git should not see nv's directory as something to add.
  let exclude = root.join(".git").join("info").join("exclude");
  let listed = std::fs::read_to_string(&exclude).unwrap_or_default();
  if !listed.lines().any(|line| line.trim() == ".nv/") {
    std::fs::create_dir_all(exclude.parent().expect("in .git")).map_err(|e| e.to_string())?;
    let sep = if listed.is_empty() || listed.ends_with('\n') { "" } else { "\n" };
    std::fs::write(&exclude, format!("{listed}{sep}.nv/\n")).map_err(|e| e.to_string())?;
  }
  let hooks = install_hooks(root)?;
  write_point(root, &Point { git: oid, branch: Some(branch.clone()), nv: head })?;
  // One look at every file, so that the next is as quick as any.
  let (differences, looked) = workspace::status(&ws, Look::Everywhere)?;
  Ok(format!(
    "adopted: on {branch}, {} files, {} that differ from the commit; hooks installed: {}",
    looked.stats,
    differences.len(),
    if hooks.is_empty() { "none (there were hooks already)".to_string() } else { hooks.join(", ") }
  ))
}
