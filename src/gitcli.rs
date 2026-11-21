//! `nv git …`: the part of Git's command line that nv speaks.
//!
//! Enough to run a script written for Git's everyday commands against an nv
//! store and get the same output and the same commit ids.
//!
//! What is spoken is a table, `SPOKEN`: each command with the options it
//! takes. An invocation is checked against the table before anything runs,
//! so a command or an option that is not in it never reaches code that might
//! half-understand it. It fails with "not supported": a script that depends
//! on it should stop, not go on with something that only looks like Git.
//! `tools/git-compat.sh` runs the same scripts under Git and under this and
//! compares; `tools/git-compat-unsupported.txt` lists what must be refused.
//!
//! Where it is not Git even inside those commands:
//!
//! - The index is kept as the paths where it differs from `HEAD`, not as a
//!   list of every file.
//! - A merge stops for someone to settle only where lines of text met. Other
//!   conflicts (a file deleted on one side, a binary file) are refused.
//! - `.gitattributes` is read for line endings only.
//! - `clone`, `fetch`, `pull` and `push` work against a Git repository on
//!   this machine, by reading and writing its objects.

use crate::commit::{Commit, root_of};
use crate::git::{TreeMemo, git_commit_bytes, git_commit_of, git_hash, git_tree_of, import_refs};
use crate::manifest::{Change, Entry, MODE_GITLINK, MODE_LINK, diff, load, lookup};
use crate::store::{Kind, Store};
use crate::textdiff;
use crate::workspace::{self, Workspace};
use crate::{Id, hex, unhex};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Exit code for "nv does not speak this".
const UNSUPPORTED: i32 = 125;

struct Fail {
  code: i32,
  message: String,
}

type Res<T> = std::result::Result<T, Fail>;

fn unsupported<T>(what: impl std::fmt::Display) -> Res<T> {
  // `what` is a command line, or one whose own closing mark is followed by words about it.
  let what = what.to_string();
  let close = if what.matches('`').count() % 2 == 1 { "" } else { "`" };
  Err(Fail { code: UNSUPPORTED, message: format!("nv: `git {what}{close} is not supported") })
}

fn fatal<T>(code: i32, message: impl Into<String>) -> Res<T> {
  Err(Fail { code, message: message.into() })
}

impl From<String> for Fail {
  fn from(message: String) -> Fail {
    Fail { code: 128, message: format!("fatal: {message}") }
  }
}

/// See `Repo::staged`.
type Staged = std::collections::BTreeMap<Vec<u8>, Option<Entry>>;

/// One path in the three places Git compares: the last commit, the index
/// (what a commit would record), and the working tree.
struct State {
  head: Option<Entry>,
  index: Option<Entry>,
  /// The file's mode and bytes, if it is on disk and is not what the index has.
  disk: Option<(u32, std::sync::Arc<Vec<u8>>)>,
  /// Whether the file on disk is what the index has (both absent counts).
  disk_is_index: bool,
  /// A merge left both sides' lines in the file, for someone to choose between.
  unmerged: bool,
}

impl State {
  /// How the index differs from the last commit: `A`, `M`, `D`, or nothing.
  fn staged(&self) -> Option<char> {
    if self.unmerged {
      return Some('U');
    }
    match (self.head, self.index) {
      (a, b) if a == b => None,
      (None, _) => Some('A'),
      (_, None) => Some('D'),
      _ => Some('M'),
    }
  }

  /// How the working tree differs from the index, for a path the index has.
  fn unstaged(&self) -> Option<char> {
    if self.unmerged {
      return Some('U');
    }
    match (self.index.is_some(), self.disk_is_index, self.on_disk()) {
      (true, false, true) => Some('M'),
      (true, false, false) => Some('D'),
      _ => None,
    }
  }

  fn on_disk(&self) -> bool {
    self.disk.is_some() || (self.disk_is_index && self.index.is_some())
  }

  fn untracked(&self) -> bool {
    self.index.is_none() && self.disk.is_some()
  }
}

struct Repo {
  ws: Workspace,
  store: Store,
  /// Where the command was run, relative to the workspace root.
  prefix: Vec<u8>,
  /// Held for as long as the repository is open: one writer at a time.
  _lock: std::fs::File,
}

fn out(bytes: &[u8]) {
  let _ = std::io::stdout().write_all(bytes);
}

/// A command killed between moving the workspace to a new commit and moving its branch there
/// leaves the branch one step behind, or, for an amend, on the commit that was replaced. Nothing else leaves a branch behind the workspace that is
/// on it, so the move is finished here: otherwise every later commit would be made off the
/// branch, and lost to it at the next switch.
fn caught_up(store: &mut Store, ws: &Workspace) -> Res<()> {
  let head_file = ws.root.join(".nv").join("git-head");
  let Some(name) = std::fs::read_to_string(head_file).ok().and_then(|text| text.trim().strip_prefix("ref: ").map(str::to_string)) else {
    return Ok(());
  };
  let (Some(head), tip) = (ws.head, store.read_ref(&format!("heads/{name}"))) else { return Ok(()) };
  if tip == Some(head) {
    return Ok(());
  }
  // Only forwards, and only along first parents: the commits this workspace made on the branch.
  let mut at = head;
  for _ in 0..4096 {
    let Some(parent) = Commit::load(store, &at)?.parents.first().copied() else { break };
    if Some(parent) == tip {
      store.compare_and_set_ref(&format!("heads/{name}"), tip, head)?;
      store.flush()?;
      return Ok(());
    }
    at = parent;
  }
  // Or sideways, for an amend: the workspace's commit is one it logged after the branch's, in its
  // place, on the same parents.
  if let Some(tip) = tip {
    let logged = ws.operations();
    let last = |id: &Id| logged.iter().rposition(|(_, what, at)| what == "commit" && *at == hex(id));
    if let (Some(replaced), Some(replacing)) = (last(&tip), last(&head))
      && replaced < replacing
      && Commit::load(store, &tip)?.parents == Commit::load(store, &head)?.parents
    {
      store.compare_and_set_ref(&format!("heads/{name}"), Some(tip), head)?;
      store.flush()?;
    }
  }
  Ok(())
}

/// Whether a path is inside nv's own directory, or what would be Git's: never a file of the tree.
fn own_place(path: &[u8]) -> bool {
  let first = path.split(|&b| b == b'/').next().unwrap_or_default();
  first.eq_ignore_ascii_case(b".nv") || first.eq_ignore_ascii_case(b".git")
}

impl Repo {
  /// The path an argument names, for a command that takes paths of the tree only.
  fn inside(&self, arg: &str) -> Res<Vec<u8>> {
    let depth = self.prefix.split(|&b| b == b'/').filter(|p| !p.is_empty()).count() as isize;
    let mut level = depth;
    for part in arg.split('/') {
      match part {
        ".." => level -= 1,
        "" | "." => {}
        _ => level += 1,
      }
      if level < 0 {
        return unsupported(format_args!("{arg}` (a path outside the repository)"));
      }
    }
    if arg.starts_with('/') {
      return unsupported(format_args!("{arg}` (an absolute path)"));
    }
    Ok(self.rooted(arg))
  }

  fn open(start: &Path) -> Res<Repo> {
    let start = std::fs::canonicalize(start).map_err(|e| e.to_string())?;
    let mut dir = start.as_path();
    loop {
      if dir.join(".nv").join("workspace").is_file() {
        let _lock = workspace::lock(dir)?;
        let mut ws = Workspace::open(dir)?;
        let mut store = Store::open(&ws.store)?;
        // Git has commits and uncommitted changes and nothing between: snapshots are seen as the latter.
        workspace::fold(&mut store, &mut ws)?;
        caught_up(&mut store, &ws)?;
        // History that came from a server came without the files of commits this workspace never
        // had on disk, and Git's id of a commit is over all of its files. They are fetched once.
        if ws.remote.is_some() && ws.base.is_some_and(|base| store.git_of(&base).is_none()) {
          let _ = crate::remote::fetch_for_export(&mut store, &ws);
        }
        let prefix = start.strip_prefix(dir).map(|p| p.as_os_str().as_bytes().to_vec()).unwrap_or_default();
        // Git counts its own objects. Those are the ones here that have a Git id, when the history came from Git.
        let objects = store.git_count().max(store.stats.objects);
        ABBREV.store(abbrev_for(objects), std::sync::atomic::Ordering::Relaxed);
        return Ok(Repo { ws, store, prefix, _lock });
      }
      match dir.parent() {
        Some(parent) => dir = parent,
        None => return fatal(128, "fatal: not a git repository (or any of the parent directories): .nv"),
      }
    }
  }

  fn meta(&self, name: &str) -> PathBuf {
    self.ws.root.join(".nv").join(name)
  }

  /// The branch HEAD is on, if it is on one.
  fn branch(&self) -> Option<String> {
    let text = std::fs::read_to_string(self.meta("git-head")).ok()?;
    text.trim().strip_prefix("ref: ").map(str::to_string)
  }

  /// Whether the branch follows the branch of its name on `origin`: it was cloned, or made from it.
  fn follows(&self, branch: &str) -> bool {
    std::fs::read_to_string(self.meta("git-upstream")).is_ok_and(|text| text.lines().any(|line| line == branch))
  }

  fn set_follows(&self, branch: &str) -> Res<()> {
    if !self.follows(branch) {
      let mut text = std::fs::read_to_string(self.meta("git-upstream")).unwrap_or_default();
      text.push_str(&format!("{branch}\n"));
      crate::write_whole(&self.meta("git-upstream"), text)?;
    }
    Ok(())
  }

  fn set_branch(&self, name: &str) -> Res<()> {
    crate::write_whole(&self.meta("git-head"), format!("ref: {name}\n"))?;
    Ok(())
  }

  fn branch_tip(&self, name: &str) -> Option<Id> {
    self.store.read_ref(&format!("heads/{name}"))
  }

  fn branches(&self) -> Vec<String> {
    self.store.refs_under("heads").into_iter().map(|(name, _)| name).collect()
  }

  /// Move the branch HEAD is on from `before` to `to`. Nothing to move when HEAD is on no branch.
  fn advance(&mut self, before: Option<Id>, to: Id) -> Res<()> {
    if let Some(name) = self.branch() {
      let expected = self.branch_tip(&name);
      if expected != before || !self.store.compare_and_set_ref(&format!("heads/{name}"), expected, to)? {
        return fatal(128, format!("fatal: branch '{name}' moved while this command ran"));
      }
    }
    Ok(())
  }

  fn config(&self, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(self.meta("git-config")).ok()?;
    text.lines().rev().find_map(|l| l.strip_prefix(key)?.strip_prefix('=').map(str::to_string))
  }

  /// The index, as far as it differs from HEAD: for each path that was
  /// added, removed or moved, the entry a commit would now record (`None`
  /// for no file). Every other path is in the index as HEAD has it.
  fn staged(&self) -> Staged {
    let text = std::fs::read_to_string(self.meta("git-staged")).unwrap_or_default();
    let mut staged = Staged::new();
    for line in text.lines() {
      let mut fields = line.split(' ');
      let Some(path) = fields.next().and_then(unhex) else { continue };
      let entry = match (
        fields.next().and_then(|m| u32::from_str_radix(m, 8).ok()),
        fields.next().and_then(unhex).and_then(|b| Id::try_from(b).ok()),
      ) {
        (Some(mode), Some(id)) => Some(Entry { mode, id }),
        _ => None,
      };
      staged.insert(path, entry);
    }
    staged
  }

  /// The paths a merge could not settle, until each is added.
  fn unmerged(&self) -> std::collections::BTreeSet<Vec<u8>> {
    std::fs::read_to_string(self.meta("git-unmerged")).unwrap_or_default().lines().filter_map(unhex).collect()
  }

  fn set_unmerged(&self, paths: &std::collections::BTreeSet<Vec<u8>>) -> Res<()> {
    let text: String = paths.iter().map(|p| format!("{}\n", hex(p))).collect();
    crate::write_whole(&self.meta("git-unmerged"), text)?;
    Ok(())
  }

  /// The merge under way, if one stopped for conflicts: the commit being merged in, and the message its commit will carry.
  fn merging(&self) -> Option<(Id, String)> {
    let text = std::fs::read_to_string(self.meta("git-merge")).ok()?;
    let (id, message) = text.split_once('\n')?;
    Some((unhex(id).and_then(|b| Id::try_from(b).ok())?, message.to_string()))
  }

  fn end_merge(&self) {
    let _ = std::fs::remove_file(self.meta("git-merge"));
    let _ = std::fs::remove_file(self.meta("git-unmerged"));
  }

  fn set_staged(&self, staged: &Staged) -> Res<()> {
    let mut text = String::new();
    for (path, entry) in staged {
      text.push_str(&match entry {
        Some(entry) => format!("{} {:o} {}\n", hex(path), entry.mode, hex(&entry.id)),
        None => format!("{} -\n", hex(path)),
      });
    }
    crate::write_whole(&self.meta("git-staged"), text)?;
    Ok(())
  }

  /// A path given on the command line, relative to the workspace root.
  /// A path given after `<rev>:`, which Git reads from the top of the tree unless it begins with `./`.
  fn rooted_from_top(&self, path: &str) -> Vec<u8> {
    if path.starts_with("./") || path.starts_with("../") {
      return self.rooted(path);
    }
    path.trim_matches('/').as_bytes().to_vec()
  }

  /// Git's id of any object: a commit, a tag, a directory or a file.
  fn object_oid(&mut self, id: &Id) -> Res<String> {
    Ok(match self.store.kind_of(id)? {
      Some(Kind::Tree) => self.tree_oid(Some(*id))?,
      Some(Kind::Blob) => hex(&crate::git::git_blob_of(&mut self.store, id, &mut |_, _| Ok(()))?),
      _ => self.git_oid(id)?,
    })
  }

  fn rooted(&self, arg: &str) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = self.prefix.split(|&b| b == b'/').filter(|p| !p.is_empty()).collect();
    for part in arg.as_bytes().split(|&b| b == b'/') {
      match part {
        b"" | b"." => {}
        b".." => {
          parts.pop();
        }
        other => parts.push(other),
      }
    }
    parts.join(&b'/')
  }

  /// Git's id for a commit or an annotated tag.
  fn git_oid(&mut self, id: &Id) -> Res<String> {
    Ok(hex(&crate::git::git_id_of(&mut self.store, id)?))
  }

  fn tree_oid(&mut self, dir: Option<Id>) -> Res<String> {
    let mut memo = TreeMemo::default();
    Ok(hex(&git_tree_of(&mut self.store, dir, &mut memo, &mut |_, _| Ok(()))?))
  }

  fn blob_oid(&mut self, id: &Id) -> Res<String> {
    if let Some(oid) = self.store.git_of(id) {
      return Ok(hex(&oid));
    }
    let bytes = self.store.get(id)?.ok_or_else(|| "a blob is missing from the store".to_string())?;
    let oid = git_hash("blob", &bytes);
    self.store.map_git(Kind::Blob, *id, oid)?;
    Ok(hex(&oid))
  }

  /// A commit named the way Git names them: `HEAD`, a branch, a tag, a full id, with `^` and `~N` after.
  fn resolve(&mut self, spec: &str) -> Res<Id> {
    let id = self.resolve_any(spec)?;
    Ok(crate::git::peel(&mut self.store, id)?)
  }

  /// The same, but an annotated tag named and not stepped from is the tag itself.
  fn resolve_any(&mut self, spec: &str) -> Res<Id> {
    let unknown =
      || Fail { code: 128, message: format!("fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.") };
    // Two dots between two names is a range; after a colon they are part of a path.
    if spec.split(':').next().is_some_and(|rev| rev.contains("..")) {
      return unsupported(format_args!("{spec}` (a range of commits)"));
    }
    if spec.contains("@{") {
      return unsupported(format_args!("rev-parse {spec}` (where a ref was before)"));
    }
    if spec.starts_with(":/") || spec.contains("^{/") || (spec.starts_with(':') && spec[1..].starts_with(|c: char| c.is_ascii_digit())) {
      return unsupported(format_args!("rev-parse {spec}` (a commit found by its message, or a stage of the index)"));
    }
    // `<rev>:<path>` is what that commit has at the path: a file or a directory.
    if let Some((rev, path)) = spec.split_once(':') {
      if rev.is_empty() {
        return unsupported(format_args!("rev-parse {spec}` (what the index has at a path)"));
      }
      let commit = self.resolve(rev)?;
      let missing = || Fail { code: 128, message: format!("fatal: path '{path}' does not exist in '{rev}'") };
      if path.starts_with('/') {
        return Err(missing());
      }
      if path.starts_with("./") || path.starts_with("../") {
        self.inside(path)?;
      }
      let root = root_of(&mut self.store, &commit)?.ok_or_else(missing)?;
      let path = self.rooted_from_top(path);
      if path.is_empty() {
        return Ok(root);
      }
      return match crate::manifest::lookup(&mut self.store, root, &path)? {
        Some(entry) if entry.mode != crate::manifest::MODE_GITLINK => Ok(entry.id),
        _ => Err(missing()),
      };
    }
    if let Some(commit) = spec.strip_suffix("^{tree}") {
      let commit = self.resolve(commit)?;
      return root_of(&mut self.store, &commit)?.ok_or_else(unknown);
    }
    let (spec, peeled) = match spec.strip_suffix("^{commit}").or_else(|| spec.strip_suffix("^{}")) {
      Some(inner) => (inner, true),
      None => (spec, false),
    };
    let cut = spec.find(['^', '~']).unwrap_or(spec.len());
    let (name, mut steps) = spec.split_at(cut);
    let full = name.strip_prefix("refs/heads/").or_else(|| name.strip_prefix("heads/"));
    let mut at = if name == "HEAD" || name == "@" {
      self.ws.head
    } else if let Some(tip) = full.and_then(|branch| self.branch_tip(branch)) {
      Some(tip)
    } else if let Some(tag) = name.strip_prefix("refs/tags/").and_then(|tag| self.store.read_ref(&format!("tags/{tag}"))) {
      Some(tag)
    } else if let Some(tip) = self.branch_tip(name) {
      Some(tip)
    } else if let Some(tag) = self.store.read_ref(&format!("tags/{name}")) {
      Some(tag)
    } else if let Some(remote) = self.store.read_ref(&format!("remotes/{name}")) {
      Some(remote)
    } else if name.len() == 40 {
      unhex(name).and_then(|b| <[u8; 20]>::try_from(b).ok()).and_then(|oid| self.store.of_git(&oid))
    } else if (4..40).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_hexdigit()) {
      // The start of an id is the id, if only one object's begins so.
      match self.store.of_git_beginning(&name.to_ascii_lowercase())[..] {
        [one] => Some(one),
        [] => None,
        _ => return fatal(128, format!("error: short object ID {name} is ambiguous\nfatal: ambiguous argument '{name}': unknown revision or path not in the working tree.")),
      }
    } else {
      None
    }
    .ok_or_else(unknown)?;
    if peeled || !steps.is_empty() {
      at = crate::git::peel(&mut self.store, at)?;
    }
    while !steps.is_empty() {
      let digits = steps[1..].find(|c: char| !c.is_ascii_digit()).map_or(steps.len(), |i| i + 1);
      let count: usize = if digits == 1 { 1 } else { steps[1..digits].parse().map_err(|_| unknown())? };
      if steps.starts_with('^') && count != 1 {
        return unsupported(format_args!("rev-parse {spec}"));
      }
      for _ in 0..count {
        at = Commit::load(&mut self.store, &at)?.parents.first().copied().ok_or_else(unknown)?;
      }
      steps = &steps[digits..];
    }
    Ok(at)
  }

  /// Every path where the last commit, the index and the working tree are not all the same.
  fn states(&self) -> Res<std::collections::BTreeMap<Vec<u8>, State>> {
    self.states_at(false, &[Vec::new()])
  }

  /// `states`, for a command that has no use for what is new and unknown: the files
  /// Git tracks, which are those of the last commit and those added since.
  fn states_of_tracked(&self) -> Res<std::collections::BTreeMap<Vec<u8>, State>> {
    let added: Vec<Vec<u8>> = self.staged().into_keys().collect();
    self.states_at(true, &added)
  }

  /// `states`, looking on disk only at the files the index knows (`tracked`) and at
  /// what is at or below `under`. What the index itself says is always all there.
  fn states_at(&self, tracked: bool, under: &[Vec<u8>]) -> Res<std::collections::BTreeMap<Vec<u8>, State>> {
    let (staged, unmerged) = (self.staged(), self.unmerged());
    let (found, _) = workspace::status_at(&self.ws, tracked, under)?;
    let index = self.ws.load_index();
    let mut states = std::collections::BTreeMap::new();
    // Where the working tree differs from the last commit.
    for (path, new, old) in found {
      let in_index = staged.get(&path).copied().unwrap_or(old);
      let same = match (&new, in_index) {
        (Some((mode, bytes)), Some(entry)) => *mode == entry.mode && crate::store::object_id(Kind::Blob, bytes) == entry.id,
        (None, None) => true,
        _ => false,
      };
      let disk = if same { None } else { new.map(|(mode, bytes)| (mode, std::sync::Arc::new(bytes))) };
      let unmerged = unmerged.contains(&path);
      states.insert(path, State { head: old, index: in_index, disk, disk_is_index: same, unmerged });
    }
    // Where it does not, but the index does.
    for (path, entry) in staged {
      let head = index.get(&path).map(|known| known.entry);
      states.entry(path).or_insert(State { head, index: entry, disk: None, disk_is_index: head == entry, unmerged: false });
    }
    // Unmerged whatever the file now holds, until it is added.
    for path in unmerged {
      let head = index.get(&path).map(|known| known.entry);
      states.entry(path).or_insert(State { head, index: head, disk: None, disk_is_index: true, unmerged: true });
    }
    // A path whose three places agree after all is not a difference.
    states.retain(|_, s| s.unmerged || s.head != s.index || !s.disk_is_index);
    Ok(states)
  }

  fn blob(&mut self, id: &Id) -> Res<std::sync::Arc<Vec<u8>>> {
    Ok(self.store.get(id)?.ok_or_else(|| "a blob is missing from the store".to_string())?)
  }
}

