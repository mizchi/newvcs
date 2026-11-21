//! What every test that runs a command shares: no command it runs may run for ever. A command
//! still running after `LIMIT` is stopped, with everything it started, and the test fails saying
//! so, instead of the test run hanging.

#![allow(dead_code)]

use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

/// Longer than any command of the tests takes on a loaded machine.
pub const LIMIT: Duration = Duration::from_secs(300);

/// Run a command to its end within `LIMIT`, its output captured.
pub fn bounded(command: &mut Command) -> Output {
  use std::os::unix::process::CommandExt;
  command.process_group(0).stdout(Stdio::piped()).stderr(Stdio::piped());
  command.stdin(Stdio::null());
  finish(command.spawn().expect("the command starts"))
}

/// Wait for a child started with `process_group(0)` and piped output, within `LIMIT`.
pub fn finish(child: Child) -> Output {
  let pid = child.id() as i32;
  let (send, receive) = std::sync::mpsc::channel();
  std::thread::spawn(move || {
    let _ = send.send(child.wait_with_output());
  });
  match receive.recv_timeout(LIMIT) {
    Ok(output) => output.expect("the command's output is read"),
    Err(_) => {
      // SAFETY: a signal to the process group the test's own child leads.
      unsafe { libc::kill(-pid, libc::SIGKILL) };
      panic!("a command of this test was still running after {} s, and was stopped", LIMIT.as_secs());
    }
  }
}
