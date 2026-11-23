//! Checkpoints: the states of a workspace that passed what `nv.toml` asks of them.
//!
//! A writer does not commit. It writes, nv records, and the acceptance conditions the
//! repository declares (tests, and stages of a pipeline after them) are run against what was
//! recorded. A state is a checkpoint of each condition it passed, under the name
//! `checkpoint-<name>`. A state that passed them all is one to publish, and a hook drafts its
//! commit message from what changed, so that the commit, when it is made, needs nothing from
//! the writer. A commit can still be made by hand, with a message or without one; it is not
//! made of a state the conditions did not pass, unless that is asked for.
//!
//! ```toml
//! auto = true                 # run the conditions behind every recorded write
//!
//! [checks.unit]
//! run = "cargo test --lib"    # a command; it passes when it exits 0
//! timeout = 600               # seconds; it fails when it takes longer
//!
//! [checks.e2e]
//! run = "pnpm test:e2e"
//! after = ["unit"]            # run only once these passed
//!
//! [message]
//! run = "claude -p 'Write a commit message for this change'"   # the change on stdin, the message on stdout
//! timeout = 120
//! ```
//!
//! The conditions run in the workspace itself. A run whose files changed under it (the writer
//! went on writing) proves nothing about the state it began on: none of its results are kept.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::commit::root_of;
use crate::store::Store;
use crate::workspace::{self, Look, Workspace};
use crate::{Id, Result, hex};

/// The file a repository declares its conditions in, at its top.
pub const FILE: &str = "nv.toml";

/// One acceptance condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
  pub name: String,
  pub run: String,
  pub after: Vec<String>,
  pub timeout: Duration,
}

/// A command run with what changed on its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
  pub run: String,
  pub timeout: Duration,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
  /// In the order they are run: each after those it waits for.
  pub checks: Vec<Check>,
  pub message: Option<Hook>,
  /// Whether a recorded write starts a run behind it.
  pub auto: bool,
  /// Whether each state that passes every condition is committed then and there (`[commit]
  /// each = true`), under its drafted message or a provisional one: commits made to be made
  /// again, fewer and better said, before they are pushed (`nv rebuild`).
  pub commit_each: bool,
}

/// What became of one condition on one state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
  Passed,
  Failed,
  /// Stopped at its limit.
  TimedOut,
  /// A condition it waits for did not pass.
  NotRun,
}

impl Outcome {
  fn word(self) -> &'static str {
    match self {
      Outcome::Passed => "passed",
      Outcome::Failed => "failed",
      Outcome::TimedOut => "did not finish",
      Outcome::NotRun => "not run",
    }
  }
}

/// A run of the conditions on one recorded state.
#[derive(Clone, Debug)]
pub struct Run {
  pub state: Id,
  pub results: Vec<(String, Outcome, Duration)>,
  /// The files changed while it ran: what it found is about no recorded state.
  pub moved: bool,
}

impl Run {
  pub fn passed(&self) -> bool {
    !self.moved && self.results.iter().all(|(_, outcome, _)| *outcome == Outcome::Passed)
  }

  /// One line for each condition, as a person reads it.
  pub fn said(&self) -> String {
    let mut text = String::new();
    for (name, outcome, took) in &self.results {
      text.push_str(&format!("{name}: {}", outcome.word()));
      if *outcome != Outcome::NotRun {
        text.push_str(&format!(" in {:.1} s", took.as_secs_f64()));
      }
      text.push('\n');
    }
    if self.moved {
      text.push_str(
        "the files changed while the conditions ran: what they found is not about any recorded state, and no checkpoint was made\n",
      );
    }
    text
  }
}