/// A path as Git prints it: in double quotes with C escapes when it holds
/// bytes Git will not print bare. `status` also quotes a path with a space.
pub fn quoted(path: &[u8], and_spaces: bool) -> Vec<u8> {
  let plain = |b: u8| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\';
  if path.iter().all(|&b| plain(b)) && !(and_spaces && path.contains(&b' ')) {
    return path.to_vec();
  }
  let mut out = vec![b'"'];
  for &b in path {
    match b {
      b'"' | b'\\' => out.extend_from_slice(&[b'\\', b]),
      b'\n' => out.extend_from_slice(b"\\n"),
      b'\t' => out.extend_from_slice(b"\\t"),
      b if plain(b) => out.push(b),
      b => out.extend_from_slice(format!("\\{b:03o}").as_bytes()),
    }
  }
  out.push(b'"');
  out
}

/// The entries of a directory in the order Git keeps them: as if directory names ended in a slash.
fn git_ordered(store: &mut Store, dir: &Id) -> Res<Vec<(Vec<u8>, Entry)>> {
  let mut entries: Vec<(Vec<u8>, Entry)> = load(store, dir)?.into_iter().collect();
  entries.sort_by_cached_key(|(name, e)| [name.as_slice(), if e.is_dir() { b"/" } else { b"" }].concat());
  Ok(entries)
}

fn kind_name(entry: &Entry) -> &'static str {
  match entry.mode {
    m if m == crate::manifest::MODE_DIR => "tree",
    MODE_GITLINK => "commit",
    _ => "blob",
  }
}

/// `author`/`committer` identity and date: from the environment, then from `git config`.
fn identity(repo: &Repo, role: &str) -> Res<String> {
  let var = |name: &str| std::env::var(format!("GIT_{role}_{name}")).ok().filter(|v| !v.is_empty());
  // The workspace's own setting, then the user's Git configuration, as Git itself would look.
  let set = |key: &str| repo.config(&format!("user.{key}")).or_else(|| crate::git::config_value(&repo.ws.root, "user", key));
  let (name, email) = (var("NAME").or_else(|| set("name")), var("EMAIL").or_else(|| set("email")));
  let (Some(name), Some(email)) = (name, email) else {
    return fatal(128, "fatal: unable to auto-detect email address: set user.name and user.email with `git config`");
  };
  let date = match var("DATE") {
    Some(date) => match crate::git::stamp(&date) {
      Some(stamp) => stamp,
      None => {
        return unsupported(format_args!("commit with GIT_{role}_DATE={date}` (seconds and a zone, or a date as ISO 8601 writes it)"));
      }
    },
    None => format!("{} +0000", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())),
  };
  Ok(format!("{name} <{email}> {date}"))
}

/// What a commit's verbatim tail says: `(author, committer, message)`.
struct Told {
  author: String,
  committer: String,
  message: String,
}

fn told(rest: &[u8]) -> Told {
  let text = String::from_utf8_lossy(rest);
  let (headers, message) = text.split_once("\n\n").unwrap_or((&text, ""));
  let field = |name: &str| headers.lines().find_map(|l| l.strip_prefix(name)).unwrap_or("").to_string();
  Told { author: field("author "), committer: field("committer "), message: message.to_string() }
}

/// `Name <email> seconds zone` taken apart.
fn who(identity: &str) -> (&str, &str, i64, &str) {
  let (name, rest) = identity.split_once(" <").unwrap_or((identity, ""));
  let (email, rest) = rest.split_once("> ").unwrap_or((rest, ""));
  let (seconds, zone) = rest.split_once(' ').unwrap_or((rest, "+0000"));
  (name, email, seconds.parse().unwrap_or(0), zone)
}

/// A date as `git log` prints it by default, in the zone it was made in.
fn date(seconds: i64, zone: &str) -> String {
  let sign = if zone.starts_with('-') { -1 } else { 1 };
  let digits: i64 = zone[1..].parse().unwrap_or(0);
  let local = seconds + sign * (digits / 100 * 3600 + digits % 100 * 60);
  let (days, secs) = (local.div_euclid(86400), local.rem_euclid(86400));
  // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
  let z = days + 719_468;
  let era = z.div_euclid(146_097);
  let doe = z.rem_euclid(146_097);
  let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let day = doy - (153 * mp + 2) / 5 + 1;
  let month = if mp < 10 { mp + 3 } else { mp - 9 };
  let year = yoe + era * 400 + (month <= 2) as i64;
  let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
  let name = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(month - 1) as usize];
  format!("{weekday} {name} {day} {:02}:{:02}:{:02} {year} {zone}", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// Every command spoken, with the options it takes. An option is written as
/// it is typed; `X=` takes the next argument as its value, a trailing `*` is
/// a value attached to the option, and `#` stands for a number. Arguments that are not
/// options (paths, revisions, names) are for the command to judge.
pub const SPOKEN: &[(&str, &[&str])] = &[
  ("init", &["-q", "--quiet", "-b=", "--initial-branch=", "--initial-branch=*"]),
  ("clone", &["-q", "--quiet"]),
  ("add", &["-A", "--all", "--"]),
  ("rm", &["-r", "-q", "--quiet", "-f", "--force", "--"]),
  ("mv", &[]),
  ("restore", &["--staged", "--"]),
  ("reset", &["-q", "--quiet", "--hard", "--soft", "--mixed", "--"]),
  ("commit", &["-q", "--quiet", "-a", "--all", "--allow-empty", "--amend", "--no-edit", "-m=", "-am=", "-qm=", "-qam=", "-aqm="]),
  ("status", &["--porcelain", "--short", "-s"]),
  ("log", &["--oneline", "--format=*", "--pretty=format:*", "--pretty=tformat:*", "-n=", "-n#", "-#"]),
  ("show", &["--name-status", "--name-only", "--stat", "--format=*", "--pretty=format:*", "--pretty=tformat:*"]),
  ("diff", &["--name-status", "--name-only", "--stat", "--cached", "--staged"]),
  ("rev-parse", &["--short", "--abbrev-ref", "--show-toplevel"]),
  ("cat-file", &["-t", "-p", "-e"]),
  ("ls-tree", &["-r", "--name-only"]),
  ("branch", &["--show-current", "-d", "-D"]),
  ("tag", &["-d", "-l", "--list", "-a", "-m="]),
  ("checkout", &["-q", "--quiet", "-b=", "--"]),
  ("switch", &["-q", "--quiet", "-c="]),
  ("merge", &["-q", "--quiet", "--no-ff", "--ff-only", "--no-edit", "--abort", "-m="]),
  ("config", &["--get", "--local"]),
  ("fsck", &["--full", "--strict", "--no-dangling"]),
  ("fetch", &["-q", "--quiet"]),
  ("pull", &["-q", "--quiet", "--ff-only", "--no-rebase"]),
  ("push", &["-q", "--quiet"]),
];

/// Check an invocation against `SPOKEN`. Whatever passes is something a
/// command was written to handle.
fn permitted(command: &str, args: &[&str]) -> Res<()> {
  let Some((_, options)) = SPOKEN.iter().find(|(name, _)| *name == command) else { return unsupported(command) };
  let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
  let mut it = args.iter();
  while let Some(&arg) = it.next() {
    if !arg.starts_with('-') || arg == "-" {
      continue;
    }
    let spoken = options.iter().find(|option| {
      if let Some(prefix) = option.strip_suffix('*') {
        arg.starts_with(prefix)
      } else if let Some(prefix) = option.strip_suffix('#') {
        arg.strip_prefix(prefix).is_some_and(digits)
      } else {
        option.strip_suffix('=').unwrap_or(option) == arg
      }
    });
    match spoken {
      // The option's value is whatever comes next, even if it looks like an option.
      Some(option) if option.ends_with('=') => {
        if it.next().is_none() {
          return fatal(129, format!("error: option `{arg}` requires a value"));
        }
      }
      Some(_) if arg == "--" => break,
      Some(_) => {}
      None => return unsupported(format_args!("{command} {arg}")),
    }
  }
  Ok(())
}

pub fn run(args: &[String]) -> i32 {
  let mut args = args;
  let mut at = PathBuf::from(".");
  while let [flag, dir, rest @ ..] = args {
    if flag != "-C" {
      break;
    }
    at = at.join(dir);
    args = rest;
  }
  let Some((command, rest)) = args.split_first() else {
    eprintln!("usage: nv git <command> [<args>]");
    return 1;
  };
  // Which Git this stands in for is a question about Git: tools ask it before anything else.
  if matches!(command.as_str(), "--version" | "version" | "--help" | "help") {
    let real = std::env::var_os("NV_REAL_GIT").unwrap_or_else(|| "git".into());
    return std::process::Command::new(real).args(args).status().map_or(127, |status| status.code().unwrap_or(1));
  }
  // The table itself, for whoever wants to know before running a script.
  if command == "--spoken" {
    for (name, options) in SPOKEN {
      println!("{name} {}", options.join(" "));
    }
    return 0;
  }
  // Where a Git repository has the directory, Git answers: whatever it speaks, with its own
  // index, configuration and hooks. nv's own commands follow it; nothing here stands in for it.
  let real_git = |at: &Path| -> i32 {
    use std::os::unix::process::CommandExt;
    let real = std::env::var_os("NV_REAL_GIT").unwrap_or_else(|| "git".into());
    let failed = std::process::Command::new(real).arg("-C").arg(at).args(args).exec();
    eprintln!("nv: cannot run git: {failed}");
    127
  };
  // Told where a Git repository is, by an option or by the environment: that is a question for Git.
  if command.starts_with("--git-dir") || command.starts_with("--work-tree") || std::env::var_os("GIT_DIR").is_some() {
    return real_git(&at);
  }
  if !matches!(command.as_str(), "init" | "clone")
    && let Ok(start) = std::fs::canonicalize(&at)
  {
    for dir in start.ancestors() {
      let (workspace, repository) = (dir.join(".nv").join("workspace").is_file(), dir.join(".git").exists());
      if repository {
        return real_git(&at);
      }
      if workspace {
        break;
      }
    }
  }
  let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
  let open = || Repo::open(&at);
  let result = permitted(command, &rest).and_then(|()| match command.as_str() {
    "init" => init(&at, &rest),
    "clone" => clone(&at, &rest),
    "add" => add(open()?, &rest),
    "rm" => rm(open()?, &rest),
    "mv" => mv(open()?, &rest),
    "restore" => restore(open()?, &rest),
    "reset" => reset(open()?, &rest),
    "commit" => commit(open()?, &rest),
    "status" => status(open()?, &rest),
    "log" => log(open()?, &rest),
    "show" => show(open()?, &rest),
    "diff" => diff_command(open()?, &rest),
    "rev-parse" => rev_parse(open()?, &rest),
    "cat-file" => cat_file(open()?, &rest),
    "ls-tree" => ls_tree(open()?, &rest),
    "branch" => branch(open()?, &rest),
    "tag" => tag(open()?, &rest),
    "checkout" => checkout(open()?, &rest, "-b", "checkout"),
    "switch" => checkout(open()?, &rest, "-c", "switch"),
    "merge" => merge(open()?, &rest),
    "config" => config(open()?, &rest),
    "fsck" => fsck(open()?),
    "fetch" => fetch(&mut open()?).map(|_| 0),
    "pull" => pull(open()?, &rest),
    "push" => push(open()?, &rest),
    other => unsupported(other),
  });
  match result {
    Ok(code) => code,
    Err(fail) => {
      if !fail.message.is_empty() {
        eprintln!("{}", fail.message);
      }
      fail.code
    }
  }
}

/// How many characters of an id Git prints when it prints a short one: seven, and more in a
/// repository with enough objects that seven would soon not tell them apart.
static ABBREV: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(7);

fn abbrev() -> usize {
  ABBREV.load(std::sync::atomic::Ordering::Relaxed)
}

/// Git's rule: half the number of bits it takes to count the objects, rounded up, and never under seven.
fn abbrev_for(objects: usize) -> usize {
  let bits = usize::BITS - objects.max(1).leading_zeros() - 1;
  (bits as usize).div_ceil(2).clamp(7, 40)
}

/// A message as Git keeps one given on the command line (`git stripspace`): no blank lines
/// before it or after it, no run of blank lines inside it, no spaces at the end of a line.
fn cleaned(message: &str) -> String {
  let mut out: Vec<&str> = Vec::new();
  for line in message.lines().map(str::trim_end) {
    if line.is_empty() && out.last().is_none_or(|last| last.is_empty()) {
      continue;
    }
    out.push(line);
  }
  while out.last().is_some_and(|last| last.is_empty()) {
    out.pop();
  }
  out.join("\n")
}

/// Whether Git would take this as the name of a branch or a tag (`git check-ref-format`).
fn ref_name_ok(name: &str) -> bool {
  let part_ok = |part: &str| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock");
  !name.is_empty()
    && name != "@"
    && name != "HEAD"
    && !name.starts_with('-')
    && !name.ends_with('.')
    && !name.contains("..")
    && !name.contains("@{")
    && !name.bytes().any(|b| b <= b' ' || b == 0x7f || b"~^:?*[\\".contains(&b))
    && name.split('/').all(part_ok)
}

impl Repo {
  /// Refuse a name Git would refuse, for a branch (`heads`) or a tag (`tags`): one that is
  /// not a name, or one that is, or is inside, a name there already.
  fn new_ref(&self, kind: &str, name: &str) -> Res<()> {
    let what = if kind == "heads" { "branch" } else { "tag" };
    if !ref_name_ok(name) {
      return fatal(128, format!("fatal: '{name}' is not a valid {what} name"));
    }
    let taken = self.store.refs_under(kind).into_iter().map(|(other, _)| other).find(|other| {
      let inside = |a: &str, b: &str| a.len() > b.len() && a.starts_with(b) && a.as_bytes()[b.len()] == b'/';
      inside(other, name) || inside(name, other)
    });
    match taken {
      Some(other) => {
        fatal(128, format!("fatal: cannot lock ref 'refs/{kind}/{name}': 'refs/{kind}/{other}' exists; cannot create 'refs/{kind}/{name}'"))
      }
      None => Ok(()),
    }
  }

  /// A path of the repository as Git shows it to someone standing where the command was run.
  fn shown(&self, path: &[u8]) -> Vec<u8> {
    let here: Vec<&[u8]> = self.prefix.split(|&b| b == b'/').filter(|part| !part.is_empty()).collect();
    let there: Vec<&[u8]> = path.split(|&b| b == b'/').collect();
    let shared = here.iter().zip(&there).take_while(|(a, b)| a == b).count().min(there.len().saturating_sub(1));
    let mut parts: Vec<&[u8]> = vec![b".."; here.len() - shared];
    parts.extend(&there[shared..]);
    parts.join(&b'/')
  }
}

fn quiet(args: &[&str]) -> bool {
  args.iter().any(|a| *a == "-q" || *a == "--quiet")
}

/// The arguments that are not options: after `permitted`, what is left once
/// the options and their values are taken away.
fn positional<'a>(command: &str, args: &[&'a str]) -> Vec<&'a str> {
  let options = SPOKEN.iter().find(|(name, _)| *name == command).map_or(&[][..], |(_, o)| *o);
  let mut out = Vec::new();
  let mut it = args.iter();
  let mut only_paths = false;
  while let Some(&arg) = it.next() {
    if only_paths || !arg.starts_with('-') {
      out.push(arg);
    } else if arg == "--" {
      only_paths = true;
    } else if options.iter().any(|o| o.ends_with('=') && o.strip_suffix('=') == Some(arg)) {
      it.next();
    }
  }
  out
}

