//! A watcher: the operating system says which files were written, by
//! whatever wrote them, and each burst of writes becomes a snapshot.
//!
//! A hook covers the tools that report their writes. A shell command, a
//! formatter, a build, an editor report nothing; without a watcher the only
//! way to find what they wrote is to walk the tree. With one, every write
//! is reported, and the commands that take `--recorded` need no walk.

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

use crate::store::Store;
use crate::workspace::{self, Look, Workspace};
use crate::{Result, hex};

/// Writes this close together are one snapshot.
const QUIET: Duration = Duration::from_millis(25);
/// However long a burst goes on, it is recorded at least this often.
const AT_MOST: Duration = Duration::from_millis(250);

/// Watch a workspace until killed. Prints `watching <dir>` once events are
/// being received, then a line for every snapshot taken.
/// The lock a watcher holds for as long as it lives. Whether it is held is
/// whether a watcher is alive: a process id in a file can outlive its
/// process and be given to another.
fn watch_lock(root: &Path, wait: bool) -> Option<std::fs::File> {
  use std::os::fd::AsRawFd;
  let file = std::fs::File::options().create(true).append(true).open(root.join(".nv").join("watch.lock")).ok()?;
  let how = libc::LOCK_EX | if wait { 0 } else { libc::LOCK_NB };
  // SAFETY: the descriptor is open for as long as `file` is.
  (unsafe { libc::flock(file.as_raw_fd(), how) } == 0).then_some(file)
}

pub fn alive(root: &Path) -> bool {
  root.join(".nv").join("watch.lock").exists() && watch_lock(root, false).is_none()
}

/// Whether the workspace is meant to be watched: someone started a watcher
/// with `--start` and nobody has stopped it with `--stop`.
pub fn expected(root: &Path) -> bool {
  root.join(".nv").join("watch.expected").exists()
}

/// One line on how the workspace is watched.
pub fn status(root: &Path) -> String {
  let pid = std::fs::read_to_string(root.join(".nv").join("watch.pid")).unwrap_or_default();
  match (alive(root), expected(root)) {
    (true, _) => format!("watched by process {}", pid.trim()),
    (false, true) => {
      "the watcher is not running: writes are going unreported until `nv watch <dir> --start`, a hook or a command with --recorded starts it again"
        .into()
    }
    (false, false) => "not watched".into(),
  }
}

/// Start a watcher that outlives this command, and return once it is ready:
/// it has looked at every file once and is receiving events.
pub fn start(root: &Path) -> Result<String> {
  use std::os::unix::process::CommandExt;
  let meta = root.join(".nv");
  if !meta.join("workspace").is_file() {
    return Err(format!("{} is not a workspace", root.display()));
  }
  std::fs::write(meta.join("watch.expected"), b"").map_err(|e| e.to_string())?;
  if alive(root) {
    return Ok(format!("already {}", status(root)));
  }
  let _ = std::fs::remove_file(meta.join("watch.pid"));
  let log = std::fs::File::options().create(true).append(true).open(meta.join("watch.log")).map_err(|e| e.to_string())?;
  let mut command = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
  command.arg("watch").arg(root).stdin(std::process::Stdio::null()).stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log);
  // A session of its own: closing the terminal it was started from does not stop it.
  // SAFETY: `setsid` is safe to call between fork and exec.
  unsafe { command.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }) };
  let child = command.spawn().map_err(|e| format!("cannot start a watcher: {e}"))?;
  let asked = Instant::now();
  let ready = || std::fs::read_to_string(meta.join("watch.pid")).is_ok_and(|pid| pid.trim() == child.id().to_string());
  while !ready() {
    if asked.elapsed() > Duration::from_secs(30) {
      return Err(format!("the watcher did not come up; see {}", meta.join("watch.log").display()));
    }
    std::thread::sleep(Duration::from_millis(2));
  }
  Ok(status(root))
}

/// Stop the watcher and no longer expect one.
pub fn stop(root: &Path) -> Result<String> {
  let meta = root.join(".nv");
  let _ = std::fs::remove_file(meta.join("watch.expected"));
  if alive(root)
    && let Some(pid) = std::fs::read_to_string(meta.join("watch.pid")).ok().and_then(|text| text.trim().parse::<i32>().ok())
  {
    // SAFETY: a signal to a process id read from the watcher's own file, which is known to be alive.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let asked = Instant::now();
    while alive(root) && asked.elapsed() < Duration::from_secs(5) {
      std::thread::sleep(Duration::from_millis(2));
    }
  }
  let _ = std::fs::remove_file(meta.join("watch.pid"));
  Ok(status(root))
}