/// The conditions of the workspace at `root`, or `None` where it declares none.
pub fn load(root: &Path) -> Result<Option<Config>> {
  let Ok(text) = std::fs::read_to_string(root.join(FILE)) else { return Ok(None) };
  let doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{FILE}: {e}"))?;
  let seconds = |item: Option<&toml_edit::Item>, default: u64| -> Result<Duration> {
    match item {
      None => Ok(Duration::from_secs(default)),
      Some(item) => item
        .as_integer()
        .filter(|n| *n > 0)
        .map(|n| Duration::from_secs(n as u64))
        .ok_or_else(|| format!("{FILE}: a timeout is a number of seconds, more than 0")),
    }
  };
  let mut checks = Vec::new();
  if let Some(table) = doc.get("checks") {
    let table = table.as_table_like().ok_or_else(|| format!("{FILE}: `checks` is a table of conditions"))?;
    for (name, item) in table.iter() {
      let run = item
        .get("run")
        .and_then(|run| run.as_str())
        .ok_or_else(|| format!("{FILE}: the condition `{name}` has no `run`, the command it is"))?
        .to_string();
      let after = match item.get("after") {
        None => Vec::new(),
        Some(after) => after
          .as_array()
          .ok_or_else(|| format!("{FILE}: `after` of `{name}` is a list of conditions"))?
          .iter()
          .map(|v| v.as_str().map(str::to_string).ok_or_else(|| format!("{FILE}: `after` of `{name}` names conditions")))
          .collect::<Result<Vec<_>>>()?,
      };
      if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)) || name.is_empty() {
        return Err(format!("{FILE}: `{name}` cannot name a condition: letters, digits, `-` and `_` only"));
      }
      checks.push(Check { name: name.to_string(), run, after, timeout: seconds(item.get("timeout"), 600)? });
    }
  }
  let message = match doc.get("message") {
    None => None,
    Some(item) => Some(Hook {
      run: item.get("run").and_then(|run| run.as_str()).ok_or_else(|| format!("{FILE}: `message` has no `run`"))?.to_string(),
      timeout: seconds(item.get("timeout"), 120)?,
    }),
  };
  let auto = doc.get("auto").and_then(|auto| auto.as_bool()).unwrap_or(false);
  let commit_each = match doc.get("commit") {
    None => false,
    Some(item) => match item.get("each") {
      None => false,
      Some(each) => each.as_bool().ok_or_else(|| format!("{FILE}: `each` of `commit` is true or false"))?,
    },
  };
  Ok(Some(Config { checks: in_order(checks)?, message, auto, commit_each }))
}

/// Each condition after those it waits for, in the order declared otherwise.
fn in_order(checks: Vec<Check>) -> Result<Vec<Check>> {
  for check in &checks {
    if let Some(unknown) = check.after.iter().find(|name| !checks.iter().any(|c| &c.name == *name)) {
      return Err(format!("{FILE}: `{}` waits for `{unknown}`, which is no condition here", check.name));
    }
  }
  let mut done: Vec<Check> = Vec::new();
  let mut left = checks;
  while !left.is_empty() {
    let ready = left.iter().position(|check| check.after.iter().all(|name| done.iter().any(|d| &d.name == name)));
    let Some(ready) = ready else {
      let names: Vec<&str> = left.iter().map(|c| c.name.as_str()).collect();
      return Err(format!("{FILE}: the conditions {} wait for each other in a circle", names.join(", ")));
    };
    done.push(left.remove(ready));
  }
  Ok(done)
}

fn runs_dir(root: &Path) -> PathBuf {
  root.join(workspace::META).join("checks")
}

