//! Git's merge drivers: a path whose `.gitattributes` says `merge=<name>` is merged by the
//! command the configuration gives as `merge.<name>.driver`, as Git merges it. The command is
//! run by the shell in the workspace, given the ancestor's version, ours and theirs as files
//! (`%O`, `%A`, `%B`), the size of conflict markers (`%L`), the path (`%P`) and the names of the
//! three sides (`%S`, `%X`, `%Y`); it leaves the result in `%A`, and exits 0 where that is clean.
//! A name the configuration does not define is the text merge, as with Git.
//!
//! The configuration is the workspace's own (`nv git config merge.<name>.driver <command>`),
//! then Git's beside it and the user's: never the repository's files, so that what a merge runs
//! is what whoever merges chose to run. The server runs none: a path a driver merges is a path
//! whose push is refused when the ref changed it too, and the sync that follows runs the driver.

use crate::Result;
use crate::union::Merge;
use std::path::Path;

/// The command a driver's name stands for in this workspace's configuration, if any.
pub fn command(root: &Path, name: &str) -> Option<String> {
  let key = format!("merge.{name}.driver");
  let own = std::fs::read_to_string(root.join(".nv").join("git-config")).unwrap_or_default();
  own
    .lines()
    .rev()
    .find_map(|l| l.strip_prefix(key.as_str())?.strip_prefix('=').map(str::to_string))
    .or_else(|| crate::git::config_value(root, &format!("merge \"{name}\""), "driver"))
}

/// How `.gitattributes` in the workspace says a path is merged.
pub fn merge_of(root: &Path, path: &[u8]) -> Merge {
  crate::union::merge_of(|dir| std::fs::read(root.join(crate::workspace::to_path(dir)).join(".gitattributes")).ok(), path)
}

/// The three-way merge of a file, as Git's attributes and configuration ask for it: by the
/// driver the path names where the configuration defines it, else as text (`union` where it says
/// so). `false` where the result is not clean: it then holds what the driver left, or both
/// versions between markers.
pub fn merge_file(root: &Path, path: &[u8], base: &[u8], ours: &[u8], theirs: &[u8], names: [&str; 3]) -> Result<(Vec<u8>, bool)> {
  let merge = merge_of(root, path);
  if let Merge::Driver(name) = &merge
    && let Some(command) = command(root, name)
  {
    return run(root, &command, path, base, ours, theirs, names);
  }
  Ok(crate::union::merge_text(base, ours, theirs, names[1], names[2], merge == Merge::Union))
}

/// `merge_file` for a sync, where a file is unsettled for as long as it holds markers (there is
/// no index to say so): a driver that says it could not merge and leaves no markers is not
/// believed to have left anything to settle by, and the file is merged as text instead, both
/// versions between markers. The names are the sync's, and a driver that writes markers of its
/// own is given them (`%X`, `%Y`) to write.
pub fn merge_file_marked(root: &Path, path: &[u8], base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<(Vec<u8>, bool)> {
  let names = ["base", "yours", "theirs"];
  let (text, clean) = match merge_file(root, path, base, ours, theirs, names) {
    // A driver killed by a signal said nothing of the file: merged as text, as one that failed.
    Err(why) if why.contains(KILLED) => (Vec::new(), false),
    other => other?,
  };
  let marked = text.split(|&b| b == b'\n').any(|line| line.starts_with(b"<<<<<<< yours"));
  // Markers are what a sync leaves to settle by, whatever the driver said of them (thirteenth
  // round: a driver that left them and exited 0 had them committed, and the push refused).
  if marked {
    return Ok((text, false));
  }
  if clean {
    return Ok((text, true));
  }
  let union = merge_of(root, path) == Merge::Union;
  Ok(crate::union::merge_text(base, ours, theirs, names[1], names[2], union))
}

/// What the error of a driver killed by a signal says, for a sync to know it by.
const KILLED: &str = " died of signal ";

/// A value as Git puts it on a driver's command line: in single quotes, a quote as `'\''`.
fn quoted(value: &str) -> String {
  format!("'{}'", value.replace('\'', "'\\''"))
}

fn run(root: &Path, command: &str, path: &[u8], base: &[u8], ours: &[u8], theirs: &[u8], names: [&str; 3]) -> Result<(Vec<u8>, bool)> {
  use std::sync::atomic::{AtomicUsize, Ordering};
  static SEQ: AtomicUsize = AtomicUsize::new(0);
  // The three versions, as files in the workspace's own directory, named relative to the
  // workspace, where the command runs: what a driver is handed is nothing of the tree's.
  let seq = SEQ.fetch_add(1, Ordering::Relaxed);
  let dir = format!(".nv/merge-{}-{seq}", std::process::id());
  // What a command stopped part-way left of its own, where only one command at a time runs in a
  // workspace: gone before anything else (thirteenth round: left there for good).
  if seq == 0 {
    let own = format!("merge-{}-", std::process::id());
    for entry in std::fs::read_dir(root.join(".nv")).into_iter().flatten().flatten() {
      let name = entry.file_name().to_string_lossy().into_owned();
      if name.starts_with("merge-") && !name.starts_with(&own) {
        let _ = std::fs::remove_dir_all(entry.path());
      }
    }
  }
  let at = root.join(&dir);
  std::fs::create_dir_all(&at).map_err(|e| format!("cannot make {}: {e}", at.display()))?;
  let done = (|| -> Result<(Vec<u8>, bool)> {
    let (o, a, b) = (format!("{dir}/O"), format!("{dir}/A"), format!("{dir}/B"));
    for (file, bytes) in [(&o, base), (&a, ours), (&b, theirs)] {
      std::fs::write(root.join(file), bytes).map_err(|e| format!("cannot write {file}: {e}"))?;
    }
    let path_text = String::from_utf8_lossy(path).into_owned();
    let mut line = String::new();
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
      if c != '%' {
        line.push(c);
        continue;
      }
      match chars.next() {
        Some('O') => line.push_str(&quoted(&o)),
        Some('A') => line.push_str(&quoted(&a)),
        Some('B') => line.push_str(&quoted(&b)),
        Some('L') => line.push('7'),
        Some('P') => line.push_str(&quoted(&path_text)),
        Some('S') => line.push_str(&quoted(names[0])),
        Some('X') => line.push_str(&quoted(names[1])),
        Some('Y') => line.push_str(&quoted(names[2])),
        Some('%') => line.push('%'),
        Some(other) => {
          line.push('%');
          line.push(other);
        }
        None => line.push('%'),
      }
    }
    let mut child = std::process::Command::new("sh");
    child.arg("-c").arg(&line).current_dir(root).stdin(std::process::Stdio::null());
    let what = format!("the merge driver for {path_text} (`{command}`)");
    let status = crate::limits::run(&mut child, crate::limits::merge_driver(), &what)?;
    // Killed by a signal, it said nothing of the file: what it left in %A is not a result, and
    // Git takes the merge as failed (thirteenth round: a half-written %A was taken as the conflict).
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
      return Err(format!("{what}{KILLED}{signal}: nothing of the merge was taken from it"));
    }
    let result = std::fs::read(root.join(&a)).map_err(|e| format!("{what} left no result: {e}"))?;
    Ok((result, status.success()))
  })();
  let _ = std::fs::remove_dir_all(&at);
  done
}