fn value<'a>(args: &[&'a str], names: &[&str]) -> Option<&'a str> {
  args.iter().position(|a| names.contains(a)).and_then(|i| args.get(i + 1)).copied()
}

fn has(args: &[&str], name: &str) -> bool {
  args.contains(&name)
}

fn init(at: &Path, args: &[&str]) -> Res<i32> {
  let attached = args.iter().find_map(|a| a.strip_prefix("--initial-branch="));
  let configured = crate::git::config_value(at, "init", "defaultbranch");
  let branch = attached.or_else(|| value(args, &["-b", "--initial-branch"])).or(configured.as_deref()).unwrap_or("master");
  if !ref_name_ok(branch) {
    return fatal(128, format!("fatal: invalid initial branch name: '{branch}'"));
  }
  let root = match positional("init", args)[..] {
    [] => at.to_path_buf(),
    [dir] => at.join(dir),
    _ => return unsupported("init` with more than one directory"),
  };
  std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
  if root.join(".nv").join("workspace").exists() {
    return unsupported("init` in a directory that already has a repository");
  }
  workspace::init(&root)?;
  crate::write_whole(&root.join(".nv").join("git-head"), format!("ref: {branch}\n"))?;
  if !quiet(args) {
    let full = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
    println!("Initialized empty nv repository in {}/.nv/", full.display());
  }
  Ok(0)
}

/// Bring a Git repository's branches and tags into the store, as `origin`'s.
/// Tags become this repository's tags too, as Git has it.
fn fetch(repo: &mut Repo) -> Res<crate::git::Refs> {
  let Ok(origin) = std::fs::read_to_string(repo.meta("git-origin")) else {
    return fatal(128, "fatal: 'origin' does not appear to be a git repository");
  };
  let refs = import_refs(&mut repo.store, Path::new(origin.trim()))?;
  for (name, id) in &refs.heads {
    repo.store.set_ref(&format!("remotes/origin/{name}"), *id)?;
  }
  for (name, id) in &refs.tags {
    repo.store.set_ref(&format!("tags/{name}"), *id)?;
  }
  Ok(refs)
}

fn clone(at: &Path, args: &[&str]) -> Res<i32> {
  let (source, dir) = match positional("clone", args)[..] {
    // Into a directory named after the source, without the `.git` a bare repository's name ends in.
    [source] => {
      let name = Path::new(source.trim_end_matches('/')).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
      (source, PathBuf::from(name.strip_suffix(".git").filter(|n| !n.is_empty()).unwrap_or(&name)))
    }
    [source, dir] => (source, PathBuf::from(dir)),
    _ => return unsupported("clone` without a source and at most one directory"),
  };
  // As it was typed, made absolute and no more: Git writes this path into the
  // message of a merge a pull makes, and a commit's id is over its message.
  let source = source.strip_suffix('/').filter(|rest| !rest.is_empty()).unwrap_or(source);
  let origin = if Path::new(source).is_absolute() {
    PathBuf::from(source)
  } else {
    // Joined as text: Git keeps a `./` or `../` that was typed, and so must the message a pull writes.
    let here = std::env::current_dir().map_err(|e| e.to_string())?;
    let here = if at == Path::new(".") { here } else { here.join(at) };
    PathBuf::from(format!("{}/{source}", here.display()))
  };
  if !origin.is_dir() {
    return unsupported(format_args!("clone {source}` (only a Git repository on this machine)"));
  }
  let root = at.join(dir);
  if root.exists() && std::fs::read_dir(&root).is_ok_and(|mut d| d.next().is_some()) {
    return fatal(128, format!("fatal: destination path '{}' already exists and is not an empty directory.", root.display()));
  }
  let was_there = root.exists();
  let done = cloned(&root, &origin);
  if done.is_err() {
    // A clone that did not come about leaves nothing that would pass for a repository.
    let _ = if was_there { std::fs::remove_dir_all(root.join(".nv")) } else { std::fs::remove_dir_all(&root) };
  }
  done
}

fn cloned(root: &Path, origin: &Path) -> Res<i32> {
  std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
  workspace::init(root)?;
  crate::write_whole(&root.join(".nv").join("git-origin"), origin.as_os_str().as_bytes())?;
  let mut repo = Repo::open(root)?;
  let refs = fetch(&mut repo)?;
  // One local branch, the one the source is on, and the files of its tip.
  let branch = refs.current.clone().unwrap_or_else(|| "master".to_string());
  repo.set_branch(&branch)?;
  repo.set_follows(&branch)?;
  if let Some((_, tip)) = refs.heads.iter().find(|(name, _)| *name == branch) {
    repo.store.set_ref(&format!("heads/{branch}"), *tip)?;
    workspace::switch(&mut repo.store, &mut repo.ws, *tip)?;
    // The commit the workspace was made at: where it goes back to if what it does next cannot go on a server's ref.
    repo.ws.start = Some(*tip);
    repo.ws.save()?;
  }
  Ok(0)
}

fn pull(mut repo: Repo, args: &[&str]) -> Res<i32> {
  fetch(&mut repo)?;
  let Some(branch) = repo.branch() else { return fatal(1, "You are not currently on a branch.") };
  let Some(theirs) = repo.store.read_ref(&format!("remotes/origin/{branch}")) else {
    return fatal(
      1,
      format!("Your configuration specifies to merge with the ref 'refs/heads/{branch}'\nfrom the remote, but no such ref was fetched."),
    );
  };
  // Where both sides have commits the other lacks, Git wants to be told how to join them.
  if let Some(ours) = repo.ws.head {
    let diverged = !ancestors(&mut repo.store, theirs)?.contains(&ours) && !ancestors(&mut repo.store, ours)?.contains(&theirs);
    if diverged && !has(args, "--no-rebase") {
      return fatal(128, "fatal: Need to specify how to reconcile divergent branches.");
    }
  }
  let origin = std::fs::read_to_string(repo.meta("git-origin")).unwrap_or_default();
  let into = Some(&branch).filter(|b| *b != "main" && *b != "master").map(|b| format!(" into {b}")).unwrap_or_default();
  // Git names where it pulled from without the `.git` a repository's directory may end in.
  let from = origin.trim();
  let from = from.strip_suffix(".git").unwrap_or(from);
  let message = format!("Merge branch '{branch}' of {from}{into}");
  let target = format!("origin/{branch}");
  let mut forwarded: Vec<&str> = args.iter().copied().filter(|a| *a == "-q" || *a == "--quiet" || *a == "--ff-only").collect();
  forwarded.extend(["-m", &message, &target]);
  merge(repo, &forwarded)
}

/// Write this branch's commits into the Git repository it was cloned from
/// and move the branch there, if that is a step forward for it.
fn push(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let Ok(origin) = std::fs::read_to_string(repo.meta("git-origin")) else {
    return fatal(128, "fatal: 'origin' does not appear to be a git repository");
  };
  let origin = PathBuf::from(origin.trim());
  let (Some(branch), Some(head)) = (repo.branch(), repo.ws.head) else {
    return fatal(128, "fatal: You are not currently on a branch with a commit.");
  };
  // One thing is pushed: the branch one is on, to the branch of that name on `origin`.
  match positional("push", args)[..] {
    [] | ["origin"] if !repo.follows(&branch) => {
      return fatal(
        128,
        format!(
          "fatal: The current branch {branch} has no upstream branch.\nTo push the current branch and set the remote as upstream, use\n\n    git push --set-upstream origin {branch}\n\nTo have this happen automatically for branches without a tracking\nupstream, see 'push.autoSetupRemote' in 'git help config'.\n"
        ),
      );
    }
    [] | ["origin"] => {}
    ["origin", named] if named == branch => {}
    _ => return unsupported(format_args!("push {}` (only the current branch, to the branch of its name on origin)", args.join(" "))),
  }
  let ask = |args: &[&str]| -> String {
    let out = crate::git::git(&origin).args(args).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
  };
  // Git refuses to move a branch that a working tree is sitting on.
  if ask(&["rev-parse", "--is-bare-repository"]) != "true" && ask(&["symbolic-ref", "-q", "--short", "HEAD"]) == branch {
    return fatal(1, format!("error: refusing to update checked out branch: refs/heads/{branch}"));
  }
  let theirs = ask(&["rev-parse", "-q", "--verify", &format!("refs/heads/{branch}")]);
  if !theirs.is_empty() {
    let known = unhex(&theirs).and_then(|b| <[u8; 20]>::try_from(b).ok()).and_then(|oid| repo.store.of_git(&oid));
    if !known.is_some_and(|theirs| ancestors(&mut repo.store, head).is_ok_and(|a| a.contains(&theirs))) {
      return fatal(
        1,
        format!(
          "error: failed to push some refs\nhint: Updates were rejected because the remote contains work that you do not have locally: fetch first ({branch})"
        ),
      );
    }
  }
  crate::git::export(&mut repo.store, &head, &origin, &format!("refs/heads/{branch}"))?;
  repo.store.set_ref(&format!("remotes/origin/{branch}"), head)?;
  Ok(0)
}

fn under(paths: &[Vec<u8>], path: &[u8]) -> bool {
  paths.iter().any(|p| p.is_empty() || p == path || (path.len() > p.len() && path.starts_with(p) && path[p.len()] == b'/'))
}

/// Put into the index what is on disk now, for the paths of `states` that `pick` chooses.
fn stage(repo: &mut Repo, states: &std::collections::BTreeMap<Vec<u8>, State>, pick: &dyn Fn(&[u8], &State) -> bool) -> Res<()> {
  let mut staged = repo.staged();
  let mut unmerged = repo.unmerged();
  for (path, state) in states.iter().filter(|(path, state)| pick(path, state) && (state.unmerged || !state.disk_is_index)) {
    // Adding an unmerged file says the file is now as it should be.
    unmerged.remove(path);
    let entry = match &state.disk {
      Some((mode, bytes)) => Some(Entry { mode: *mode, id: repo.store.put(Kind::Blob, bytes, state.index.or(state.head).map(|e| e.id))? }),
      None if state.disk_is_index => state.index,
      // Known only as staged (a removal, say), and there on disk: what the look over the files did
      // not list is as the last commit has it.
      // (A directory standing there is not the file.)
      None if std::fs::symlink_metadata(repo.ws.root.join(std::ffi::OsStr::from_bytes(path))).is_ok_and(|meta| !meta.is_dir()) => {
        state.head
      }
      None => None,
    };
    // An index entry that says what HEAD says is no entry at all.
    if entry == state.head {
      staged.remove(path);
    } else {
      staged.insert(path.clone(), entry);
    }
  }
  repo.store.flush()?;
  if repo.merging().is_some() {
    repo.set_unmerged(&unmerged)?;
  }
  repo.set_staged(&staged)
}

fn add(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let given = positional("add", args);
  // A pattern is Git's to expand, by rules not spoken here.
  if let Some(pattern) =
    given.iter().find(|a| a.contains(['*', '?', '[']) && !repo.ws.root.join(std::ffi::OsStr::from_bytes(&repo.rooted(a))).exists())
  {
    return unsupported(format_args!("add {pattern}` (a pattern)"));
  }
  // What is nv's own is not a file of the tree, as `.git` is not to Git: named, it is passed over.
  let given: Vec<&str> = given.into_iter().filter(|a| !own_place(&repo.rooted(a))).collect();
  let mut paths: Vec<Vec<u8>> = given.iter().map(|a| repo.inside(a)).collect::<Res<_>>()?;
  for (path, arg) in paths.iter().zip(&given) {
    if workspace::link_on_the_way(&repo.ws.root, path).is_some() {
      return fatal(128, format!("fatal: pathspec '{arg}' is beyond a symbolic link"));
    }
  }
  if has(args, "-A") || has(args, "--all") {
    paths.push(Vec::new());
  }
  if paths.is_empty() {
    println!("Nothing specified, nothing added.");
    return Ok(0);
  }
  // A path has to be something: on disk, or known and now gone.
  let known: Vec<Vec<u8>> = repo.ws.load_index().entries().into_iter().map(|(p, _)| p).chain(repo.staged().into_keys()).collect();
  for (path, arg) in paths.iter().zip(&given) {
    let on_disk = std::fs::symlink_metadata(repo.ws.root.join(std::ffi::OsStr::from_bytes(path))).is_ok();
    if !path.is_empty() && !on_disk && !known.iter().any(|k| under(std::slice::from_ref(path), k)) {
      return fatal(128, format!("fatal: pathspec '{arg}' did not match any files"));
    }
  }
  // Only what was named is looked at: adding a file does not walk the tree.
  let states = repo.states_at(false, &paths)?;
  // Naming a file the rules ignore is refused; a directory that holds some just leaves them out.
  let ignored: Vec<&str> = paths
    .iter()
    .zip(&given)
    .filter(|(path, _)| !path.is_empty() && !known.iter().any(|k| under(std::slice::from_ref(path), k)))
    .filter(|(path, _)| crate::ignore::is_ignored(&repo.ws.root, path))
    .map(|(_, arg)| *arg)
    .collect();
  stage(&mut repo, &states, &|path, _| under(&paths, path))?;
  // A path inside what was a tracked file and is now a directory: the file is gone, and Git
  // stages that with the first thing added below it.
  let index = repo.ws.load_index();
  let mut staged = repo.staged();
  let before = staged.len();
  for path in &paths {
    let mut upto = 0;
    while let Some(slash) = path[upto..].iter().position(|&b| b == b'/') {
      let parent = &path[..upto + slash];
      upto += slash + 1;
      let is_dir = std::fs::symlink_metadata(repo.ws.root.join(std::ffi::OsStr::from_bytes(parent))).is_ok_and(|meta| meta.is_dir());
      if is_dir && index.get(parent).is_some() && staged.get(parent) != Some(&None) {
        staged.insert(parent.to_vec(), None);
      }
    }
  }
  if staged.len() != before {
    repo.set_staged(&staged)?;
  }
  if !ignored.is_empty() {
    return fatal(
      1,
      format!(
        "The following paths are ignored by one of your .gitignore files:\n{}\nhint: Use -f if you really want to add them.\nhint: Disable this message with \"git config set advice.addIgnoredFile false\"",
        ignored.join("\n")
      ),
    );
  }
  Ok(0)
}

fn rm(repo: Repo, args: &[&str]) -> Res<i32> {
  let states = repo.states()?;
  let mut staged = repo.staged();
  // What the index has: HEAD's files and what was added, less what was removed.
  let mut known: Vec<Vec<u8>> =
    repo.ws.load_index().entries().into_iter().map(|(p, _)| p).filter(|p| staged.get(p) != Some(&None)).collect();
  known.extend(staged.iter().filter(|(_, e)| e.is_some()).map(|(p, _)| p.clone()));
  known.sort();
  known.dedup();
  if positional("rm", args).is_empty() {
    return fatal(128, "fatal: No pathspec was given. Which files should I remove?");
  }
  let mut doomed: Vec<Vec<u8>> = Vec::new();
  for arg in positional("rm", args) {
    let path = repo.inside(arg)?;
    // Beyond a link is somewhere else's file: Git removes it there when forced, and nv does not go
    // there. A link that is itself a tracked file is removed as one.
    let tracked_link = known.contains(&path)
      && std::fs::symlink_metadata(repo.ws.root.join(std::ffi::OsStr::from_bytes(&path))).is_ok_and(|meta| meta.file_type().is_symlink());
    if (!tracked_link && workspace::link_on_the_way(&repo.ws.root, &[path.as_slice(), b"/."].concat()).is_some())
      || workspace::link_on_the_way(&repo.ws.root, &path).is_some()
    {
      return unsupported(format_args!("rm {arg}` (a path at or beyond a symbolic link that stands for a directory)"));
    }
    let files: Vec<&Vec<u8>> = known.iter().filter(|k| under(std::slice::from_ref(&path), k)).collect();
    if files.is_empty() {
      return fatal(128, format!("fatal: pathspec '{arg}' did not match any files"));
    }
    if files.iter().any(|f| **f != path) && !has(args, "-r") {
      return fatal(128, format!("fatal: not removing '{arg}' recursively without -r"));
    }
    doomed.extend(files.into_iter().cloned());
  }
  if !has(args, "-f") && !has(args, "--force") {
    // Git will not throw away what is in neither the last commit nor, for the file's edits, the index.
    let changed: Vec<String> = doomed
      .iter()
      .filter(|p| states.get(*p).is_some_and(|s| !s.unmerged && (s.unstaged() == Some('M') || s.staged().is_some())))
      .map(|p| String::from_utf8_lossy(p).into_owned())
      .collect();
    if !changed.is_empty() {
      return fatal(
        1,
        format!(
          "error: the following file has local modifications:\n    {}\n(use --cached to keep the file, or -f to force removal)",
          changed.join("\n    ")
        ),
      );
    }
  }
  doomed.sort();
  if repo.merging().is_some() {
    let mut unmerged = repo.unmerged();
    unmerged.retain(|path| !doomed.contains(path));
    repo.set_unmerged(&unmerged)?;
  }
  let index = repo.ws.load_index();
  for path in &doomed {
    let full = repo.ws.root.join(std::ffi::OsStr::from_bytes(path));
    let _ = std::fs::remove_file(&full);
    // Directories this empties go too, as with Git: except the one the command was run in,
    // which is someone's place to stand.
    let standing = repo.ws.root.join(std::ffi::OsStr::from_bytes(&repo.prefix));
    let mut dir = full.parent();
    while let Some(d) = dir.filter(|d| *d != repo.ws.root && *d != standing) {
      if std::fs::remove_dir(d).is_err() {
        break;
      }
      dir = d.parent();
    }
    if index.get(path).is_some() {
      staged.insert(path.clone(), None);
    } else {
      staged.remove(path);
    }
    if !quiet(args) {
      out(&[b"rm '".as_slice(), path, b"'\n"].concat());
    }
  }
  repo.set_staged(&staged)?;
  Ok(0)
}