/// Record what is on disk, run the conditions against it, and make checkpoints of what passed.
/// The workspace is held only to record and to name: never while a condition runs, which can
/// take minutes while the writer goes on writing.
pub fn run(root: &Path, config: &Config, author: &str) -> Result<Run> {
  let recorded = |look: Look| -> Result<Option<Id>> {
    let _held = workspace::lock(root)?;
    let mut ws = Workspace::open(root)?;
    let mut store = Store::open(&ws.store)?;
    workspace::unfold(&mut store, &mut ws)?;
    workspace::snapshot(&mut store, &mut ws, author, look)?;
    Ok(ws.head)
  };
  let state = recorded(Look::Everywhere)?.ok_or("there is nothing recorded to run the conditions on")?;
  // A state they passed on before passes still: they are about the files, and the files are the same.
  if passed_on(root, config, &state) {
    let results = config.checks.iter().map(|c| (c.name.clone(), Outcome::Passed, Duration::ZERO)).collect();
    return Ok(Run { state, results, moved: false });
  }
  let dir = runs_dir(root);
  std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
  let mut results: Vec<(String, Outcome, Duration)> = Vec::new();
  for check in &config.checks {
    let waited_on = check.after.iter().all(|name| results.iter().any(|(n, o, _)| n == name && *o == Outcome::Passed));
    if !waited_on {
      results.push((check.name.clone(), Outcome::NotRun, Duration::ZERO));
      continue;
    }
    let log = std::fs::File::create(dir.join(format!("{}.log", check.name))).map_err(|e| e.to_string())?;
    let mut command = std::process::Command::new("sh");
    command
      .arg("-c")
      .arg(&check.run)
      .current_dir(root)
      .env("NV_STATE", hex(&state))
      .stdin(std::process::Stdio::null())
      .stdout(log.try_clone().map_err(|e| e.to_string())?)
      .stderr(log);
    let started = Instant::now();
    let outcome = match crate::limits::run(&mut command, Some(check.timeout), &format!("the condition `{}`", check.name)) {
      Ok(status) if status.success() => Outcome::Passed,
      Ok(_) => Outcome::Failed,
      Err(why) if why.contains("did not finish") => Outcome::TimedOut,
      Err(why) => return Err(why),
    };
    results.push((check.name.clone(), outcome, started.elapsed()));
  }
  // What the run found is about the state it began on only if the files are that state still.
  // The files, not the state's id: a runner behind this one may have committed that state
  // meanwhile, which moves the head and leaves the files as they were.
  let after = recorded(Look::Everywhere)?;
  let moved = match after {
    Some(after) if after == state => false,
    Some(after) => {
      let ws = Workspace::open(root)?;
      let mut store = Store::open(&ws.store)?;
      root_of(&mut store, &after)? != root_of(&mut store, &state)?
    }
    None => true,
  };
  let run = Run { state, results, moved };
  if !run.moved {
    let _held = workspace::lock(root)?;
    let ws = Workspace::open(root)?;
    for (name, outcome, _) in &run.results {
      if *outcome == Outcome::Passed {
        workspace::name_state(&ws, &format!("checkpoint-{name}"), &state)?;
      }
    }
    let line = |(name, outcome, _): &(String, Outcome, Duration)| format!("{name} {outcome:?}\n");
    let said: String = run.results.iter().map(line).collect();
    crate::write_whole(&dir.join(hex(&state)), &said)?;
    // And under its files: a commit of the same files, made again, passed as well.
    let mut store = Store::open(&ws.store)?;
    if let Some(tree) = root_of(&mut store, &state)? {
      crate::write_whole(&dir.join(format!("tree-{}", hex(&tree))), said)?;
    }
  }
  Ok(run)
}

/// The conditions that passed on these files, by the id of their tree.
pub fn passed_on_tree(root: &Path, tree: &Id) -> Vec<String> {
  let text = std::fs::read_to_string(runs_dir(root).join(format!("tree-{}", hex(tree)))).unwrap_or_default();
  text.lines().filter_map(|line| line.strip_suffix(" Passed")).map(str::to_string).collect()
}

/// Commit a state every condition passed on, as `[commit] each` asks: under the message drafted
/// for it, or a provisional one that says what passed. The writer may have gone on writing; what
/// it wrote since stays recorded on top.
pub fn commit_checkpoint(root: &Path, config: &Config, state: &Id, author: &str) -> Result<Option<Id>> {
  let names: Vec<&str> = config.checks.iter().map(|c| c.name.as_str()).collect();
  let _held = workspace::lock(root)?;
  let mut ws = Workspace::open(root)?;
  let mut store = Store::open(&ws.store)?;
  workspace::unfold(&mut store, &mut ws)?;
  // The writer's own word for these files first (`nv note`), then a drafted message, then one
  // that says only what passed.
  let (noted, taken) = notes_for(root, &mut store, &ws, state)?;
  let message = noted.or_else(|| draft(root, state)).unwrap_or_else(|| format!("{PROVISIONAL}{} passed", names.join(", ")));
  // Signed as any commit made here is: nv does not make a name up.
  let signed = |role: &str| workspace::signature(&ws, role).map_err(|why| format!("not committed: {why}"));
  let rest = format!("author {}\ncommitter {}\n\n{}\n", signed("AUTHOR")?, signed("COMMITTER")?, message.trim_end()).into_bytes();
  let made = workspace::commit_state(&mut store, &mut ws, *state, rest, author)?;
  // Which commit was the runner's own, for a command to say so: a note, nothing depends on it.
  if let Some(id) = made {
    let _ = crate::write_whole(&runs_dir(root).join("committed"), hex(&id).as_bytes());
    forget_notes(root, &taken)?;
  }
  Ok(made)
}

