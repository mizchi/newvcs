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
  let after = recorded(Look::Everywhere)?;
  let run = Run { state, results, moved: after != Some(state) };
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
  let message = draft(root, state).unwrap_or_else(|| format!("checkpoint: {} passed", names.join(", ")));
  let _held = workspace::lock(root)?;
  let mut ws = Workspace::open(root)?;
  let mut store = Store::open(&ws.store)?;
  workspace::unfold(&mut store, &mut ws)?;
  let rest = workspace::commit_rest(author, &message);
  workspace::commit_state(&mut store, &mut ws, *state, rest, author)
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