fn mv(repo: Repo, args: &[&str]) -> Res<i32> {
  let [from_arg, to_arg] = positional("mv", args)[..] else { return unsupported("mv` without exactly a source and a destination") };
  let (from, mut to) = (repo.inside(from_arg)?, repo.inside(to_arg)?);
  for (path, arg) in [(&from, from_arg), (&to, to_arg)] {
    if workspace::link_on_the_way(&repo.ws.root, path).is_some() {
      return unsupported(format_args!("mv {arg}` (a path beyond a symbolic link)"));
    }
  }
  if own_place(&to) || own_place(&from) {
    return unsupported(format_args!("mv {from_arg} {to_arg}` (into or out of nv's own directory)"));
  }
  if repo.unmerged().iter().any(|path| under(std::slice::from_ref(&from), path)) {
    return fatal(128, format!("fatal: conflicted, source={from_arg}, destination={to_arg}"));
  }
  let mut staged = repo.staged();
  let index = repo.ws.load_index();
  // The index's entry for each file that moves: what was added, or what HEAD has.
  let moving: Vec<(Vec<u8>, Entry)> = index
    .entries()
    .into_iter()
    .map(|(path, known)| (path, Some(known.entry)))
    .chain(staged.iter().map(|(p, e)| (p.clone(), *e)))
    .filter(|(path, _)| under(std::slice::from_ref(&from), path))
    .collect::<std::collections::BTreeMap<_, _>>()
    .into_iter()
    .filter_map(|(path, entry)| Some((path, entry?)))
    .collect();
  if moving.is_empty() {
    return fatal(128, format!("fatal: not under version control, source={from_arg}, destination={to_arg}"));
  }
  let full = |path: &[u8]| repo.ws.root.join(std::ffi::OsStr::from_bytes(path));
  if to_arg.ends_with('/') && !full(&to).is_dir() {
    return fatal(128, "fatal: destination directory does not exist");
  }
  // Into a directory that is there, under its own name.
  if full(&to).is_dir() {
    let name = from.rsplit(|&b| b == b'/').next().unwrap_or(&from).to_vec();
    to = [to.as_slice(), b"/", &name].concat();
  }
  if full(&to).exists() {
    return fatal(128, format!("fatal: destination exists, source={from_arg}, destination={to_arg}"));
  }
  if full(&to).parent().is_some_and(|parent| !parent.is_dir()) {
    return fatal(128, format!("fatal: renaming '{from_arg}' failed: No such file or directory"));
  }
  std::fs::rename(full(&from), full(&to)).map_err(|e| format!("renaming '{from_arg}' failed: {e}"))?;
  // The index entry moves with the file; edits not yet added stay not added, under the new name.
  for (path, entry) in moving {
    let moved = [to.as_slice(), &path[from.len()..]].concat();
    if index.get(&path).is_some() {
      staged.insert(path, None);
    } else {
      staged.remove(&path);
    }
    staged.insert(moved, Some(entry));
  }
  repo.set_staged(&staged)?;
  Ok(0)
}

/// Take these paths out of the index: back to what HEAD has for them.
/// The index back to HEAD for these paths. In a merge that stopped, a path unstaged is no longer
/// unmerged either: its index entry is HEAD's, and what the merge left is an edit in the file.
fn unstage(repo: &Repo, paths: &[Vec<u8>]) -> Res<()> {
  let mut staged = repo.staged();
  staged.retain(|path, _| !under(paths, path));
  repo.set_staged(&staged)?;
  if repo.merging().is_some() {
    let mut unmerged = repo.unmerged();
    unmerged.retain(|path| !under(paths, path));
    repo.set_unmerged(&unmerged)?;
  }
  Ok(())
}

fn restore(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let given = positional("restore", args);
  let paths: Vec<Vec<u8>> = given.iter().map(|a| repo.inside(a)).collect::<Res<_>>()?;
  if paths.is_empty() {
    return fatal(128, "fatal: you must specify path(s) to restore");
  }
  if has(args, "--staged") {
    // From HEAD, which a branch with no commits does not have.
    if repo.ws.head.is_none() {
      return fatal(128, "fatal: could not resolve HEAD");
    }
    let known: Vec<Vec<u8>> = repo.ws.load_index().entries().into_iter().map(|(p, _)| p).chain(repo.staged().into_keys()).collect();
    for (path, arg) in paths.iter().zip(&given) {
      if !path.is_empty() && !known.iter().any(|k| under(std::slice::from_ref(path), k)) {
        return fatal(1, format!("error: pathspec '{arg}' did not match any file(s) known to git"));
      }
    }
    unstage(&repo, &paths)?;
    return Ok(0);
  }
  put_back(&mut repo, &given, &paths)
}

/// Files back to what the index has for them: what was added, or else what HEAD has.
fn put_back(repo: &mut Repo, given: &[&str], paths: &[Vec<u8>]) -> Res<i32> {
  let staged = repo.staged();
  let known: Vec<Vec<u8>> = repo.ws.load_index().entries().into_iter().map(|(p, _)| p).chain(staged.keys().cloned()).collect();
  for (path, arg) in paths.iter().zip(given) {
    if !known.iter().any(|k| under(std::slice::from_ref(path), k)) {
      return fatal(1, format!("error: pathspec '{arg}' did not match any file(s) known to git"));
    }
  }
  let states = repo.states()?;
  let wanted: Vec<(Vec<u8>, Entry)> = states
    .iter()
    .filter(|(path, state)| under(paths, path) && !state.disk_is_index)
    .filter_map(|(path, state)| Some((path.clone(), state.index?)))
    .collect();
  // A file on its way back that is still unmerged is the merge's to settle, as Git has it.
  if let Some(path) = wanted.iter().map(|(path, _)| path).find(|path| repo.unmerged().contains(*path)) {
    return fatal(1, format!("error: path '{}' is unmerged", String::from_utf8_lossy(&repo.shown(path))));
  }
  let changes: Vec<Change> = wanted.iter().map(|(path, entry)| (path.clone(), Some(*entry))).collect();
  // What stands at a path being put back is the file the index has there, tracked: only what
  // stands where one of its directories has to be is in the way.
  let mut blocking = workspace::blocked(&mut repo.store, &repo.ws, &changes)?;
  blocking.retain(|path| !wanted.iter().any(|(want, _)| want.as_slice() == path.as_bytes()));
  if !blocking.is_empty() {
    return fatal(1, format!("error: these are not tracked, and stand where a tracked file has to be written: {}", blocking.join(", ")));
  }
  workspace::write_entries(&mut repo.store, &repo.ws, &wanted)?;
  Ok(0)
}

fn reset(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let mode = args.iter().rev().find(|a| ["--hard", "--soft", "--mixed"].contains(a)).copied().unwrap_or("--mixed");
  // A first argument that names a commit is the commit; the rest, or all after `--`, are paths.
  let mut given = positional("reset", args);
  let explicit = args.iter().position(|a| *a == "--");
  let before_dashes = explicit.map_or(given.len(), |at| positional("reset", &args[..at]).len());
  if let Some(Err(refusal)) = given.first().filter(|_| before_dashes > 0).map(|spec| repo.resolve(spec))
    && refusal.code == UNSUPPORTED
  {
    return Err(refusal);
  }
  let target = match given.first().filter(|_| before_dashes > 0) {
    Some(spec) if repo.resolve(spec).is_ok() => Some(repo.resolve(given.remove(0))?),
    Some(spec)
      if explicit.is_some() || mode != "--mixed" || !repo.ws.root.join(std::ffi::OsStr::from_bytes(&repo.rooted(spec))).exists() =>
    {
      return fatal(128, format!("fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree."));
    }
    _ => None,
  };
  let paths: Vec<Vec<u8>> = given.iter().map(|a| repo.rooted(a)).collect();
  if !paths.is_empty() {
    if mode != "--mixed" || target.is_some_and(|t| Some(t) != repo.ws.head) {
      return unsupported("reset` of paths to another commit, or with --hard or --soft");
    }
    unstage(&repo, &paths)?;
    if !quiet(args) {
      report_unstaged(&repo)?;
    }
    return Ok(0);
  }
  let before = repo.ws.head;
  let target = target.or(before);
  let root = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  match mode {
    "--hard" => {
      // Nothing is discarded before it is known that the whole of it can be done.
      let going: Vec<Vec<u8>> =
        repo.states()?.into_iter().filter(|(_, state)| state.head.is_none() && state.staged().is_some()).map(|(path, _)| path).collect();
      let blocking = workspace::hard_reset_blocked(&mut repo.store, &repo.ws, target, &going)?;
      if let Some(first) = blocking.first() {
        return fatal(
          128,
          format!(
            "error: Untracked working tree file '{first}' would be overwritten by merge.\nfatal: Could not reset index file to revision '{}'.",
            args.iter().rev().find(|a| !a.starts_with('-')).copied().unwrap_or("HEAD")
          ),
        );
      }
      // The index and the files both become the commit. What was added and is in no commit goes;
      // what was never added stays where it is.
      for (path, state) in repo.states()? {
        if state.head.is_none() && state.staged().is_some() {
          // With the directories it leaves empty, as Git removes them.
          workspace::unplace(&repo.ws.root, &path);
        }
      }
      repo.set_staged(&Staged::new())?;
      repo.end_merge();
      workspace::restore(&mut repo.store, &repo.ws, &[Vec::new()])?;
      if let Some(target) = target.filter(|t| Some(*t) != before) {
        workspace::switch(&mut repo.store, &mut repo.ws, target)?;
      }
    }
    "--soft" if repo.merging().is_some() => return fatal(128, "fatal: Cannot do a soft reset in the middle of a merge."),
    "--soft" => {
      // Only HEAD moves. The index keeps saying what it said, which is now a difference from HEAD.
      let mut staged = repo.staged();
      let (from, to) = (root(&mut repo.store, target)?, root(&mut repo.store, before)?);
      for (path, entry) in diff(&mut repo.store, from, to)? {
        staged.entry(path).or_insert(entry);
      }
      workspace::move_head(&mut repo.store, &mut repo.ws, target)?;
      // An entry the new HEAD agrees with is no longer a difference.
      let now = repo.ws.load_index();
      staged.retain(|path, entry| now.get(path).map(|known| known.entry) != *entry);
      repo.set_staged(&staged)?;
    }
    _ => {
      workspace::move_head(&mut repo.store, &mut repo.ws, target)?;
      repo.set_staged(&Staged::new())?;
      // The index is the commit's again: nothing is unmerged, and no merge is under way.
      repo.end_merge();
    }
  }
  if let (Some(name), Some(target)) = (repo.branch(), target) {
    repo.store.set_ref(&format!("heads/{name}"), target)?;
  }
  if !quiet(args) {
    match (mode, target) {
      ("--hard", Some(target)) => {
        let message = told(&Commit::load(&mut repo.store, &target)?.rest).message;
        let subject = message.lines().take_while(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" ");
        println!("HEAD is now at {} {subject}", &repo.git_oid(&target)?[..abbrev()]);
      }
      ("--mixed", _) => report_unstaged(&repo)?,
      _ => {}
    }
  }
  Ok(0)
}

/// After the index is reset, what the working tree still has that it does not.
fn report_unstaged(repo: &Repo) -> Res<()> {
  let left: Vec<(Vec<u8>, char)> = repo.states()?.into_iter().filter_map(|(path, state)| Some((path, state.unstaged()?))).collect();
  if !left.is_empty() {
    println!("Unstaged changes after reset:");
    for (path, what) in left {
      out(&[format!("{what}\t").as_bytes(), &quoted(&path, false), b"\n"].concat());
    }
  }
  Ok(())
}

fn commit(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let combined = args.iter().find(|a| a.len() > 2 && !a.starts_with("--") && a.ends_with('m')).copied().unwrap_or("");
  let all = has(args, "-a") || has(args, "--all") || combined.contains('a');
  let silent = quiet(args) || combined.contains('q');
  let (amend, allow_empty) = (has(args, "--amend"), has(args, "--allow-empty"));
  let messages: Vec<&str> = args
    .iter()
    .enumerate()
    .filter(|(_, a)| **a == "-m" || (a.len() > 2 && !a.starts_with("--") && a.ends_with('m')))
    .filter_map(|(i, _)| args.get(i + 1).copied())
    .collect();
  // Workspaces that share a store share its branches. A branch another of them has moved is not
  // one this workspace's commit can go on: said before the commit is made, not after.
  if let Some(name) = repo.branch()
    && let Some(tip) = repo.branch_tip(&name)
    && repo.ws.head.is_some_and(|head| head != tip)
  {
    return fatal(
      128,
      format!(
        "fatal: branch '{name}' is at another commit than this workspace: another workspace on the same store moved it. Nothing was committed.\nGo on under a name of this workspace's own: nv git switch -c <name>"
      ),
    );
  }
  if messages.len() > 1 {
    return unsupported("commit` with more than one -m");
  }
  if !positional("commit", args).is_empty() {
    return unsupported("commit` of named paths");
  }
  let merging = repo.merging();
  // With -a every tracked file goes in as it is on disk, which settles the ones a merge left unsettled.
  if all && merging.is_some() {
    let states = repo.states_of_tracked()?;
    stage(&mut repo, &states, &|_, state| state.index.is_some() || state.unmerged)?;
  }
  let unmerged = repo.unmerged();
  if !unmerged.is_empty() {
    for path in &unmerged {
      out(&[b"U\t".as_slice(), &quoted(path, false), b"\n"].concat());
    }
    return fatal(
      128,
      "error: Committing is not possible because you have unmerged files.\nhint: Fix them up in the work tree, and then use 'git add/rm <file>'\nhint: as appropriate to mark resolution and make a commit.\nfatal: Exiting because of an unresolved conflict.",
    );
  }
  if merging.is_some() && amend {
    return fatal(128, "fatal: You are in the middle of a merge -- cannot amend.");
  }
  // A sync that could not merge leaves the workspace on the ref's commit, someone else's, with
  // this workspace's work on top as changes to commit: the same as a merge that stopped. Amended,
  // that commit would carry this workspace's resolution under another writer's name and parents.
  if amend
    && let Some(head) = repo.ws.head
    && repo.ws.pushed == Some(head)
    && !repo.ws.operations().iter().any(|(_, what, id)| what == "commit" && *id == hex(&head))
  {
    return fatal(
      128,
      "fatal: HEAD is the ref's commit, made by another workspace, as the last sync left it -- cannot amend.\nCommit without --amend: the commit goes on top of theirs.",
    );
  }
  let before = repo.ws.head;
  let previous = match (amend, before) {
    (true, Some(head)) => Some(Commit::load(&mut repo.store, &head)?),
    (true, None) => return fatal(128, "fatal: You have nothing to amend."),
    _ => None,
  };
  let message = match (messages.first(), &previous) {
    (Some(message), _) => cleaned(message),
    (None, Some(previous)) if has(args, "--no-edit") => told(&previous.rest).message.trim_end().to_string(),
    (None, None) if has(args, "--no-edit") && merging.is_some() => {
      merging.as_ref().map(|(_, message)| message.trim_end().to_string()).unwrap_or_default()
    }
    _ => return unsupported("commit` without -m (there is no editor)"),
  };
  if message.is_empty() {
    return fatal(1, "Aborting commit due to empty commit message.");
  }
  // With -a, every file the index has goes in as it is on disk first. Git does that in an index
  // of its own and keeps it only for a commit: should none come of it, the staging is as it was.
  let staged_before = repo.staged();
  if all {
    let states = repo.states_of_tracked()?;
    stage(&mut repo, &states, &|_, state| state.index.is_some())?;
  }
  // What the index has that the last commit does not say: read off the index, with no look at the files.
  let known = repo.ws.load_index();
  let changes: Vec<Change> = repo.staged().into_iter().filter(|(path, entry)| known.get(path).map(|k| k.entry) != *entry).collect();
  if changes.is_empty() && !allow_empty && !amend && merging.is_none() {
    repo.set_staged(&staged_before)?;
    // There is nothing to commit, and why is what status says.
    let (lines, untracked) = status_lines(&mut repo)?;
    // What `status` calls "No commits yet", a commit that finds nothing calls the commit it is not.
    out(long_status(&mut repo, &lines, &untracked)?.replacen("\nNo commits yet\n", "\nInitial commit\n", 1).as_bytes());
    return Ok(1);
  }
  // An amended commit keeps who wrote it and when; who commits it is whoever is here now.
  let author = match &previous {
    Some(previous) => told(&previous.rest).author,
    None => identity(&repo, "AUTHOR")?,
  };
  let rest = format!("author {author}\ncommitter {}\n\n{message}\n", identity(&repo, "COMMITTER")?).into_bytes();
  // An amend is one commit in place of the one before, made and named in one step.
  let id =
    workspace::commit_entries_as(&mut repo.store, &mut repo.ws, rest, changes, merging.map(|(theirs, _)| theirs), previous.as_ref())?;
  repo.end_merge();
  repo.set_staged(&Staged::new())?;
  repo.advance(before, id)?;
  let oid = repo.git_oid(&id)?;
  if !silent {
    let made = Commit::load(&mut repo.store, &id)?;
    let branch = repo.branch().unwrap_or_else(|| "detached HEAD".into());
    println!(
      "[{branch}{} {}] {}",
      if made.parents.is_empty() && !amend { " (root-commit)" } else { "" },
      &oid[..abbrev()],
      message.lines().take_while(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" ")
    );
    // Who wrote it, when that is not who commits it; and when, when the commit is one being redone.
    let said = told(&made.rest);
    let (author, committer) = (who(&said.author), who(&said.committer));
    if (author.0, author.1) != (committer.0, committer.1) {
      println!(" Author: {} <{}>", author.0, author.1);
    }
    if amend {
      println!(" Date: {}", date(author.2, author.3));
    }
    let root = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
    let (from, to) = (root(&mut repo.store, made.parents.first().copied())?, root(&mut repo.store, Some(id))?);
    let changed = between(&mut repo, from, to)?;
    // A merge's commit is announced and no more.
    if !changed.is_empty() && made.parents.len() < 2 {
      out(summary(&changed).as_bytes());
      out(file_notes(&changed).as_bytes());
    }
  }
  Ok(0)
}

/// What `status` has to say about one path.
struct Line {
  path: Vec<u8>,
  /// For a rename, where it came from.
  from: Option<Vec<u8>>,
  /// The index against the last commit, and the working tree against the index: `A`, `M`, `D`, `R` or a space.
  staged: char,
  unstaged: char,
}