/// How a message the runner made up begins: `nv publish` knows such a commit for one to fold.
pub const PROVISIONAL: &str = "checkpoint: ";

fn notes_file(root: &Path) -> PathBuf {
  runs_dir(root).join("notes")
}

/// The notes waiting for a commit: the state each was made on, and its line.
fn notes(root: &Path) -> Vec<(String, String)> {
  std::fs::read_to_string(notes_file(root))
    .unwrap_or_default()
    .lines()
    .filter_map(|line| line.split_once('\t').map(|(state, text)| (state.to_string(), text.to_string())))
    .collect()
}

fn forget_notes(root: &Path, taken: &[String]) -> Result<()> {
  if taken.is_empty() {
    return Ok(());
  }
  let left: String =
    notes(root).into_iter().filter(|(state, _)| !taken.contains(state)).map(|(state, text)| format!("{state}\t{text}\n")).collect();
  crate::write_whole(&notes_file(root), left.as_bytes())
}

/// The notes made on `state` or on a state recorded before it since the last commit, as one
/// message (the first note its first line), and which they were.
fn notes_for(root: &Path, store: &mut Store, ws: &Workspace, state: &Id) -> Result<(Option<String>, Vec<String>)> {
  let waiting = notes(root);
  if waiting.is_empty() {
    return Ok((None, Vec::new()));
  }
  let mut before: Vec<String> = Vec::new();
  let mut at = Some(*state);
  while let Some(id) = at.filter(|id| Some(*id) != ws.base) {
    before.push(hex(&id));
    if before.len() > 100_000 {
      break;
    }
    at = crate::commit::Commit::load(store, &id)?.parents.first().copied();
  }
  let taken: Vec<(String, String)> = waiting.into_iter().filter(|(noted, _)| before.contains(noted)).collect();
  if taken.is_empty() {
    return Ok((None, Vec::new()));
  }
  let lines: Vec<&str> = taken.iter().map(|(_, text)| text.as_str()).collect();
  let message = match lines.split_first() {
    Some((first, [])) => first.to_string(),
    Some((first, more)) => format!("{first}\n\n{}", more.join("\n")),
    None => unreachable!("taken is not empty"),
  };
  Ok((Some(message), taken.into_iter().map(|(state, _)| state).collect()))
}

/// A writer's line for what is on disk (`nv note`), to be the message of the next commit the
/// conditions pass on, of these files or of files written after them. Where the runner has
/// committed these files already, under a message it made up, that commit takes the line. Run
/// with the workspace held.
pub fn note(root: &Path, text: &str, author: &str) -> Result<String> {
  let text = text.trim();
  if text.is_empty() || text.contains('\n') {
    return Err("a note is one line that says what was done".to_string());
  }
  let mut ws = Workspace::open(root)?;
  let mut store = Store::open(&ws.store)?;
  workspace::unfold(&mut store, &mut ws)?;
  workspace::snapshot(&mut store, &mut ws, author, Look::Everywhere)?;
  let state = ws.head.ok_or("there is nothing recorded to note")?;
  // Committed already, by the runner, under a message of its own making: that commit is renamed.
  if let Some(base) = ws.base.filter(|base| ws.head == Some(*base) && committed_by_runner(root, base)) {
    let chain = workspace::pending(&mut store, &ws)?;
    let last = chain.last().map(|(id, _)| *id);
    if last == Some(base) && String::from_utf8_lossy(&chain.last().expect("not empty").1.rest).contains(&format!("\n\n{PROVISIONAL}")) {
      let plan: Vec<(Id, Vec<u8>)> = chain
        .iter()
        .map(|(id, commit)| {
          if *id == base {
            let said = String::from_utf8_lossy(&commit.rest).into_owned();
            let head = said.split_once("\n\n").map_or(said.as_str(), |(h, _)| h);
            (*id, format!("{head}\n\n{text}\n").into_bytes())
          } else {
            (*id, commit.rest.clone())
          }
        })
        .collect();
      let made = workspace::rebuild(&mut store, &mut ws, &plan, author)?;
      if let Some(id) = made.last() {
        let _ = crate::write_whole(&runs_dir(root).join("committed"), hex(id).as_bytes());
      }
      return Ok(format!("the commit of these files, made already, is now \"{text}\""));
    }
  }
  std::fs::create_dir_all(runs_dir(root)).map_err(|e| e.to_string())?;
  let mut all = notes(root);
  all.retain(|(noted, _)| *noted != hex(&state));
  all.push((hex(&state), text.to_string()));
  let lines: String = all.into_iter().map(|(state, text)| format!("{state}\t{text}\n")).collect();
  crate::write_whole(&notes_file(root), lines.as_bytes())?;
  Ok(format!("noted: the next commit the conditions pass on, of these files or later ones, is \"{text}\""))
}

