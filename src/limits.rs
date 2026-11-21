//! How long nv waits for anything before it gives up. Nothing nv does waits without end: a lock
//! another process holds, a server that does not answer, a Git hook that does not return, a
//! command as a whole. Each wait has a limit, from the environment or a default, and when it is
//! reached nv says what it was waiting for and stops.
//!
//! Stopping part-way is safe: every command leaves what was, or what was meant, when it is killed
//! at any moment (see AGENTS.md), and giving up is a kill nv does to itself.

use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use crate::Result;

/// A limit in seconds from the environment, or the default; `0` means no limit.
fn seconds(name: &str, default: u64) -> Option<Duration> {
  let value = std::env::var(name).ok().and_then(|text| text.trim().parse::<u64>().ok()).unwrap_or(default);
  (value > 0).then(|| Duration::from_secs(value))
}

/// How long a command waits for a lock another nv command holds (`NV_LOCK_TIMEOUT`, 60 s).
pub fn lock_wait() -> Option<Duration> {
  seconds("NV_LOCK_TIMEOUT", 60)
}

/// How long a request to a server may go without a byte moving (`NV_HTTP_TIMEOUT`, 60 s), and
/// how long making the connection may take (10 s, or that if it is shorter).
pub fn http_quiet() -> Option<Duration> {
  seconds("NV_HTTP_TIMEOUT", 60)
}

/// How long a commit Git makes for nv may take, its hooks included (`NV_GIT_TIMEOUT`, 300 s).
pub fn git_commit() -> Option<Duration> {
  seconds("NV_GIT_TIMEOUT", 300)
}

/// How long one command may run in all (`NV_TIMEOUT`, 1800 s). A watcher is not one command.
pub fn command() -> Option<Duration> {
  seconds("NV_TIMEOUT", 1800)
}

/// Take an exclusive `flock` on `file`, waiting at most `lock_wait()`. `what` names the lock for
/// the message when the wait is given up.
pub fn lock(file: &std::fs::File, what: &str) -> Result<()> {
  let fd = file.as_raw_fd();
  let Some(limit) = lock_wait() else {
    // SAFETY: the descriptor is open for as long as `file` is.
    return match unsafe { libc::flock(fd, libc::LOCK_EX) } {
      0 => Ok(()),
      _ => Err(format!("cannot lock {what}: {}", std::io::Error::last_os_error())),
    };
  };
  let started = Instant::now();
  let mut pause = Duration::from_millis(1);
  loop {
    // SAFETY: as above.
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
      return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
      return Err(format!("cannot lock {what}: {error}"));
    }
    if started.elapsed() >= limit {
      return Err(format!(
        "{what} has been held by another nv command for {} s, and this one gave up waiting. If nothing of nv's is running, a process that hangs holds it: find it with `lsof` on the lock file. (NV_LOCK_TIMEOUT sets how long to wait; 0 waits for ever.)",
        limit.as_secs()
      ));
    }
    std::thread::sleep(pause);
    pause = (pause * 2).min(Duration::from_millis(50));
  }
}

/// Run a child to its end, at most `limit`; past it, the child and everything it started (its
/// process group) is stopped and an error says so. The child is put in a process group of its
/// own for that, so it must not need the terminal's.
pub fn run(command: &mut std::process::Command, limit: Option<Duration>, what: &str) -> Result<std::process::ExitStatus> {
  use std::os::unix::process::CommandExt;
  command.process_group(0);
  let mut child = command.spawn().map_err(|e| format!("cannot start {what}: {e}"))?;
  let Some(limit) = limit else { return child.wait().map_err(|e| e.to_string()) };
  let started = Instant::now();
  loop {
    if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
      return Ok(status);
    }
    if started.elapsed() >= limit {
      // SAFETY: a signal to the process group the child leads, made for it above.
      unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
      let _ = child.wait();
      return Err(format!("{what} did not finish in {} s and was stopped, with what it had started", limit.as_secs()));
    }
    std::thread::sleep(Duration::from_millis(5));
  }
}

/// Give the whole command `command()` to run, then stop it with a message and exit code 124, as
/// `timeout(1)` does. Called once, at the start of a command.
pub fn watch_the_clock(what: &str) {
  let Some(limit) = command() else { return };
  let what = what.to_string();
  std::thread::spawn(move || {
    std::thread::sleep(limit);
    eprintln!(
      "nv: `{what}` was still running after {} s and was stopped. What it had done is kept as far as it went, as for any command stopped part-way. (NV_TIMEOUT sets the limit; 0 is none.)",
      limit.as_secs()
    );
    std::process::exit(124);
  });
}