/// The differences as Git's status would list them: tracked paths in
/// order, then what is untracked. In the index, a file gone and a file new
/// that are mostly one content are one rename.
fn status_lines(repo: &mut Repo) -> Res<(Vec<Line>, Vec<Vec<u8>>)> {
  let states = repo.states()?;
  // What Git has in its index: the files of HEAD and what was added.
  let mut tracked: Vec<Vec<u8>> = repo.ws.load_index().entries().into_iter().map(|(p, _)| p).collect();
  tracked.extend(states.iter().filter(|(_, s)| s.index.is_some()).map(|(p, _)| p.clone()));
  let holds_tracked = |dir: &[u8]| tracked.iter().any(|p| p.len() > dir.len() && p.starts_with(dir) && p[dir.len()] == b'/');
  let mut lines: Vec<Line> = Vec::new();
  let mut untracked: std::collections::BTreeSet<Vec<u8>> = Default::default();
  for (path, state) in &states {
    if state.untracked() {
      // An untracked file is shown as the outermost directory that holds nothing tracked.
      let mut shown = path.clone();
      for (i, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
        if !holds_tracked(&path[..i]) {
          shown = path[..=i].to_vec();
          break;
        }
      }
      untracked.insert(shown);
    }
    let (staged, unstaged) = (state.staged().unwrap_or(' '), state.unstaged().unwrap_or(' '));
    if (staged, unstaged) != (' ', ' ') {
      lines.push(Line { path: path.clone(), from: None, staged, unstaged });
    }
  }
  let gone: Vec<(Vec<u8>, Id)> =
    states.iter().filter(|(_, s)| s.staged() == Some('D')).filter_map(|(p, s)| Some((p.clone(), s.head?.id))).collect();
  let came: Vec<(Vec<u8>, Id)> =
    states.iter().filter(|(_, s)| s.staged() == Some('A')).filter_map(|(p, s)| Some((p.clone(), s.index?.id))).collect();
  let (mut before, mut after) = (Vec::new(), Vec::new());
  for (_, id) in &gone {
    before.push(repo.blob(id)?);
  }
  for (_, id) in &came {
    after.push(repo.blob(id)?);
  }
  let pairs =
    rename_pairs(&before.iter().map(|b| b.as_slice()).collect::<Vec<_>>(), &after.iter().map(|b| b.as_slice()).collect::<Vec<_>>());
  for (i, j, _) in pairs {
    let (from, to) = (&gone[i].0, &came[j].0);
    lines.retain(|l| l.path != *from);
    if let Some(line) = lines.iter_mut().find(|l| l.path == *to) {
      (line.staged, line.from) = ('R', Some(from.clone()));
    }
  }
  lines.sort_by(|a, b| a.path.cmp(&b.path));
  Ok((lines, untracked.into_iter().collect()))
}

fn status(mut repo: Repo, args: &[&str]) -> Res<i32> {
  if !positional("status", args).is_empty() {
    return unsupported("status` of named paths");
  }
  let (mut lines, mut untracked) = status_lines(&mut repo)?;
  // To someone in a subdirectory paths are told from there, except in the format meant for programs.
  if !has(args, "--porcelain") {
    for line in &mut lines {
      line.path = repo.shown(&line.path);
      line.from = line.from.take().map(|from| repo.shown(&from));
    }
    for path in &mut untracked {
      *path = repo.shown(path);
    }
  }
  if !args.is_empty() {
    for line in &lines {
      let from = line.from.as_ref().map(|f| [quoted(f, true).as_slice(), b" -> "].concat()).unwrap_or_default();
      out(&[[line.staged as u8, line.unstaged as u8, b' '].as_slice(), &from, &quoted(&line.path, true), b"\n"].concat());
    }
    for path in &untracked {
      out(&[b"?? ".as_slice(), &quoted(path, true), b"\n"].concat());
    }
    return Ok(0);
  }
  out(long_status(&mut repo, &lines, &untracked)?.as_bytes());
  Ok(0)
}

/// `git status` in full, in Git's words.
fn long_status(repo: &mut Repo, lines: &[Line], untracked: &[Vec<u8>]) -> Res<String> {
  let mut text = match repo.branch() {
    Some(branch) => format!("On branch {branch}\n"),
    None => "Not currently on any branch.\n".to_string(),
  };
  let unborn = repo.ws.head.is_none();
  if unborn {
    text.push_str("\nNo commits yet\n\n");
  }
  // Where the branch stands against the one it was cloned from.
  if let (Some(branch), Some(ours)) = (repo.branch(), repo.ws.head)
    && repo.follows(&branch)
    && let Some(theirs) = repo.store.read_ref(&format!("remotes/origin/{branch}"))
  {
    let (ahead, behind) = apart(&mut repo.store, ours, theirs)?;
    let commits = |n: usize| if n == 1 { "1 commit".to_string() } else { format!("{n} commits") };
    text.push_str(&match (ahead, behind) {
      (0, 0) => format!("Your branch is up to date with 'origin/{branch}'.\n"),
      (ahead, 0) => format!("Your branch is ahead of 'origin/{branch}' by {}.\n  (use \"git push\" to publish your local commits)\n", commits(ahead)),
      (0, behind) => format!("Your branch is behind 'origin/{branch}' by {}, and can be fast-forwarded.\n  (use \"git pull\" to update your local branch)\n", commits(behind)),
      (ahead, behind) => format!(
        "Your branch and 'origin/{branch}' have diverged,\nand have {ahead} and {behind} different commits each, respectively.\n  (use \"git pull\" if you want to integrate the remote branch with yours)\n"
      ),
    });
    text.push('\n');
  }
  let label = |what: char| match what {
    'A' => "new file:   ",
    'M' => "modified:   ",
    'D' => "deleted:    ",
    _ => "renamed:    ",
  };
  let listed = |what: char, l: &Line| {
    let from =
      l.from.as_ref().filter(|_| what == 'R').map(|f| format!("{} -> ", String::from_utf8_lossy(&quoted(f, false)))).unwrap_or_default();
    format!("\t{}{from}{}\n", label(what), String::from_utf8_lossy(&quoted(&l.path, false)))
  };
  let staged: Vec<&Line> = lines.iter().filter(|l| l.staged != ' ' && l.staged != 'U').collect();
  let unstaged: Vec<&Line> = lines.iter().filter(|l| l.unstaged != ' ' && l.unstaged != 'U').collect();
  let unmerged: Vec<&Line> = lines.iter().filter(|l| l.staged == 'U').collect();
  let merging = repo.merging().is_some();
  if !unmerged.is_empty() {
    text
      .push_str("You have unmerged paths.\n  (fix conflicts and run \"git commit\")\n  (use \"git merge --abort\" to abort the merge)\n\n");
  } else if merging {
    text.push_str("All conflicts fixed but you are still merging.\n  (use \"git commit\" to conclude merge)\n\n");
  }
  if !staged.is_empty() {
    text.push_str("Changes to be committed:\n");
    // In a merge there is nothing to tell about unstaging.
    text.push_str(match (merging, unborn) {
      (true, _) => "",
      (_, true) => "  (use \"git rm --cached <file>...\" to unstage)\n",
      _ => "  (use \"git restore --staged <file>...\" to unstage)\n",
    });
    text.extend(staged.iter().map(|l| listed(l.staged, l)));
    text.push('\n');
  }
  if !unmerged.is_empty() {
    text.push_str("Unmerged paths:\n  (use \"git add <file>...\" to mark resolution)\n");
    text.extend(unmerged.iter().map(|l| format!("\tboth modified:   {}\n", String::from_utf8_lossy(&quoted(&l.path, false)))));
    text.push('\n');
  }
  if !unstaged.is_empty() {
    let rm = if unstaged.iter().any(|l| l.unstaged == 'D') { "/rm" } else { "" };
    text.push_str(&format!("Changes not staged for commit:\n  (use \"git add{rm} <file>...\" to update what will be committed)\n  (use \"git restore <file>...\" to discard changes in working directory)\n"));
    text.extend(unstaged.iter().map(|l| listed(l.unstaged, l)));
    text.push('\n');
  }
  if !untracked.is_empty() {
    text.push_str("Untracked files:\n  (use \"git add <file>...\" to include in what will be committed)\n");
    text.extend(untracked.iter().map(|p| format!("\t{}\n", String::from_utf8_lossy(&quoted(p, false)))));
    text.push('\n');
  }
  if staged.is_empty() {
    text.push_str(match (unstaged.is_empty(), untracked.is_empty(), unborn) {
      (false, _, _) => "no changes added to commit (use \"git add\" and/or \"git commit -a\")\n",
      (true, false, _) => "nothing added to commit but untracked files present (use \"git add\" to track)\n",
      (true, true, true) => "nothing to commit (create/copy files and use \"git add\" to track)\n",
      (true, true, false) => "nothing to commit, working tree clean\n",
    });
  }
  Ok(text)
}

fn rev_parse(mut repo: Repo, args: &[&str]) -> Res<i32> {
  match args {
    ["--show-toplevel"] => println!("{}", std::fs::canonicalize(&repo.ws.root).map_err(|e| e.to_string())?.display()),
    ["--abbrev-ref", "HEAD"] => match (repo.ws.head, repo.branch()) {
      (None, _) => return fatal(128, "fatal: ambiguous argument 'HEAD': unknown revision or path not in the working tree."),
      (_, Some(name)) => println!("{name}"),
      (_, None) => println!("HEAD"),
    },
    ["--short", spec] => {
      let id = repo.resolve_any(spec)?;
      println!("{}", &repo.git_oid(&id)?[..abbrev()]);
    }
    [spec] if !spec.starts_with('-') => match spec.strip_suffix("^{tree}") {
      Some(commit) => {
        let id = repo.resolve(commit)?;
        let root = root_of(&mut repo.store, &id)?;
        println!("{}", repo.tree_oid(root)?);
      }
      None => {
        let id = repo.resolve_any(spec).inspect_err(|refusal| {
          if refusal.code == 128 {
            println!("{spec}");
          }
        })?;
        println!("{}", repo.object_oid(&id)?);
      }
    },
    other => return unsupported(format_args!("rev-parse {}", other.join(" "))),
  }
  Ok(0)
}

/// One commit as `log` and `show` print it: in the given format, or Git's default.
fn described(repo: &mut Repo, id: &Id, format: Option<&str>) -> Res<String> {
  let commit = Commit::load(&mut repo.store, id)?;
  let said = told(&commit.rest);
  let oid = repo.git_oid(id)?;
  let mut parents = Vec::new();
  for p in &commit.parents {
    parents.push(repo.git_oid(p)?);
  }
  let short = |oids: &[String]| oids.iter().map(|p| &p[..abbrev()]).collect::<Vec<_>>().join(" ");
  let (author, committer) = (who(&said.author), who(&said.committer));
  let Some(format) = format else {
    let mut text = format!("commit {oid}\n");
    if parents.len() > 1 {
      text.push_str(&format!("Merge: {}\n", short(&parents)));
    }
    text.push_str(&format!("Author: {} <{}>\nDate:   {}\n\n", author.0, author.1, date(author.2, author.3)));
    for line in said.message.trim_end().lines() {
      text.push_str(&format!("    {line}\n"));
    }
    return Ok(text);
  };
  let root = root_of(&mut repo.store, id)?;
  let mut line = String::new();
  let mut chars = format.chars();
  while let Some(c) = chars.next() {
    if c != '%' {
      line.push(c);
      continue;
    }
    match chars.next() {
      Some('H') => line.push_str(&oid),
      Some('h') => line.push_str(&oid[..abbrev()]),
      Some('T') => line.push_str(&repo.tree_oid(root)?),
      Some('t') => line.push_str(&repo.tree_oid(root)?[..abbrev()]),
      Some('P') => line.push_str(&parents.join(" ")),
      Some('p') => line.push_str(&short(&parents)),
      // The subject is the first paragraph, on one line; the body is what follows it.
      Some('s') => line.push_str(&said.message.lines().take_while(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" ")),
      Some('B') => line.push_str(&(said.message.trim_end().to_string() + "\n")),
      Some('b') => {
        let body: Vec<&str> = said.message.lines().skip_while(|l| !l.trim().is_empty()).skip_while(|l| l.trim().is_empty()).collect();
        if !body.is_empty() {
          line.push_str(&(body.join("\n").trim_end().to_string() + "\n"));
        }
      }
      Some('n') => line.push('\n'),
      Some('%') => line.push('%'),
      Some(role @ ('a' | 'c')) => {
        let of = if role == 'a' { author } else { committer };
        match chars.next() {
          Some('n') => line.push_str(of.0),
          Some('e') => line.push_str(of.1),
          Some('t') => line.push_str(&of.2.to_string()),
          other => return unsupported(format_args!("log --format` with `%{role}{}", other.unwrap_or(' '))),
        }
      }
      other => return unsupported(format_args!("log --format` with `%{}", other.unwrap_or(' '))),
    }
  }
  Ok(line)
}

fn format_of<'a>(args: &[&'a str]) -> Option<&'a str> {
  // The last one given is the one that counts, `--oneline` among them.
  args.iter().rev().find_map(|a| {
    if *a == "--oneline" {
      return Some("%h %s");
    }
    let given = a.strip_prefix("--format=").or_else(|| a.strip_prefix("--pretty="))?;
    Some(given.strip_prefix("format:").or_else(|| given.strip_prefix("tformat:")).unwrap_or(given))
  })
}

/// `format:` puts a newline between commits; every other way of giving a format ends each with one.
fn format_separates(args: &[&str]) -> bool {
  args
    .iter()
    .rev()
    .find(|a| **a == "--oneline" || a.starts_with("--format=") || a.starts_with("--pretty="))
    .is_some_and(|a| a.contains("=format:"))
}

/// The formats Git has names for are not spoken: taken as a format they would be printed as they stand.
fn format_spoken(args: &[&str]) -> Res<()> {
  const NAMED: &[&str] = &["oneline", "short", "medium", "full", "fuller", "reference", "email", "mboxrd", "raw"];
  match args.iter().find_map(|a| a.strip_prefix("--format=")).filter(|format| NAMED.contains(format)) {
    Some(name) => unsupported(format_args!("log --format={name}")),
    None => Ok(()),
  }
}

fn log(mut repo: Repo, args: &[&str]) -> Res<i32> {
  format_spoken(args)?;
  let format = format_of(args);
  let separates = format_separates(args);
  let count = |text: &str| text.parse::<usize>().map_err(|_| Fail { code: 128, message: format!("fatal: '{text}': not an integer") });
  let mut limit = usize::MAX;
  for (i, arg) in args.iter().enumerate() {
    if *arg == "-n" {
      limit = count(args.get(i + 1).copied().unwrap_or(""))?;
    } else if arg.len() > 1
      && arg.starts_with('-')
      && arg.trim_start_matches("-n").trim_start_matches('-').bytes().all(|b| b.is_ascii_digit())
      && arg.bytes().any(|b| b.is_ascii_digit())
    {
      limit = count(arg.trim_start_matches("-n").trim_start_matches('-'))?;
    }
  }
  let from = match positional("log", args)[..] {
    [] => None,
    [from] => Some(from),
    _ => return unsupported("log` of more than one revision, or of paths"),
  };
  if repo.ws.head.is_none() && from.is_none() {
    let branch = repo.branch().unwrap_or_default();
    return fatal(128, format!("fatal: your current branch '{branch}' does not have any commits yet"));
  }
  if let Some(named) = from
    && repo.ws.root.join(std::ffi::OsStr::from_bytes(&repo.rooted(named))).exists()
  {
    if repo.resolve_any(named).is_err() {
      return unsupported("log` of paths");
    }
    return fatal(
      128,
      format!(
        "fatal: ambiguous argument '{named}': both revision and filename\nUse '--' to separate paths from revisions, like this:\n'git <command> [<revision>...] -- [<file>...]'"
      ),
    );
  }
  let start = repo.resolve(from.unwrap_or("HEAD"))?;
  // Newest first by committer date; of two made at the same second, the one reached first.
  let mut queue: std::collections::BinaryHeap<(i64, std::cmp::Reverse<usize>, Id)> = Default::default();
  let mut seen = std::collections::HashSet::new();
  let mut reached = 0;
  let mut push = |store: &mut Store, queue: &mut std::collections::BinaryHeap<_>, id: Id| -> Res<()> {
    if seen.insert(id) {
      let commit = Commit::load(store, &id)?;
      queue.push((who(&told(&commit.rest).committer).2, std::cmp::Reverse(reached), id));
      reached += 1;
    }
    Ok(())
  };
  push(&mut repo.store, &mut queue, start)?;
  let mut shown = 0;
  while let Some((_, _, id)) = queue.pop() {
    if shown == limit {
      break;
    }
    if shown > 0 && (format.is_none() || (separates && format != Some(""))) {
      println!();
    }
    let text = described(&mut repo, &id, format)?;
    out(text.as_bytes());
    // An empty format prints nothing at all for a commit, not an empty line.
    if format.is_some() && !separates && !(format == Some("") && text.is_empty()) {
      println!();
    }
    shown += 1;
    for p in Commit::load(&mut repo.store, &id)?.parents {
      push(&mut repo.store, &mut queue, p)?;
    }
  }
  Ok(0)
}

/// One side of a changed file: its mode, its bytes, and Git's id for them.
struct Side {
  mode: u32,
  bytes: std::sync::Arc<Vec<u8>>,
  oid: String,
}

struct FileChange {
  path: Vec<u8>,
  /// For a rename, the path it had, and how much of the content it kept (see `textdiff::similarity`).
  from: Option<Vec<u8>>,
  score: u32,
  old: Option<Side>,
  new: Option<Side>,
}

impl FileChange {
  fn letter(&self) -> String {
    match (&self.old, &self.new, &self.from) {
      (_, _, Some(_)) => format!("R{:03}", self.score * 100 / textdiff::SAME),
      (None, _, _) => "A".to_string(),
      (_, None, _) => "D".to_string(),
      _ => "M".to_string(),
    }
  }
}

/// Which removed things and added things are one thing that moved: the
/// pairs Git would make, best match first, each side used once.
fn rename_pairs(removed: &[&[u8]], added: &[&[u8]]) -> Vec<(usize, usize, u32)> {
  let mut scored: Vec<(u32, usize, usize)> = Vec::new();
  for (i, from) in removed.iter().enumerate() {
    for (j, to) in added.iter().enumerate() {
      let score = textdiff::similarity(from, to);
      if score >= textdiff::RENAME {
        scored.push((score, i, j));
      }
    }
  }
  scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
  let (mut used_from, mut used_to, mut pairs) = (vec![false; removed.len()], vec![false; added.len()], Vec::new());
  for (score, i, j) in scored {
    if !used_from[i] && !used_to[j] {
      (used_from[i], used_to[j]) = (true, true);
      pairs.push((i, j, score));
    }
  }
  pairs
}

fn side_of(repo: &mut Repo, entry: Entry) -> Res<Side> {
  let bytes = repo.store.get(&entry.id)?.ok_or_else(|| "a blob is missing from the store".to_string())?;
  Ok(Side { mode: entry.mode, bytes, oid: repo.blob_oid(&entry.id)? })
}