/// Whether this commit is one the conditions' runner made (`[commit] each`), as far as its note says.
pub fn committed_by_runner(root: &Path, id: &Id) -> bool {
  std::fs::read_to_string(runs_dir(root).join("committed")).is_ok_and(|said| said.trim() == hex(id))
}

/// Whether the conditions are known to have passed on this state.
pub fn passed_on(root: &Path, config: &Config, state: &Id) -> bool {
  let Ok(text) = std::fs::read_to_string(runs_dir(root).join(hex(state))) else { return false };
  config.checks.iter().all(|check| text.lines().any(|line| line == format!("{} Passed", check.name)))
}

/// The change from the workspace's last commit to `state`, as `git diff` writes one: what the
/// message hook reads.
pub fn change(store: &mut Store, ws: &Workspace, state: &Id) -> Result<Vec<u8>> {
  let tree = |store: &mut Store, id: Option<Id>| id.map_or(Ok(None), |id| root_of(store, &id));
  let (from, to) = (tree(store, ws.base)?, tree(store, Some(*state))?);
  let mut out = Vec::new();
  let changes = crate::manifest::diff(store, from, to)?;
  for (path, entry) in crate::manifest::expand(store, from, &changes)? {
    let before = match from {
      Some(root) => crate::manifest::lookup(store, root, &path)?,
      None => None,
    };
    let bytes = |store: &mut Store, entry: Option<crate::manifest::Entry>| -> Result<Vec<u8>> {
      Ok(match entry.filter(|e| !e.is_dir()) {
        Some(entry) => store.get(&entry.id)?.map(|b| b.to_vec()).unwrap_or_default(),
        None => Vec::new(),
      })
    };
    let (old, new) = (bytes(store, before)?, bytes(store, entry)?);
    let name = String::from_utf8_lossy(&path);
    out.extend_from_slice(format!("diff --git a/{name} b/{name}\n").as_bytes());
    if crate::textdiff::binary(&old) || crate::textdiff::binary(&new) {
      out.extend_from_slice(b"Binary files differ\n");
      continue;
    }
    let (a, b) = (
      if before.is_some() { format!("a/{name}") } else { "/dev/null".into() },
      if entry.is_some() { format!("b/{name}") } else { "/dev/null".into() },
    );
    out.extend_from_slice(format!("--- {a}\n+++ {b}\n").as_bytes());
    out.extend_from_slice(&crate::textdiff::hunks(&old, &new));
  }
  Ok(out)
}

fn draft_file(root: &Path, state: &Id) -> PathBuf {
  runs_dir(root).join(format!("{}.message", hex(state)))
}

/// The message drafted for this state, if one was.
pub fn draft(root: &Path, state: &Id) -> Option<String> {
  std::fs::read_to_string(draft_file(root, state)).ok().map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
}

/// Have the message hook write a message for `state`, from the change since the last commit,
/// and keep it as that state's draft.
pub fn write_draft(root: &Path, hook: &Hook, state: &Id) -> Result<String> {
  let change = {
    let ws = Workspace::open(root)?;
    let mut store = Store::open(&ws.store)?;
    change(&mut store, &ws, state)?
  };
  let dir = runs_dir(root);
  std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
  let input = dir.join(format!("{}.change", hex(state)));
  std::fs::write(&input, &change).map_err(|e| e.to_string())?;
  let output = dir.join(format!("{}.drafting", hex(state)));
  let mut command = std::process::Command::new("sh");
  command
    .arg("-c")
    .arg(&hook.run)
    .current_dir(root)
    .env("NV_STATE", hex(state))
    .stdin(std::fs::File::open(&input).map_err(|e| e.to_string())?)
    .stdout(std::fs::File::create(&output).map_err(|e| e.to_string())?)
    .stderr(std::process::Stdio::null());
  let status = crate::limits::run(&mut command, Some(hook.timeout), "the message hook");
  let _ = std::fs::remove_file(&input);
  let status = status?;
  let message = std::fs::read_to_string(&output).unwrap_or_default().trim().to_string();
  let _ = std::fs::remove_file(&output);
  if !status.success() || message.is_empty() {
    return Err(format!("the message hook gave no message (exit {})", status.code().unwrap_or(-1)));
  }
  crate::write_whole(&draft_file(root, state), format!("{message}\n"))?;
  Ok(message)
}

