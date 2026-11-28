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

/// How much of one answer from a server is read (`NV_ANSWER_MB`, 1024 MB): an answer is held
/// whole, and a server that does not stop sending would otherwise take all the memory there is.
pub fn answer_bytes() -> u64 {
  std::env::var("NV_ANSWER_MB").ok().and_then(|text| text.trim().parse::<u64>().ok()).filter(|mb| *mb > 0).unwrap_or(1024) << 20
}

/// How long a commit Git makes for nv may take, its hooks included (`NV_GIT_TIMEOUT`, 300 s).
pub fn git_commit() -> Option<Duration> {
  seconds("NV_GIT_TIMEOUT", 300)
}

/// How long a merge driver may take on one file (`NV_MERGE_DRIVER_TIMEOUT`, 120 s).
pub fn merge_driver() -> Option<Duration> {
  seconds("NV_MERGE_DRIVER_TIMEOUT", 120)
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
  // What the child starts in a session of its own is out of its process group: it comes back to
  // this process when its parent ends, and is stopped with the rest (second pass: a driver's
  // `setsid` child wrote into the workspace after the sync had stopped it).
  adopt_orphans();
  stop_with_this_process();
  let before = children();
  let mut child = command.spawn().map_err(|e| format!("cannot start {what}: {e}"))?;
  let group = child.id() as i32;
  RUNNING.lock().unwrap_or_else(|e| e.into_inner()).push((group, before.clone()));
  let done = (|| {
    let Some(limit) = limit else { return child.wait().map_err(|e| e.to_string()) };
    let started = Instant::now();
    loop {
      if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
        return Ok(status);
      }
      if started.elapsed() >= limit {
        // SAFETY: a signal to the process group the child leads, made for it above.
        unsafe { libc::kill(-group, libc::SIGKILL) };
        let _ = child.wait();
        stop_new_children(&before);
        return Err(format!("{what} did not finish in {} s and was stopped, with what it had started", limit.as_secs()));
      }
      std::thread::sleep(Duration::from_millis(5));
    }
  })();
  RUNNING.lock().unwrap_or_else(|e| e.into_inner()).retain(|(g, _)| *g != group);
  // Stopped meanwhile: nothing goes on from what the child left, while the signal's thread ends
  // the process.
  while STOPPING.load(std::sync::atomic::Ordering::SeqCst) != 0 {
    std::thread::sleep(Duration::from_millis(50));
  }
  done
}

/// The signal that is stopping this process, once one has come.
static STOPPING: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// Where the handler says a signal came, for the thread that acts on it.
static SIGNALLED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_stop(signal: libc::c_int) {
  STOPPING.store(signal, std::sync::atomic::Ordering::SeqCst);
  let fd = SIGNALLED.load(std::sync::atomic::Ordering::SeqCst);
  let byte = signal as u8;
  // SAFETY: write(2) is async-signal-safe, of one byte from the stack to a pipe made for it.
  unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
}

/// Stopped by a signal (SIGTERM from `timeout(1)` or a supervisor, SIGINT from a Ctrl-C, SIGHUP),
/// this process stops what `run` started first, and then itself as the signal would have: a driver
/// is in a process group of its own, which a Ctrl-C does not reach (third pass: a driver went on and
/// wrote into the workspace after nv had been stopped, as Git's never does).
fn stop_with_this_process() {
  static ONCE: std::sync::Once = std::sync::Once::new();
  ONCE.call_once(|| {
    let mut fds = [0; 2];
    // SAFETY: a pipe into an array of two descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
      return;
    }
    SIGNALLED.store(fds[1], std::sync::atomic::Ordering::SeqCst);
    let reading = fds[0];
    std::thread::spawn(move || {
      let mut byte = 0u8;
      // SAFETY: a read of one byte into a byte on this stack, from the pipe made above.
      while unsafe { libc::read(reading, (&mut byte as *mut u8).cast(), 1) } != 1 {}
      let signal = STOPPING.load(std::sync::atomic::Ordering::SeqCst);
      let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).clone();
      for (group, _) in &running {
        // SAFETY: a signal to a process group `run` made for a child it has not yet reaped.
        unsafe { libc::kill(-group, libc::SIGKILL) };
      }
      if let Some((_, before)) = running.first() {
        stop_new_children(before);
      }
      // SAFETY: the signal's own handling put back, and the signal raised again, to end as it would have.
      unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
      }
      std::process::exit(128 + signal);
    });
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
      // SAFETY: a handler that only stores a number and writes a byte, both async-signal-safe.
      unsafe { libc::signal(signal, on_stop as *const () as libc::sighandler_t) };
    }
  });
}

/// Have orphans of this process's descendants come back to it (Linux), so that a limit reached can
/// find them.
fn adopt_orphans() {
  #[cfg(target_os = "linux")]
  {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: a flag on this process, with no pointer passed.
    ONCE.call_once(|| unsafe {
      libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    });
  }
}

/// This process's children now, by the parent each process names in `/proc`.
fn children() -> Vec<i32> {
  let me = std::process::id().to_string();
  let mut out = Vec::new();
  for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
    let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else { continue };
    let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue };
    // The fields after the name, which is in parentheses and may hold anything.
    let Some(after) = stat.rfind(')').map(|at| &stat[at + 1..]) else { continue };
    if after.split_whitespace().nth(1) == Some(me.as_str()) {
      out.push(pid);
    }
  }
  out
}

/// Stop the children this process has that it did not have `before`, and theirs as they come back
/// to it, a few rounds over.
fn stop_new_children(before: &[i32]) {
  for _ in 0..8 {
    let new: Vec<i32> = children().into_iter().filter(|pid| !before.contains(pid)).collect();
    if new.is_empty() {
      return;
    }
    for pid in new {
      // SAFETY: signals to a process this one is the parent of, and its group; then reaping it.
      unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
      }
    }
  }
}

/// The process groups of the children `run` is waiting on: stopped with the command, should the
/// command's own limit stop it (thirteenth round: a merge driver that never returned went on,
/// after the sync that started it was stopped, and wrote into the workspace without its lock).
static RUNNING: std::sync::Mutex<Vec<(i32, Vec<i32>)>> = std::sync::Mutex::new(Vec::new());

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
    let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (group, _) in &running {
      // SAFETY: a signal to a process group `run` made for a child it has not yet reaped.
      unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    // And what they started elsewhere: children this process did not have before the first of them.
    if let Some((_, before)) = running.first() {
      stop_new_children(before);
    }
    std::process::exit(124);
  });
}