/// A file removed and a file added that are mostly the same content are one file that moved.
fn pair_renames(mut changes: Vec<FileChange>) -> Vec<FileChange> {
  let regular = |side: &Option<Side>| side.as_ref().filter(|s| s.mode != MODE_LINK && s.mode != MODE_GITLINK).map(|s| s.bytes.clone());
  let removed: Vec<(usize, std::sync::Arc<Vec<u8>>)> =
    changes.iter().enumerate().filter(|(_, c)| c.new.is_none()).filter_map(|(i, c)| Some((i, regular(&c.old)?))).collect();
  let added: Vec<(usize, std::sync::Arc<Vec<u8>>)> =
    changes.iter().enumerate().filter(|(_, c)| c.old.is_none()).filter_map(|(i, c)| Some((i, regular(&c.new)?))).collect();
  let pairs = rename_pairs(
    &removed.iter().map(|(_, b)| b.as_slice()).collect::<Vec<_>>(),
    &added.iter().map(|(_, b)| b.as_slice()).collect::<Vec<_>>(),
  );
  let mut gone = Vec::new();
  for (i, j, score) in pairs {
    let (from, to) = (removed[i].0, added[j].0);
    let old = changes[from].old.take();
    (changes[to].from, changes[to].score, changes[to].old) = (Some(changes[from].path.clone()), score, old);
    gone.push(from);
  }
  gone.sort_unstable();
  for at in gone.into_iter().rev() {
    changes.remove(at);
  }
  changes
}

/// What changed between two commits' trees.
fn between(repo: &mut Repo, from: Option<Id>, to: Option<Id>) -> Res<Vec<FileChange>> {
  let mut changes = Vec::new();
  for (path, entry) in diff(&mut repo.store, from, to)? {
    let before = match from {
      Some(root) => lookup(&mut repo.store, root, &path)?.filter(|e| !e.is_dir()),
      None => None,
    };
    let old = before.map(|e| side_of(repo, e)).transpose()?;
    let new = entry.map(|e| side_of(repo, e)).transpose()?;
    changes.push(FileChange { path, from: None, score: 0, old, new });
  }
  changes.sort_by(|a, b| a.path.cmp(&b.path));
  Ok(pair_renames(changes))
}

/// The index against the last commit (`cached`), or the working tree against the index.
fn on_disk(repo: &mut Repo, cached: bool) -> Res<Vec<FileChange>> {
  let mut changes = Vec::new();
  // What is staged is in the index and needs no look at the files; what is not is among the files Git tracks.
  let states = if cached { repo.states_at(false, &[])? } else { repo.states_of_tracked()? };
  for (path, state) in states {
    let (old, new) = if cached {
      if state.staged().is_none() {
        continue;
      }
      (state.head.map(|e| side_of(repo, e)).transpose()?, state.index.map(|e| side_of(repo, e)).transpose()?)
    } else {
      if state.unstaged().is_none() {
        continue;
      }
      let disk = state.disk.clone().map(|(mode, bytes)| Side { mode, oid: hex(&git_hash("blob", &bytes)), bytes });
      (state.index.map(|e| side_of(repo, e)).transpose()?, disk)
    };
    changes.push(FileChange { path, from: None, score: 0, old, new });
  }
  Ok(if cached { pair_renames(changes) } else { changes })
}

/// The changes as a patch, in Git's layout.
fn say(text: &mut Vec<u8>, line: &[u8]) {
  text.extend_from_slice(line);
  text.push(b'\n');
}

fn patch(changes: &[FileChange]) -> Vec<u8> {
  let mut text = Vec::new();
  for change in changes {
    let name = |prefix: &str, path: &[u8]| quoted(&[prefix.as_bytes(), path].concat(), false);
    let (a, b) = (name("a/", change.from.as_ref().unwrap_or(&change.path)), name("b/", &change.path));
    say(&mut text, &[b"diff --git ".as_slice(), &a, b" ", &b].concat());
    let none = "0000000";
    match (&change.old, &change.new) {
      (Some(old), Some(new)) => {
        if let Some(from) = &change.from {
          say(&mut text, format!("similarity index {}%", change.score * 100 / textdiff::SAME).as_bytes());
          say(&mut text, &[b"rename from ".as_slice(), &quoted(from, false)].concat());
          say(&mut text, &[b"rename to ".as_slice(), &quoted(&change.path, false)].concat());
        }
        if old.oid == new.oid && old.mode == new.mode {
          continue;
        }
        if old.mode != new.mode {
          say(&mut text, format!("old mode {:06o}", old.mode).as_bytes());
          say(&mut text, format!("new mode {:06o}", new.mode).as_bytes());
          if old.oid == new.oid {
            continue;
          }
          say(&mut text, format!("index {}..{}", &old.oid[..abbrev()], &new.oid[..abbrev()]).as_bytes());
        } else {
          say(&mut text, format!("index {}..{} {:06o}", &old.oid[..abbrev()], &new.oid[..abbrev()], new.mode).as_bytes());
        }
      }
      (None, Some(new)) => {
        say(&mut text, format!("new file mode {:06o}", new.mode).as_bytes());
        say(&mut text, format!("index {none}..{}", &new.oid[..abbrev()]).as_bytes());
      }
      (Some(old), None) => {
        say(&mut text, format!("deleted file mode {:06o}", old.mode).as_bytes());
        say(&mut text, format!("index {}..{none}", &old.oid[..abbrev()]).as_bytes());
      }
      (None, None) => continue,
    }
    let empty = Vec::new();
    let (before, after) = (change.old.as_ref().map_or(&empty, |s| &*s.bytes), change.new.as_ref().map_or(&empty, |s| &*s.bytes));
    if before == after {
      continue;
    }
    let (left, right) = (
      if change.old.is_some() { a.clone() } else { b"/dev/null".to_vec() },
      if change.new.is_some() { b.clone() } else { b"/dev/null".to_vec() },
    );
    if textdiff::binary(before) || textdiff::binary(after) {
      say(&mut text, &[b"Binary files ".as_slice(), &left, b" and ", &right, b" differ"].concat());
      continue;
    }
    // A name with a space in it is followed by a tab, so that the name's end can be told.
    let ended = |name: &[u8]| if name.contains(&b' ') && name != b"/dev/null" { [name, b"\t"].concat() } else { name.to_vec() };
    say(&mut text, &[b"--- ".as_slice(), &ended(&left)].concat());
    say(&mut text, &[b"+++ ".as_slice(), &ended(&right)].concat());
    text.extend_from_slice(&textdiff::hunks(before, after));
  }
  text
}

/// A rename as Git writes it where space matters: what the two paths share, once, around what differs.
fn rename_name(from: &[u8], to: &[u8]) -> String {
  // The shared start up to a slash, and the shared end from a slash.
  let mut prefix = 0;
  for (i, (a, b)) in from.iter().zip(to).enumerate() {
    if a != b {
      break;
    }
    if *a == b'/' {
      prefix = i + 1;
    }
  }
  let mut suffix = 0;
  for (i, (a, b)) in from.iter().rev().zip(to.iter().rev()).enumerate() {
    if a != b || i >= from.len() - prefix || i >= to.len() - prefix {
      break;
    }
    if *a == b'/' {
      suffix = i + 1;
    }
  }
  let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
  let (a, b) = (&from[prefix..from.len() - suffix], &to[prefix..to.len() - suffix]);
  if prefix + suffix == 0 {
    format!("{} => {}", text(a), text(b))
  } else {
    format!("{}{{{} => {}}}{}", text(&from[..prefix]), text(a), text(b), text(&from[from.len() - suffix..]))
  }
}

impl FileChange {
  fn shown(&self) -> String {
    match &self.from {
      Some(from) => rename_name(from, &self.path),
      None => String::from_utf8_lossy(&quoted(&self.path, false)).into_owned(),
    }
  }

  /// `(lines added, lines removed)`, or `None` for content Git counts in bytes.
  fn lines(&self) -> Option<(usize, usize)> {
    let empty = Vec::new();
    let (before, after) = (self.old.as_ref().map_or(&empty, |s| &*s.bytes), self.new.as_ref().map_or(&empty, |s| &*s.bytes));
    if textdiff::binary(before) || textdiff::binary(after) {
      return None;
    }
    let (removed, added) = textdiff::counts(before, after);
    Some((added, removed))
  }
}

/// ` 3 files changed, 5 insertions(+), 2 deletions(-)`.
fn summary(changes: &[FileChange]) -> String {
  let (added, removed) = changes.iter().filter_map(FileChange::lines).fold((0, 0), |(a, r), (x, y)| (a + x, r + y));
  let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
  let mut text = format!(" {} changed", plural(changes.len(), "file"));
  // Both are named when there is nothing of either; otherwise only what there is.
  if added > 0 || removed == 0 {
    text.push_str(&format!(", {}(+)", plural(added, "insertion")));
  }
  if removed > 0 || added == 0 {
    text.push_str(&format!(", {}(-)", plural(removed, "deletion")));
  }
  text.push('\n');
  text
}

/// What happened to files as files: created, deleted, renamed, mode changed.
fn file_notes(changes: &[FileChange]) -> String {
  let mut text = String::new();
  for change in changes {
    let name = String::from_utf8_lossy(&quoted(&change.path, false)).into_owned();
    match (&change.old, &change.new, &change.from) {
      (None, Some(new), _) => text.push_str(&format!(" create mode {:06o} {name}\n", new.mode)),
      (Some(old), None, _) => text.push_str(&format!(" delete mode {:06o} {name}\n", old.mode)),
      (Some(old), Some(new), Some(_)) => {
        text.push_str(&format!(" rename {} ({}%)\n", change.shown(), change.score * 100 / textdiff::SAME));
        if old.mode != new.mode {
          text.push_str(&format!(" mode change {:06o} => {:06o}\n", old.mode, new.mode));
        }
      }
      (Some(old), Some(new), None) if old.mode != new.mode => {
        text.push_str(&format!(" mode change {:06o} => {:06o} {name}\n", old.mode, new.mode))
      }
      _ => {}
    }
  }
  text
}

/// One line per file with a bar for its added and removed lines, as `--stat` draws it, then the summary.
fn diffstat(changes: &[FileChange]) -> String {
  if changes.is_empty() {
    return String::new();
  }
  const WIDTH: usize = 80;
  let rows: Vec<(String, Option<(usize, usize)>)> = changes.iter().map(|c| (c.shown(), c.lines())).collect();
  let most = rows.iter().filter_map(|(_, l)| l.map(|(a, r)| a + r)).max().unwrap_or(0);
  let digits = most.to_string().len().max(if rows.iter().any(|(_, l)| l.is_none()) { 3 } else { 1 });
  let mut name_width = rows.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0);
  // The bar is as long as the largest change, unless that does not fit the line.
  let mut bar_width = most;
  if name_width + digits + 6 + bar_width > WIDTH {
    bar_width = bar_width.min((WIDTH * 3 / 8).saturating_sub(digits + 6).max(6));
    if name_width > WIDTH - digits - 6 - bar_width {
      name_width = WIDTH - digits - 6 - bar_width;
    } else {
      bar_width = WIDTH - digits - 6 - name_width;
    }
  }
  // Git's rule, to the letter: a non-zero count never scales to nothing, the total is scaled, and
  // the smaller of the two shares is scaled while the larger takes what is left.
  let scaling = bar_width <= most && most > 0;
  let scaled = |n: usize| if n == 0 { 0 } else { 1 + n * (bar_width - 1) / most };
  let mut text = String::new();
  for (index, (name, lines)) in rows.iter().enumerate() {
    // A name too long for its column loses its start.
    let name = if name.chars().count() > name_width {
      let keep: String = name.chars().skip(name.chars().count() - name_width + 3).collect();
      let keep = keep.find('/').map_or(keep.clone(), |slash| keep[slash..].to_string());
      format!("...{keep}")
    } else {
      name.clone()
    };
    match lines {
      Some((added, removed)) => {
        let (mut plus, mut minus) = (*added, *removed);
        if scaling {
          let mut total = scaled(plus + minus);
          if total < 2 && plus > 0 && minus > 0 {
            total = 2;
          }
          if plus < minus {
            plus = scaled(plus);
            minus = total - plus;
          } else {
            minus = scaled(minus);
            plus = total - minus;
          }
        }
        let bar = format!("{}{}", "+".repeat(plus), "-".repeat(minus));
        text.push_str(format!(" {name:<name_width$} | {:>digits$} {bar}", added + removed).trim_end());
        text.push('\n');
      }
      None => {
        let change = &changes[index];
        let size = |side: &Option<Side>| side.as_ref().map_or(0, |s| s.bytes.len());
        text.push_str(&format!(" {name:<name_width$} | {:>digits$} {} -> {} bytes\n", "Bin", size(&change.old), size(&change.new)));
      }
    }
  }
  text.push_str(&summary(changes));
  text
}

fn names(changes: &[FileChange], with_status: bool) -> Vec<u8> {
  let mut text = Vec::new();
  for change in changes {
    if with_status {
      text.extend_from_slice(change.letter().as_bytes());
      text.push(b'\t');
      if let Some(from) = &change.from {
        text.extend_from_slice(&quoted(from, false));
        text.push(b'\t');
      }
    }
    text.extend_from_slice(&quoted(&change.path, false));
    text.push(b'\n');
  }
  text
}

fn rendered(args: &[&str], changes: &[FileChange]) -> Res<Vec<u8>> {
  let chosen: Vec<&str> = ["--name-status", "--name-only", "--stat"].into_iter().filter(|o| has(args, o)).collect();
  Ok(match chosen[..] {
    [] => patch(changes),
    ["--name-status"] => names(changes, true),
    ["--name-only"] => names(changes, false),
    ["--stat"] => diffstat(changes).into_bytes(),
    _ => return unsupported(format_args!("diff {} {}` (two ways of listing at once)", chosen[0], chosen[1])),
  })
}

fn diff_command(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let cached = has(args, "--cached") || has(args, "--staged");
  let root = |repo: &mut Repo, spec: &str| -> Res<Option<Id>> {
    let id = repo.resolve(spec)?;
    Ok(root_of(&mut repo.store, &id)?)
  };
  let changes = match positional("diff", args)[..] {
    [a, b] if !cached => {
      for named in [a, b] {
        if repo.ws.root.join(std::ffi::OsStr::from_bytes(&repo.rooted(named))).exists() {
          return unsupported(format_args!("diff {named}` (a name that is a revision and a file)"));
        }
      }
      let (from, to) = (root(&mut repo, a)?, root(&mut repo, b)?);
      between(&mut repo, from, to)?
    }
    // With conflicts in the tree Git shows both sides against the result, which is not spoken.
    [] if !repo.unmerged().is_empty() => return unsupported("diff` while a merge has conflicts"),
    [] => on_disk(&mut repo, cached)?,
    _ => return unsupported("diff` with this combination of revisions and paths"),
  };
  out(&rendered(args, &changes)?);
  Ok(0)
}

fn show(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let spec = match positional("show", args)[..] {
    [] => "HEAD",
    [spec] => spec,
    _ => return unsupported("show` of more than one thing"),
  };
  // `<rev>:<path>` is the file as that commit has it.
  if spec.contains(':') {
    let id = repo.resolve_any(spec)?;
    return match repo.store.kind_of(&id)? {
      Some(Kind::Blob) => {
        out(&repo.store.get(&id)?.ok_or_else(|| "a file is missing from the store".to_string())?);
        Ok(0)
      }
      _ => unsupported("show` of a directory"),
    };
  }
  if spec.ends_with("^{tree}") {
    return unsupported("show` of a directory");
  }
  let named = repo.resolve_any(spec)?;
  if repo.store.kind_of(&named)? == Some(Kind::Tag) {
    // The tag first, as Git writes one, then the commit it names.
    let bytes = repo.store.get(&named)?.ok_or_else(|| "a tag is missing from the store".to_string())?;
    let text = String::from_utf8_lossy(crate::git::tag_parts(&bytes)?.1).into_owned();
    let (headers, message) = text.split_once("\n\n").unwrap_or((&text, ""));
    let field = |name: &str| headers.lines().find_map(|l| l.strip_prefix(name)).unwrap_or("");
    let tagger = who(field("tagger "));
    // With a format of the caller's, Git leaves out the date and the line that sets the commit apart.
    let when = match format_of(args) {
      Some(_) => String::new(),
      None => format!("Date:   {}\n", date(tagger.2, tagger.3)),
    };
    println!("tag {}\nTagger: {} <{}>\n{when}\n{}", field("tag "), tagger.0, tagger.1, message.trim_end());
    if format_of(args).is_none() {
      println!();
    }
  }
  let id = crate::git::peel(&mut repo.store, named)?;
  let commit = Commit::load(&mut repo.store, &id)?;
  format_spoken(args)?;
  let format = format_of(args);
  out(described(&mut repo, &id, format)?.as_bytes());
  if format.is_some_and(|format| !format.is_empty()) {
    println!();
  }
  // A merge that needed no decisions has nothing of its own to show: an empty line stands for it.
  if commit.parents.len() > 1 {
    println!();
    return Ok(0);
  }
  // Before the diff: an empty line, unless the format was empty or was given as one that separates.
  let spaced = match format {
    None => true,
    Some(format) => !format.is_empty() && !format_separates(args),
  };
  let root = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  let (from, to) = (root(&mut repo.store, commit.parents.first().copied())?, root(&mut repo.store, Some(id))?);
  let changes = between(&mut repo, from, to)?;
  if !changes.is_empty() {
    if spaced {
      println!();
    }
    out(&rendered(args, &changes)?);
  }
  Ok(0)
}

/// `(path, entry)` for the tree `dir`, in Git's order; below subdirectories too when `recursive`.
fn listing(store: &mut Store, dir: &Id, at: &mut Vec<u8>, recursive: bool, into: &mut Vec<(Vec<u8>, Entry)>) -> Res<()> {
  for (name, entry) in git_ordered(store, dir)? {
    let len = at.len();
    if len > 0 {
      at.push(b'/');
    }
    at.extend_from_slice(&name);
    if entry.is_dir() && recursive {
      listing(store, &entry.id, at, recursive, into)?;
    } else {
      into.push((at.clone(), entry));
    }
    at.truncate(len);
  }
  Ok(())
}

fn entry_line(repo: &mut Repo, path: &[u8], entry: &Entry) -> Res<Vec<u8>> {
  let oid = match entry.mode {
    m if m == crate::manifest::MODE_DIR => repo.tree_oid(Some(entry.id))?,
    MODE_GITLINK => hex(&entry.id[..20]),
    _ => repo.blob_oid(&entry.id)?,
  };
  Ok([format!("{:06o} {} {oid}\t", entry.mode, kind_name(entry)).as_bytes(), &quoted(path, false), b"\n"].concat())
}