/// The lock the one runner of a workspace holds while it lives.
fn runner_lock(root: &Path) -> Option<std::fs::File> {
  use std::os::fd::AsRawFd;
  let dir = runs_dir(root);
  std::fs::create_dir_all(&dir).ok()?;
  let file = std::fs::File::options().create(true).append(true).open(dir.join("runner.lock")).ok()?;
  // SAFETY: the descriptor is open for as long as `file` is.
  (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

/// Run the conditions on what is recorded until a run finds the files as it began, drafting a
/// message where they all pass. One runner to a workspace: a second returns at once, and the
/// one running takes in what was written meanwhile on its next round.
pub fn run_until_still(root: &Path, config: &Config, author: &str) -> Result<Option<Run>> {
  let Some(_held) = runner_lock(root) else { return Ok(None) };
  let mut last = None;
  for _ in 0..20 {
    let run = run(root, config, author)?;
    let moved = run.moved;
    if run.passed()
      && let Some(hook) = &config.message
      && draft(root, &run.state).is_none()
    {
      let _ = write_draft(root, hook, &run.state);
    }
    if run.passed() && config.commit_each {
      match commit_checkpoint(root, config, &run.state, author) {
        Ok(Some(id)) => println!("{} passed every condition, and is committed as {}", hex(&run.state), hex(&id)),
        Ok(None) => {}
        Err(why) => eprintln!("nv: {} passed every condition, and was not committed: {why}", hex(&run.state)),
      }
    }
    last = Some(run);
    if !moved {
      break;
    }
  }
  Ok(last)
}

/// Wait until nothing more is coming: the watcher has recorded every write made before this
/// call, no run of the conditions is under way, and what is recorded has its verdict where the
/// conditions run behind writes (`auto`): committed, or known not to have passed. A commit no
/// run has reached, as a sync or a push leaves one, is run on. Says which, in a line. Gives up
/// at `limits::command()`.
pub fn wait_settled(root: &Path) -> Result<String> {
  let started = Instant::now();
  let limit = crate::limits::command();
  let config = load(root)?;
  let runs = config.as_ref().is_some_and(|c| c.auto && !c.checks.is_empty());
  let each = config.as_ref().is_some_and(|c| c.commit_each);
  // When a runner this command started was seen to have finished without reaching the state.
  let mut started_runner: Option<Instant> = None;
  loop {
    let watched = crate::watch::settle(root);
    let idle = runner_lock(root);
    let (head, base, tree) = {
      // Held for the look only: snapshots `nv git` set aside are taken up again first.
      let _held = workspace::lock(root)?;
      let mut ws = Workspace::open(root)?;
      let mut store = Store::open(&ws.store)?;
      workspace::unfold(&mut store, &mut ws)?;
      let tree = match ws.head {
        Some(head) => root_of(&mut store, &head)?,
        None => None,
      };
      (ws.head, ws.base, tree)
    };
    let unrecorded = if watched { "" } else { " (no watcher is running: writes since the last snapshot are not in this)" };
    if idle.is_some() {
      let committed = head == base;
      // What a run found on these files, by the state or by its tree.
      let said = head
        .and_then(|h| std::fs::read_to_string(runs_dir(root).join(hex(&h))).ok())
        .or_else(|| tree.and_then(|t| std::fs::read_to_string(runs_dir(root).join(format!("tree-{}", hex(&t)))).ok()));
      let failed = said.as_ref().map(|said| -> Vec<String> {
        said.lines().filter(|l| !l.ends_with(" Passed")).filter_map(|l| l.split(' ').next()).map(str::to_string).collect()
      });
      let passed = said.as_ref().map(|said| -> Vec<&str> { said.lines().filter_map(|l| l.strip_suffix(" Passed")).collect() });
      match (committed, failed, &passed) {
        (true, _, _) if !runs => return Ok(format!("settled: what is recorded is committed{unrecorded}")),
        (true, Some(failed), _) if failed.is_empty() => {
          let passed = passed.clone().unwrap_or_default().join(", ");
          return Ok(format!("settled: what is recorded is committed, and passed {passed}{unrecorded}"));
        }
        (true, Some(failed), _) => {
          return Ok(format!("settled: what is recorded is committed, and did not pass ({} did not){unrecorded}", failed.join(", ")));
        }
        (false, Some(failed), _) if failed.is_empty() && each => {
          return Ok(format!(
            "settled: the conditions passed on what is recorded, and it was not committed: `nv checkpoint <dir>` says why{unrecorded}"
          ));
        }
        (false, Some(failed), _) if failed.is_empty() => {
          return Ok(format!(
            "settled: the conditions passed on what is recorded, which is not committed ([commit] each is off){unrecorded}"
          ));
        }
        (false, Some(failed), _) => {
          return Ok(format!(
            "settled: what is recorded since the last commit did not pass ({} did not); it stays uncommitted{unrecorded}",
            failed.join(", ")
          ));
        }
        (false, None, _) if !runs => {
          return Ok(format!(
            "settled: changes are recorded that are not committed, and the conditions are not run on their own here (`auto`){unrecorded}"
          ));
        }
        // No run has reached what is recorded: one is about to start, or none was started.
        (_, None, _) => {
          drop(idle);
          match started_runner {
            None if started.elapsed() > Duration::from_secs(1) => {
              start_behind(root);
              started_runner = Some(Instant::now());
            }
            // Started, and done, and it did not reach this state: it will not.
            Some(at) if at.elapsed() > Duration::from_secs(2) => {
              let what = if committed { "committed" } else { "not committed" };
              return Ok(format!(
                "settled: what is recorded is {what}, and the conditions could not be run on it (.nv/checks/runner.log says why){unrecorded}"
              ));
            }
            _ => {}
          }
        }
      }
    }
    if limit.is_some_and(|limit| started.elapsed() >= limit) {
      return Err(format!(
        "waited {} s for the conditions to finish on what is recorded, and gave up (NV_TIMEOUT sets how long)",
        started.elapsed().as_secs()
      ));
    }
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// Start a runner behind this command, if the workspace asks for one and none is running.
pub fn start_behind(root: &Path) {
  use std::os::unix::process::CommandExt;
  if !load(root).ok().flatten().is_some_and(|config| config.auto && !config.checks.is_empty()) {
    return;
  }
  // A runner alive already will see this write on its next round.
  if runner_lock(root).is_none() {
    return;
  }
  let Ok(exe) = std::env::current_exe() else { return };
  let dir = runs_dir(root);
  let Ok(log) = std::fs::File::options().create(true).append(true).open(dir.join("runner.log")) else { return };
  let mut command = std::process::Command::new(exe);
  command.arg("checkpoint").arg(root).arg("--until-still").stdin(std::process::Stdio::null());
  if let Ok(out) = log.try_clone() {
    command.stdout(out).stderr(log);
  }
  // A session of its own: whoever started it may be gone long before the conditions are.
  // SAFETY: `setsid` is safe to call between fork and exec.
  unsafe { command.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }) };
  let _ = command.spawn();
}

#[cfg(test)]
mod tests {
  use super::*;

  fn check(name: &str, after: &[&str]) -> Check {
    Check { name: name.into(), run: "true".into(), after: after.iter().map(|s| s.to_string()).collect(), timeout: Duration::from_secs(1) }
  }

  #[test]
  fn conditions_run_after_those_they_wait_for() {
    let order = in_order(vec![check("e2e", &["unit", "lint"]), check("unit", &[]), check("lint", &[])]).unwrap();
    assert_eq!(order.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["unit", "lint", "e2e"]);
    assert!(in_order(vec![check("a", &["b"]), check("b", &["a"])]).unwrap_err().contains("circle"));
    assert!(in_order(vec![check("a", &["nope"])]).unwrap_err().contains("nope"));
  }
}