/// Start the watcher again if the workspace is meant to have one and it is
/// gone. What was written meanwhile is found by the look at every file a
/// watcher takes as it starts.
pub fn ensure(root: &Path) {
  if expected(root) && !alive(root) {
    match start(root) {
      Ok(_) => eprintln!("nv: the watcher was not running; it was started again and has looked at every file"),
      Err(why) => eprintln!("nv: the watcher is not running and could not be started: {why}"),
    }
  }
}

/// Wait until a watcher of this workspace, if one is running, has recorded
/// every write made so far. Without this a command that trusts reported
/// writes could run in the moment between a write and its being reported.
/// A watcher that should be running and is not is started first. Returns at
/// once when nothing is watching and nothing is meant to.
pub fn settle(root: &Path) {
  ensure(root);
  let meta = root.join(".nv");
  if !alive(root) {
    return;
  }
  let question = meta.join(format!("asked-{}", std::process::id()));
  if std::fs::write(&question, b"").is_err() {
    return;
  }
  let asked = Instant::now();
  while question.exists() && asked.elapsed() < Duration::from_secs(2) {
    std::thread::sleep(Duration::from_millis(2));
  }
  if question.exists() {
    let _ = std::fs::remove_file(&question);
    eprintln!("nv: the watcher did not answer; going on with what has been reported so far");
  }
}

pub fn watch(root: &Path, author: &str) -> Result<()> {
  let root = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
  Workspace::open(&root)?;
  // One watcher to a workspace, and the lock is how others know it lives.
  let Some(_alive) = watch_lock(&root, false) else { return Err(format!("{} is watched already", root.display())) };
  // Whoever started the watcher may stop reading what it says. That is no reason to stop watching.
  // SAFETY: set once, before anything is written.
  unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
  let (sender, events) = channel();
  let mut watcher = notify::recommended_watcher(sender).map_err(|e| format!("cannot watch: {e}"))?;
  watcher.watch(&root, RecursiveMode::Recursive).map_err(|e| format!("cannot watch {}: {e}", root.display()))?;
  // What was written while nobody watched: one look at every file. Events are already being
  // received, so nothing falls between this look and the first of them.
  {
    let _lock = workspace::lock(&root)?;
    let mut ws = Workspace::open(&root)?;
    let mut store = Store::open(&ws.store)?;
    workspace::snapshot(&mut store, &mut ws, author, Look::Everywhere)?;
  }
  let pid_file = root.join(".nv").join("watch.pid");
  std::fs::write(&pid_file, std::process::id().to_string()).map_err(|e| format!("cannot write {}: {e}", pid_file.display()))?;
  let say = |line: String| {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
  };
  say(format!("watching {}", root.display()));

  let mut paths: BTreeSet<Vec<u8>> = BTreeSet::new();
  let mut asked: Vec<std::path::PathBuf> = Vec::new();
  let take = |paths: &mut BTreeSet<Vec<u8>>, asked: &mut Vec<std::path::PathBuf>, event: notify::Result<notify::Event>| {
    for path in event.map(|e| e.paths).unwrap_or_default() {
      // A file someone made to ask whether every write before it has been recorded.
      if path.parent() == Some(&root.join(".nv")) && path.file_name().is_some_and(|name| name.as_bytes().starts_with(b"asked-")) {
        asked.push(path);
        continue;
      }
      // Not nv's own files, which it writes while recording, nor Git's.
      if let Ok(relative) = path.strip_prefix(&root)
        && relative.components().next().is_some_and(|first| first.as_os_str() != ".nv" && first.as_os_str() != ".git")
      {
        paths.insert(relative.as_os_str().as_bytes().to_vec());
      }
    }
  };
  loop {
    let Ok(first) = events.recv() else { return Ok(()) };
    take(&mut paths, &mut asked, first);
    let started = Instant::now();
    while started.elapsed() < AT_MOST {
      match events.recv_timeout(QUIET) {
        Ok(event) => take(&mut paths, &mut asked, event),
        Err(RecvTimeoutError::Timeout) => break,
        Err(RecvTimeoutError::Disconnected) => return Ok(()),
      }
    }
    if !paths.is_empty() {
      // Someone else may be at the workspace: a hook, a commit, a restore.
      let _lock = workspace::lock(&root)?;
      let mut ws = Workspace::open(&root)?;
      let reported = workspace::record_events(&ws, &std::mem::take(&mut paths))?;
      let mut store = Store::open(&ws.store)?;
      if let Some(id) = workspace::snapshot(&mut store, &mut ws, author, Look::Recorded)? {
        say(format!("{} from {reported} paths", hex(&id)));
      }
    }
    // Events come in the order things happened: every write before these questions is recorded now.
    for question in asked.drain(..) {
      let _ = std::fs::remove_file(question);
    }
  }
}