fn ls_tree(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let (recursive, names) = (has(args, "-r"), has(args, "--name-only"));
  let plain = positional("ls-tree", args);
  let (spec, only) = match plain[..] {
    [spec] => (spec, None),
    [spec, path] => (spec, Some(repo.rooted(path))),
    _ => return unsupported("ls-tree` without exactly one revision and at most one path"),
  };
  // What is listed is a commit's tree, or the directory `<rev>:<path>` names.
  let by_path = spec.contains(':') || spec.ends_with("^{tree}");
  if !repo.prefix.is_empty() && (by_path || plain.get(1).is_some_and(|path| path.split('/').any(|part| part == ".."))) {
    return unsupported("ls-tree` of a path above where it is run, or of `<rev>:<path>` from below the top");
  }
  // `.` is the directory the command was run in, listed: the same as no path at all.
  let only = only.filter(|_| !plain.get(1).is_some_and(|path| path.trim_end_matches('/') == "." && !repo.prefix.is_empty()));
  let top = if by_path {
    let id = repo.resolve_any(spec)?;
    if repo.store.kind_of(&id)? != Some(Kind::Tree) {
      return fatal(128, "fatal: not a tree object");
    }
    Some(id)
  } else {
    let commit = repo.resolve(spec)?;
    root_of(&mut repo.store, &commit)?
  };
  // Run below the top with no path given, Git lists where it was run, with names from there.
  let here = (only.is_none() && !by_path && !repo.prefix.is_empty()).then(|| repo.prefix.clone());
  let mut entries = Vec::new();
  if let Some(root) = top {
    match &here {
      Some(dir) => {
        if let Some(entry) = crate::manifest::lookup(&mut repo.store, root, dir)?.filter(Entry::is_dir) {
          listing(&mut repo.store, &entry.id, &mut Vec::new(), recursive, &mut entries)?;
        }
      }
      None => listing(&mut repo.store, &root, &mut Vec::new(), recursive, &mut entries)?,
    }
  }
  // A path names itself, and with `-r` or a `/` after it what is below it.
  let below = recursive || plain.get(1).is_some_and(|path| path.ends_with('/'));
  if !recursive
    && below
    && let (Some(root), Some(dir)) = (top, &only)
  {
    entries.clear();
    if let Some(entry) = crate::manifest::lookup(&mut repo.store, root, dir)?.filter(Entry::is_dir) {
      listing(&mut repo.store, &entry.id, &mut dir.clone(), false, &mut entries)?;
    }
  }
  // One path, not listed recursively: the entry at that path, wherever in the tree it is.
  if !below && let (Some(root), Some(path)) = (top, &only) {
    entries.clear();
    if let Some(entry) = crate::manifest::lookup(&mut repo.store, root, path)? {
      entries.push((path.clone(), entry));
    }
  }
  let wanted = |path: &Vec<u8>| match &only {
    None => true,
    Some(only) => path == only || (below && path.len() > only.len() && path.starts_with(only) && path[only.len()] == b'/'),
  };
  for (path, entry) in entries.iter().filter(|(p, _)| wanted(p)) {
    // A path that was asked for by name is told from where the command was run, as Git tells it.
    let path = if only.is_some() && !by_path { repo.shown(path) } else { path.clone() };
    if names {
      out(&[quoted(&path, false).as_slice(), b"\n"].concat());
    } else {
      out(&entry_line(&mut repo, &path, entry)?);
    }
  }
  Ok(0)
}

fn cat_file(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let [mode @ ("-t" | "-p" | "-e"), spec] = args else { return unsupported(format_args!("cat-file {}", args.join(" "))) };
  // Asked only whether it is there, a well-formed id that names nothing is quietly not there.
  let missing = || match *mode {
    "-e" if spec.len() == 40 && spec.bytes().all(|b| b.is_ascii_hexdigit()) => Fail { code: 1, message: String::new() },
    _ => Fail { code: 128, message: format!("fatal: Not a valid object name {spec}") },
  };
  // A commit by name, a tree as `<commit>^{tree}`, or any object by its full Git id.
  let id = if let Some(commit) = spec.strip_suffix("^{tree}") {
    let commit = repo.resolve(commit)?;
    root_of(&mut repo.store, &commit)?.ok_or_else(missing)?
  } else if let Ok(named) = repo.resolve_any(spec) {
    named
  } else {
    unhex(spec).and_then(|b| <[u8; 20]>::try_from(b).ok()).and_then(|oid| repo.store.of_git(&oid)).ok_or_else(missing)?
  };
  let kind = repo.store.kind_of(&id)?.ok_or_else(missing)?;
  match (*mode, kind) {
    ("-e", _) => {}
    ("-t", Kind::Commit) => println!("commit"),
    ("-t", Kind::Tree) => println!("tree"),
    ("-t", Kind::Blob) => println!("blob"),
    ("-t", Kind::Tag) => println!("tag"),
    (_, Kind::Tag) => {
      let bytes = repo.store.get(&id)?.ok_or_else(missing)?;
      let (target, rest) = crate::git::tag_parts(&bytes)?;
      out(&crate::git::git_tag_bytes(&crate::git::git_id_of(&mut repo.store, &target)?, rest));
    }
    (_, Kind::Blob) => out(&repo.store.get(&id)?.ok_or_else(missing)?),
    (_, Kind::Tree) => {
      for (name, entry) in git_ordered(&mut repo.store, &id)? {
        out(&entry_line(&mut repo, &name, &entry)?);
      }
    }
    (_, Kind::Commit) => {
      let commit = Commit::load(&mut repo.store, &id)?;
      let mut memo = TreeMemo::default();
      let mut parents = Vec::new();
      for p in &commit.parents {
        parents.push(git_commit_of(&mut repo.store, p, &mut memo, &mut |_, _| Ok(()))?);
      }
      let root = root_of(&mut repo.store, &id)?;
      let tree = git_tree_of(&mut repo.store, root, &mut memo, &mut |_, _| Ok(()))?;
      out(&git_commit_bytes(&tree, &parents, &commit.rest));
    }
  }
  Ok(0)
}

/// Every commit reachable from `id`, itself included.
/// How many commits each of two has that the other has not. Where one is behind the other, which
/// is nearly always, only the commits between them are read; all of history only when they have
/// truly gone apart.
fn apart(store: &mut Store, ours: Id, theirs: Id) -> crate::Result<(usize, usize)> {
  if ours == theirs {
    return Ok((0, 0));
  }
  // From `from` back, not going past `until`: the count, if every way back ends at `until`.
  let mut ahead_of = |from: Id, until: Id| -> crate::Result<Option<usize>> {
    let mut seen = std::collections::HashSet::from([from]);
    let mut todo = vec![from];
    while let Some(at) = todo.pop() {
      if seen.len() > 20_000 {
        return Ok(None);
      }
      let parents = Commit::load(store, &at)?.parents;
      if parents.is_empty() {
        return Ok(None);
      }
      for parent in parents {
        if parent != until && seen.insert(parent) {
          todo.push(parent);
        }
      }
    }
    Ok(Some(seen.len()))
  };
  if let Some(ahead) = ahead_of(ours, theirs)? {
    return Ok((ahead, 0));
  }
  if let Some(behind) = ahead_of(theirs, ours)? {
    return Ok((0, behind));
  }
  let (mine, upstream) = (ancestors(store, ours)?, ancestors(store, theirs)?);
  Ok((mine.difference(&upstream).count(), upstream.difference(&mine).count()))
}

fn ancestors(store: &mut Store, id: Id) -> crate::Result<std::collections::HashSet<Id>> {
  let mut seen = std::collections::HashSet::from([id]);
  let mut todo = vec![id];
  while let Some(at) = todo.pop() {
    for parent in Commit::load(store, &at)?.parents {
      if seen.insert(parent) {
        todo.push(parent);
      }
    }
  }
  Ok(seen)
}

fn branch(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let names = positional("branch", args);
  if has(args, "-") {
    return fatal(
      128,
      "fatal: '-' is not a valid branch name\nhint: See 'git help check-ref-format'\nhint: Disable this message with \"git config set advice.refSyntax false\"",
    );
  }
  let delete = if has(args, "-D") { Some(true) } else { has(args, "-d").then_some(false) };
  match (delete, &names[..]) {
    (None, []) if has(args, "--show-current") => println!("{}", repo.branch().unwrap_or_default()),
    (None, []) => {
      let current = repo.branch();
      for name in repo.branches() {
        println!("{} {name}", if Some(&name) == current.as_ref() { '*' } else { ' ' });
      }
    }
    (None, [name]) => {
      repo.new_ref("heads", name)?;
      let head = repo.resolve("HEAD")?;
      if !repo.store.compare_and_set_ref(&format!("heads/{name}"), None, head)? {
        return fatal(128, format!("fatal: a branch named '{name}' already exists"));
      }
    }
    (Some(force), [name]) => {
      let Some(tip) = repo.branch_tip(name) else { return fatal(1, format!("error: branch '{name}' not found")) };
      if repo.branch().as_deref() == Some(*name) {
        return fatal(1, format!("error: cannot delete branch '{name}' used by worktree at '{}'", repo.ws.root.display()));
      }
      let merged = match repo.ws.head {
        Some(head) => ancestors(&mut repo.store, head)?.contains(&tip),
        None => false,
      };
      if !force && !merged {
        return fatal(
          1,
          format!(
            "error: the branch '{name}' is not fully merged\nhint: If you are sure you want to delete it, run 'git branch -D {name}'"
          ),
        );
      }
      repo.store.delete_ref(&format!("heads/{name}"));
      println!("Deleted branch {name} (was {}).", &repo.git_oid(&tip)?[..abbrev()]);
    }
    _ => return unsupported(format_args!("branch {}", args.join(" "))),
  }
  Ok(0)
}

fn tag(mut repo: Repo, args: &[&str]) -> Res<i32> {
  let names = positional("tag", args);
  if names.is_empty() && (value(args, &["-m"]).is_some() || has(args, "-a")) {
    return unsupported("tag -m` with no name");
  }
  match (has(args, "-d"), &names[..]) {
    (false, []) => {
      for (name, _) in repo.store.refs_under("tags") {
        println!("{name}");
      }
    }
    (false, [name, rest @ ..]) if rest.len() <= 1 && !has(args, "-l") && !has(args, "--list") => {
      repo.new_ref("tags", name)?;
      if let Some(named) = rest.first()
        && repo.resolve_any(named).is_ok_and(|id| matches!(repo.store.kind_of(&id), Ok(Some(Kind::Tag))))
        && value(args, &["-m"]).is_some()
      {
        return unsupported(format_args!("tag -a {name} {named}` (a tag of a tag)"));
      }
      let mut at = repo.resolve(rest.first().copied().unwrap_or("HEAD"))?;
      if repo.store.read_ref(&format!("tags/{name}")).is_some() {
        return fatal(128, format!("fatal: tag '{name}' already exists"));
      }
      // With a message it is an object of its own, naming the commit.
      match value(args, &["-m"]) {
        Some(message) => {
          let rest = format!("type commit\ntag {name}\ntagger {}\n\n{}\n", identity(&repo, "COMMITTER")?, message.trim_end());
          at = crate::git::put_tag(&mut repo.store, &at, rest.as_bytes())?;
          repo.store.flush()?;
        }
        None if has(args, "-a") => return unsupported("tag -a` without -m (there is no editor)"),
        None => {}
      }
      repo.store.compare_and_set_ref(&format!("tags/{name}"), None, at)?;
    }
    (true, [name]) => {
      let Some(at) = repo.store.read_ref(&format!("tags/{name}")) else { return fatal(1, format!("error: tag '{name}' not found.")) };
      repo.store.delete_ref(&format!("tags/{name}"));
      println!("Deleted tag '{name}' (was {})", &repo.git_oid(&at)?[..abbrev()]);
    }
    _ => return unsupported(format_args!("tag {}", args.join(" "))),
  }
  Ok(0)
}

/// `git checkout <branch>` and `git switch <branch>`, their `-b`/`-c` to make one first, and `checkout -- <paths>`.
fn checkout(mut repo: Repo, args: &[&str], create_flag: &str, command: &str) -> Res<i32> {
  let plain = positional(command, args);
  let say = |text: String| {
    if !quiet(args) {
      eprintln!("{text}");
    }
  };
  // Files put back as the index has them: after `--`, or, for `checkout`, when what is named is no branch.
  let paths_meant = has(args, "--")
    || (command == "checkout" && !plain.is_empty() && value(args, &[create_flag]).is_none() && repo.resolve_any(plain[0]).is_err());
  if has(args, "--") && args.iter().position(|a| *a == "--").is_some_and(|at| positional(command, &args[..at]).len() == 1) {
    return unsupported(format_args!("{command} <revision> -- <paths>` (files as another commit has them)"));
  }
  if paths_meant {
    let paths: Vec<Vec<u8>> = plain.iter().map(|a| repo.rooted(a)).collect();
    let before = repo.states()?.iter().filter(|(path, state)| under(&paths, path) && !state.disk_is_index && state.index.is_some()).count();
    let code = put_back(&mut repo, &plain, &paths)?;
    if !has(args, "--") {
      say(format!("Updated {before} path{} from the index", if before == 1 { "" } else { "s" }));
    }
    return Ok(code);
  }
  // A branch is not gone to, or made, in the middle of a merge.
  if repo.merging().is_some() {
    return fatal(128, "fatal: cannot switch branch while merging\nConsider \"git merge --quit\" or \"git worktree add\".");
  }
  // Files that differ from the commit come along to wherever HEAD goes, and Git names them.
  let carried = |repo: &mut Repo| -> Res<()> {
    if quiet(args) {
      return Ok(());
    }
    for (path, state) in repo.states()? {
      let mark = match (state.staged(), state.unstaged()) {
        (Some('D'), _) | (_, Some('D')) => 'D',
        (Some('A'), _) => 'A',
        (Some('M'), _) | (_, Some('M')) => 'M',
        _ => continue,
      };
      out(&[format!("{mark}\t").as_bytes(), &quoted(&repo.shown(&path), false), b"\n"].concat());
    }
    Ok(())
  };
  if let Some(name) = value(args, &[create_flag]) {
    repo.new_ref("heads", name)?;
    if repo.branch_tip(name).is_some() {
      return fatal(128, format!("fatal: a branch named '{name}' already exists"));
    }
    // From where HEAD is, or from the commit named after it.
    let start = match plain[..] {
      [] => repo.ws.head,
      [start] => Some(repo.resolve(start)?),
      _ => return unsupported(format_args!("{command} {}", args.join(" "))),
    };
    if let Some(start) = start {
      if Some(start) != repo.ws.head
        && let Err(refusal) = workspace::switch(&mut repo.store, &mut repo.ws, start)
      {
        return fatal(1, format!("error: {refusal}\nAborting"));
      }
      repo.store.compare_and_set_ref(&format!("heads/{name}"), None, start)?;
    }
    repo.set_branch(name)?;
    carried(&mut repo)?;
    say(format!("Switched to a new branch '{name}'"));
    return Ok(0);
  }
  let [name] = plain[..] else { return unsupported(format_args!("{command} {}", args.join(" "))) };
  // Only a branch: a commit would leave HEAD on none.
  // A branch only the remote has is made here, following it, as Git does.
  if repo.branch_tip(name).is_none()
    && let Some(theirs) = repo.store.read_ref(&format!("remotes/origin/{name}"))
  {
    if let Err(refusal) = workspace::switch(&mut repo.store, &mut repo.ws, theirs) {
      return fatal(1, format!("error: {refusal}\nAborting"));
    }
    repo.store.set_ref(&format!("heads/{name}"), theirs)?;
    repo.set_branch(name)?;
    repo.set_follows(name)?;
    carried(&mut repo)?;
    if !quiet(args) {
      println!("branch '{name}' set up to track 'origin/{name}'.");
    }
    say(format!("Switched to a new branch '{name}'"));
    return Ok(0);
  }
  let Some(tip) = repo.branch_tip(name) else {
    if repo.resolve(name).is_ok() {
      return unsupported(format_args!("{command} {name}` (a commit, not a branch)"));
    }
    return match command {
      "switch" => fatal(128, format!("fatal: invalid reference: {name}")),
      _ => fatal(1, format!("error: pathspec '{name}' did not match any file(s) known to git")),
    };
  };
  let already = repo.branch().as_deref() == Some(name);
  if let Err(refusal) = workspace::switch(&mut repo.store, &mut repo.ws, tip) {
    return fatal(1, format!("error: {refusal}\nAborting"));
  }
  repo.set_branch(name)?;
  carried(&mut repo)?;
  say(if already { format!("Already on '{name}'") } else { format!("Switched to branch '{name}'") });
  Ok(0)
}

/// Git's three-way merge of one file's text, by gitoxide: the merged bytes, and whether every change found its place.
/// When not, the bytes hold both sides' lines between markers.
fn merge_text(base: &[u8], ours: &[u8], theirs: &[u8], their_name: &str) -> (Vec<u8>, bool) {
  merge_text_named(base, ours, theirs, "HEAD", their_name)
}

/// Git's three-way merge of a text file. `false` when the two sides changed the
/// same lines: the text then holds both, between Git's markers.
pub(crate) fn merge_text_named(base: &[u8], ours: &[u8], theirs: &[u8], our_name: &str, their_name: &str) -> (Vec<u8>, bool) {
  use gix_merge::blob::{Resolution, builtin_driver::text};
  let mut merged = Vec::new();
  let mut input = imara_diff::InternedInput::default();
  let labels = text::Labels { ancestor: None, current: Some(our_name.into()), other: Some(their_name.into()) };
  let how = text(&mut merged, &mut input, labels, ours, base, theirs, text::Options::default());
  (merged, how == Resolution::Complete)
}

fn merge(mut repo: Repo, args: &[&str]) -> Res<i32> {
  if has(args, "--abort") {
    if repo.merging().is_none() {
      return fatal(128, "fatal: There is no merge to abort (MERGE_HEAD missing).");
    }
    return abort_merge(repo);
  }
  if repo.merging().is_some() {
    return fatal(
      128,
      "error: Merging is not possible because you have unmerged files.\nfatal: Exiting because of an unresolved conflict.",
    );
  }
  let [name] = positional("merge", args)[..] else { return unsupported("merge` of anything but one branch") };
  let Ok(theirs) = repo.resolve(name) else { return fatal(1, format!("merge: {name} - not something we can merge")) };
  let Some(ours) = repo.ws.head else { return unsupported("merge` into a branch with no commits") };
  let behind = ancestors(&mut repo.store, ours)?;
  if behind.contains(&theirs) {
    if !quiet(args) {
      println!("Already up to date.");
    }
    return Ok(0);
  }
  let ahead = ancestors(&mut repo.store, theirs)?;
  let root = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  // A fast-forward is a checkout: what is staged stays staged, unless the move has to write it.
  if ahead.contains(&ours) && !has(args, "--no-ff") && !repo.staged().is_empty() {
    let (from, to) = (root(&mut repo.store, Some(ours))?, root(&mut repo.store, Some(theirs))?);
    let moved = diff(&mut repo.store, from, to)?;
    let staged = repo.staged();
    if moved.iter().any(|(path, _)| staged.keys().any(|s| under(std::slice::from_ref(s), path) || under(std::slice::from_ref(path), s))) {
      return unsupported("merge` that only moves the branch forward, over a file staged that it changes");
    }
  }
  // Git merges into the last commit, not into what has been added since: with anything staged it will not start.
  let staged: Vec<String> = repo.staged().keys().map(|path| String::from_utf8_lossy(path).into_owned()).collect();
  if !staged.is_empty() && !(ahead.contains(&ours) && !has(args, "--no-ff")) {
    return fatal(
      2,
      format!(
        "error: Your local changes to the following files would be overwritten by merge:\n  {}\nMerge with strategy ort failed.",
        staged.join(" ")
      ),
    );
  }
  if ahead.contains(&ours) && !has(args, "--no-ff") {
    // Nothing of ours that they lack: the branch just moves to where they are.
    if let Err(refusal) = workspace::switch(&mut repo.store, &mut repo.ws, theirs) {
      return fatal(1, format!("error: {refusal}\nAborting"));
    }
    repo.advance(Some(ours), theirs)?;
    if !quiet(args) {
      println!("Updating {}..{}\nFast-forward", &repo.git_oid(&ours)?[..abbrev()], &repo.git_oid(&theirs)?[..abbrev()]);
      brought(&mut repo, ours, theirs)?;
    }
    return Ok(0);
  }
  if has(args, "--ff-only") {
    return fatal(128, "fatal: Not possible to fast-forward, aborting.");
  }
  // The newest commit both have: of theirs, the first found walking back that ours has too.
  let mut base = None;
  let mut todo = std::collections::VecDeque::from([theirs]);
  let mut seen = std::collections::HashSet::new();
  while let Some(at) = todo.pop_front() {
    if behind.contains(&at) {
      base = Some(at);
      break;
    }
    for parent in Commit::load(&mut repo.store, &at)?.parents {
      if seen.insert(parent) {
        todo.push_back(parent);
      }
    }
  }
  if base.is_none() {
    return fatal(128, "fatal: refusing to merge unrelated histories");
  }
  let (from, our_root, their_root) =
    (root(&mut repo.store, base)?, root(&mut repo.store, Some(ours))?, root(&mut repo.store, Some(theirs))?);
  let our_changes = diff(&mut repo.store, from, our_root)?;
  let mut their_changes: Vec<Change> = diff(&mut repo.store, from, their_root)?;
  // What both did alike needs doing once.
  their_changes.retain(|change| !our_changes.contains(change));
  let nested = |a: &[u8], b: &[u8]| {
    (a.len() > b.len() && a.starts_with(b) && a[b.len()] == b'/') || (b.len() > a.len() && b.starts_with(a) && b[a.len()] == b'/')
  };
  if let Some((path, _)) = their_changes.iter().find(|(theirs, _)| our_changes.iter().any(|(ours, _)| nested(ours, theirs))) {
    return unsupported(format_args!("merge {name}` (a file on one side is a directory on the other: {})", String::from_utf8_lossy(path)));
  }
  // What both did differently to one file is merged within the file, where the changes are clear of each other.
  // `marked`: files whose changes met, with both sides' lines left in them.
  let (mut conflicts, mut merged_within) = (Vec::new(), Vec::new());
  let mut marked: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
  for (path, theirs) in &mut their_changes {
    let Some((_, ours)) = our_changes.iter().find(|(p, _)| p == path) else { continue };
    let base = match from {
      Some(root) => crate::manifest::lookup(&mut repo.store, root, path)?,
      None => None,
    };
    let regular = |e: &Entry| e.mode == 0o100644 || e.mode == 0o100755;
    let merged = match (base, *ours, *theirs) {
      (Some(base), Some(ours), Some(theirs)) if regular(&base) && regular(&ours) && regular(&theirs) => {
        let (b, o, t) = (repo.blob(&base.id)?, repo.blob(&ours.id)?, repo.blob(&theirs.id)?);
        let binary = textdiff::binary(&b) || textdiff::binary(&o) || textdiff::binary(&t);
        // The mode goes with whichever side changed it.
        let mode = if ours.mode != base.mode { ours.mode } else { theirs.mode };
        let content = if ours.id == theirs.id {
          Some(o.to_vec())
        } else if binary {
          None
        } else {
          merged_within.push(path.clone());
          let (bytes, settled) = merge_text(&b, &o, &t, name);
          if !settled {
            marked.push((path.clone(), bytes));
            continue;
          }
          Some(bytes)
        };
        match content {
          Some(bytes) => Some(Entry { mode, id: repo.store.put(Kind::Blob, &bytes, Some(ours.id))? }),
          None => None,
        }
      }
      _ => None,
    };
    match merged {
      Some(entry) => *theirs = Some(entry),
      None => conflicts.push(String::from_utf8_lossy(path).into_owned()),
    }
  }
  if !conflicts.is_empty() {
    // Only lines that met within a text file can be left for someone to settle.
    return unsupported(format_args!("merge {name}` (a conflict that is not between lines of text: {})", conflicts.join(", ")));
  }
  // Git says so for each file it had to merge, whether or not it was asked to be quiet; once it
  // knows the merge can be made, and not when it refuses to start.
  let say_merged = || {
    for path in &merged_within {
      out(&[b"Auto-merging ".as_slice(), &quoted(path, false), b"\n"].concat());
      if marked.iter().any(|(p, _)| p == path) {
        out(&[b"CONFLICT (content): Merge conflict in ".as_slice(), &quoted(path, false), b"\n"].concat());
      }
    }
  };
  // What came out as ours already is no change to us.
  their_changes.retain(|change| !our_changes.contains(change));
  let into = repo.branch().filter(|b| b != "main" && b != "master").map(|b| format!(" into {b}")).unwrap_or_default();
  // A tag that is not also a branch is merged as a tag, in Git's words; an annotated one brings its message.
  let as_tag = match repo.store.read_ref(&format!("tags/{name}")).filter(|_| repo.branch_tip(name).is_none()) {
    Some(tag) if repo.store.kind_of(&tag)? == Some(Kind::Tag) => {
      let bytes = repo.store.get(&tag)?.ok_or_else(|| "a tag is missing from the store".to_string())?;
      let text = String::from_utf8_lossy(crate::git::tag_parts(&bytes)?.1).into_owned();
      let said = text.split_once("\n\n").map_or("", |(_, message)| message).trim_end().to_string();
      Some(if said.is_empty() { format!("Merge tag '{name}'{into}") } else { format!("Merge tag '{name}'{into}\n\n{said}") })
    }
    Some(_) => Some(format!("Merge tag '{name}'{into}")),
    None => None,
  };
  let message = value(args, &["-m"]).map(str::to_string).or(as_tag).unwrap_or_else(|| format!("Merge branch '{name}'{into}"));
  if !marked.is_empty() {
    // The merge stops here: what could be merged is in the files and in the index, and the
    // rest is in the files for someone to settle, add and commit.
    their_changes.retain(|(path, _)| !marked.iter().any(|(p, _)| p == path));
    let states = repo.states_of_tracked()?;
    let touched = their_changes.iter().map(|(p, _)| p).chain(marked.iter().map(|(p, _)| p));
    let in_the_way: Vec<String> = touched.filter(|p| states.contains_key(*p)).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
    if !in_the_way.is_empty() {
      return fatal(
        2,
        format!(
          "error: Your local changes to the following files would be overwritten by merge:\n\t{}\nPlease commit your changes or stash them before you merge.\nAborting\nMerge with strategy ort failed.",
          in_the_way.join("\n\t")
        ),
      );
    }
    let blocking = workspace::blocked(&mut repo.store, &repo.ws, &their_changes)?;
    if !blocking.is_empty() {
      return fatal(
        1,
        format!("error: The following untracked working tree files would be overwritten by merge:\n\t{}\nAborting", blocking.join("\n\t")),
      );
    }
    say_merged();
    let mut staged = repo.staged();
    // What goes, first: a file may be written where a directory of files that go stood.
    for (path, entry) in &their_changes {
      if entry.is_none() {
        workspace::unplace(&repo.ws.root, path);
      }
      staged.insert(path.clone(), *entry);
    }
    let placed: Vec<(Vec<u8>, Entry)> = their_changes.iter().filter_map(|(path, entry)| Some((path.clone(), (*entry)?))).collect();
    workspace::write_entries(&mut repo.store, &repo.ws, &placed)?;
    for (path, bytes) in &marked {
      std::fs::write(repo.ws.root.join(std::ffi::OsStr::from_bytes(path)), bytes).map_err(|e| e.to_string())?;
    }
    repo.store.flush()?;
    repo.set_staged(&staged)?;
    // The message its commit will carry, should nobody give another: Git notes what conflicted.
    let noted: String = marked.iter().map(|(path, _)| format!("#\t{}\n", String::from_utf8_lossy(path))).collect();
    crate::write_whole(&repo.meta("git-merge"), format!("{}\n{}\n\n# Conflicts:\n{noted}", hex(&theirs), message.trim_end()))?;
    repo.set_unmerged(&marked.iter().map(|(path, _)| path.clone()).collect())?;
    println!("Automatic merge failed; fix conflicts and then commit the result.");
    return Ok(1);
  }
  // A tracked file the merge has to write that was changed here and not committed: Git will not
  // start. An untracked one in the way is the next check's.
  let states = repo.states_of_tracked()?;
  let in_the_way: Vec<String> =
    their_changes.iter().map(|(p, _)| p).filter(|p| states.contains_key(*p)).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
  if !in_the_way.is_empty() {
    return fatal(
      2,
      format!(
        "error: Your local changes to the following files would be overwritten by merge:\n\t{}\nPlease commit your changes or stash them before you merge.\nAborting\nMerge with strategy ort failed.",
        in_the_way.join("\n\t")
      ),
    );
  }
  if let Err(refusal) = workspace::apply_changes(&mut repo.store, &repo.ws, &their_changes) {
    return fatal(1, format!("error: {refusal}\nAborting"));
  }
  say_merged();
  let rest =
    format!("author {}\ncommitter {}\n\n{}\n", identity(&repo, "AUTHOR")?, identity(&repo, "COMMITTER")?, message.trim_end()).into_bytes();
  let id = Commit { parents: vec![ours, theirs], rest, changes: their_changes }.store(&mut repo.store)?;
  root_of(&mut repo.store, &id)?;
  repo.store.flush()?;
  (repo.ws.base, repo.ws.head) = (Some(id), Some(id));
  repo.ws.save()?;
  repo.advance(Some(ours), id)?;
  if !quiet(args) {
    println!("Merge made by the 'ort' strategy.");
    brought(&mut repo, ours, id)?;
  }
  Ok(0)
}

/// `git merge --abort`, which is Git's `reset --merge`: the files the merge wrote, and what was
/// added since, go back to HEAD. A file the merge did not touch keeps what was done to it and not
/// added: the merge would not have started over an edit to a file it had to write.
fn abort_merge(mut repo: Repo) -> Res<i32> {
  let staged = repo.staged();
  let mut paths: Vec<Vec<u8>> = staged.keys().cloned().chain(repo.unmerged()).collect();
  paths.sort();
  paths.dedup();
  let states = repo.states()?;
  let going: Vec<Vec<u8>> =
    paths.iter().filter(|path| states.get(*path).is_some_and(|state| state.head.is_none() && state.staged().is_some())).cloned().collect();
  let blocking = workspace::hard_reset_blocked(&mut repo.store, &repo.ws, repo.ws.head, &going)?;
  if let Some(first) = blocking.first() {
    return fatal(
      128,
      format!(
        "error: Untracked working tree file '{first}' would be overwritten by merge.\nfatal: Could not reset index file to revision 'HEAD'."
      ),
    );
  }
  for path in &going {
    workspace::unplace(&repo.ws.root, path);
  }
  repo.set_staged(&Staged::new())?;
  repo.end_merge();
  if !paths.is_empty() {
    workspace::restore(&mut repo.store, &repo.ws, &paths)?;
  }
  Ok(0)
}

/// What a merge brought into the branch: the files that changed between where it was and where it is.
fn brought(repo: &mut Repo, was: Id, now: Id) -> Res<()> {
  let (from, to) = (root_of(&mut repo.store, &was)?, root_of(&mut repo.store, &now)?);
  let changed = between(repo, from, to)?;
  out(diffstat(&changed).as_bytes());
  out(file_notes(&changed).as_bytes());
  Ok(())
}

fn config(repo: Repo, args: &[&str]) -> Res<i32> {
  match positional("config", args)[..] {
    [key] => match repo.config(key) {
      Some(value) => println!("{value}"),
      None => return Ok(1),
    },
    [key @ ("user.name" | "user.email"), value] if !has(args, "--get") => {
      let mut file = std::fs::File::options().append(true).create(true).open(repo.meta("git-config")).map_err(|e| e.to_string())?;
      writeln!(file, "{key}={value}").map_err(|e| e.to_string())?;
    }
    [key, _] => return unsupported(format_args!("config {key}` (only user.name and user.email can be set)")),
    _ => return unsupported(format_args!("config {}", args.join(" "))),
  }
  Ok(0)
}

/// Read every object and check it is what its name says; then walk from
/// every ref and check that each commit's parents, trees and files are
/// there. Like Git's, it says nothing when nothing is wrong.
fn fsck(mut repo: Repo) -> Res<i32> {
  let (_, mut wrong) = repo.store.check()?;
  let mut tips: Vec<Id> = ["heads", "tags", "remotes/origin"].iter().flat_map(|p| repo.store.refs_under(p)).map(|(_, id)| id).collect();
  tips.extend(repo.ws.head);
  for tip in &mut tips {
    match crate::git::peel(&mut repo.store, *tip) {
      Ok(commit) => *tip = commit,
      Err(e) => wrong.push(format!("tag {}: {e}", hex(tip))),
    }
  }
  let mut commits = std::collections::HashSet::new();
  let mut trees = std::collections::HashSet::new();
  let mut todo = tips;
  while let Some(id) = todo.pop() {
    if !commits.insert(id) {
      continue;
    }
    let commit = match Commit::load(&mut repo.store, &id) {
      Ok(commit) => commit,
      Err(e) => {
        wrong.push(format!("commit {}: {e}", hex(&id)));
        continue;
      }
    };
    todo.extend(commit.parents.iter().copied());
    // Every directory of the commit's tree, once per directory however many commits share it.
    let mut dirs: Vec<Id> = match root_of(&mut repo.store, &id) {
      Ok(root) => root.into_iter().collect(),
      Err(e) => {
        wrong.push(format!("commit {}: its tree cannot be derived: {e}", hex(&id)));
        continue;
      }
    };
    while let Some(dir) = dirs.pop() {
      if !trees.insert(dir) {
        continue;
      }
      match load(&mut repo.store, &dir) {
        Ok(entries) => {
          for (name, entry) in entries {
            if entry.is_dir() {
              dirs.push(entry.id);
            } else if entry.mode != MODE_GITLINK && !repo.store.contains(&entry.id) {
              wrong.push(format!("missing blob {} ({})", hex(&entry.id), String::from_utf8_lossy(&name)));
            } else if entry.mode == MODE_LINK && repo.store.kind_of(&entry.id)? != Some(Kind::Blob) {
              wrong.push(format!("link {} is not a blob", String::from_utf8_lossy(&name)));
            }
          }
        }
        Err(e) => wrong.push(format!("tree {}: {e}", hex(&dir))),
      }
    }
  }
  for problem in &wrong {
    eprintln!("error: {problem}");
  }
  Ok(if wrong.is_empty() { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn check(line: &str) -> std::result::Result<(), i32> {
    let mut words = line.split(' ');
    let command = words.next().unwrap();
    permitted(command, &words.collect::<Vec<_>>()).map_err(|fail| fail.code)
  }

  #[test]
  fn what_is_in_the_table_passes_and_nothing_else_does() {
    for spoken in [
      "init -q -b main repo",
      "init --initial-branch=main",
      "add -A",
      "add -- -a-file-named-like-an-option",
      "commit -q -am message",
      "commit -m -q",
      "log --oneline -n 3",
      "log -n3 --format=%H",
      "log -5",
      "log --pretty=format:%s main",
      "checkout -q -b topic main",
      "checkout -- path",
      "reset --hard HEAD~1",
      "merge --no-ff -m message side",
      "rm -r -q dir",
      "status",
    ] {
      assert_eq!(check(spoken), Ok(()), "{spoken}");
    }
    for refused in [
      "rebase main",
      "frobnicate",
      "status --branch",
      "log --graph",
      "log --formatted",
      "log -n",
      "log -x5",
      "commit --amend=yes",
      "commit -S",
      "add -p",
      "init --bare",
      "merge --squash side",
      "checkout -B topic",
      "push --force",
      "-C",
    ] {
      assert!(check(refused).is_err(), "{refused}");
    }
    // An option that needs a value and has none is Git's usage error, not ours.
    assert_eq!(check("commit -m"), Err(129));
    assert_eq!(check("rebase"), Err(UNSUPPORTED));
    assert_eq!(check("log --graph"), Err(UNSUPPORTED));
  }

  #[test]
  fn every_spoken_command_is_dispatched() {
    // A command in the table that `run` does not know would be refused only after passing the check.
    // Run where no Git repository is: inside one, Git itself would answer.
    let nowhere = std::env::temp_dir().to_string_lossy().into_owned();
    let at = |args: &[&str]| run(&["-C", &nowhere].iter().chain(args).map(|a| a.to_string()).collect::<Vec<_>>());
    for (name, _) in SPOKEN {
      assert_eq!(at(&[name, "--no-such-option"]), UNSUPPORTED, "{name}");
    }
    assert_eq!(at(&["not-a-command"]), UNSUPPORTED);
  }

  #[test]
  fn paths_are_quoted_as_git_quotes_them() {
    assert_eq!(quoted(b"plain/path.txt", true), b"plain/path.txt");
    assert_eq!(quoted(b"a b", false), b"a b");
    assert_eq!(quoted(b"a b", true), b"\"a b\"");
    assert_eq!(quoted("名".as_bytes(), false), b"\"\\345\\220\\215\"");
    assert_eq!(quoted(b"q\"t\tn\n", false), b"\"q\\\"t\\tn\\n\"");
  }

  #[test]
  fn dates_are_written_in_the_zone_they_were_made_in() {
    assert_eq!(date(1_700_000_000, "+0900"), "Wed Nov 15 07:13:20 2023 +0900");
    assert_eq!(date(1_700_000_000, "-0500"), "Tue Nov 14 17:13:20 2023 -0500");
    assert_eq!(date(0, "+0000"), "Thu Jan 1 00:00:00 1970 +0000");
    assert_eq!(date(951_782_400, "+0000"), "Tue Feb 29 00:00:00 2000 +0000");
  }
}
