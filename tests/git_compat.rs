//! Scripts written for Git, run under Git and under `nv git`, compared.
//! The cases, the list of them and the list of commands that must be refused
//! are plain files; `tools/git-compat.sh` is the runner and prints the table.

mod common;

use std::process::Command;

#[test]
fn nv_speaks_the_everyday_git_commands_and_refuses_the_rest() {
  let out = Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tools/git-compat.sh"))
    .arg(env!("CARGO_BIN_EXE_nv"))
    .output()
    .expect("the runner starts");
  let report = String::from_utf8_lossy(&out.stdout);
  assert!(out.status.success(), "{report}{}", String::from_utf8_lossy(&out.stderr));
  // Every listed case ran and agreed, and every listed command was refused.
  let listed = |file: &str| {
    std::fs::read_to_string(format!("{}/tools/{file}", env!("CARGO_MANIFEST_DIR")))
      .unwrap()
      .lines()
      .filter(|l| !l.is_empty() && !l.starts_with('#'))
      .count()
  };
  let reads = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/git-compat/reads")).unwrap().count();
  assert_eq!(report.lines().filter(|l| l.starts_with("ok ")).count(), listed("git-compat-allowlist.txt") + reads, "{report}");
  assert!(report.contains(&format!("refused  {} commands", listed("git-compat-unsupported.txt"))), "{report}");
}

/// `git fsck` says nothing about a sound repository; this is the other half.
#[test]
fn fsck_finds_an_object_that_is_not_what_its_name_says() {
  let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fsck");
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  let git = |args: &[&str]| {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nv"));
    command.arg("git").args(args).current_dir(&dir).env("GIT_AUTHOR_NAME", "a").env("GIT_AUTHOR_EMAIL", "a@example.com");
    command.env("GIT_COMMITTER_NAME", "c").env("GIT_COMMITTER_EMAIL", "c@example.com");
    common::bounded(&mut command)
  };
  assert!(git(&["init", "-q", "-b", "main"]).status.success());
  let text: String = (0..400u32).map(|i| format!("line {i} of a file\n")).collect();
  std::fs::write(dir.join("file.txt"), &text).unwrap();
  assert!(git(&["add", "-A"]).status.success());
  assert!(git(&["commit", "-q", "-m", "one"]).status.success());
  let sound = git(&["fsck"]);
  assert!(sound.status.success() && sound.stdout.is_empty() && sound.stderr.is_empty(), "{sound:?}");

  // One byte changed on disk, wherever it falls: in an object's content it
  // no longer hashes to its name, in a record's header the objects after it are lost.
  let segment = dir.join(".nv/store/objects.seg");
  let original = std::fs::read(&segment).unwrap();
  for at in [original.len() / 4, original.len() / 2, original.len() - 3] {
    let mut bytes = original.clone();
    bytes[at] ^= 1;
    std::fs::write(&segment, &bytes).unwrap();
    let damaged = git(&["fsck"]);
    assert!(!damaged.status.success(), "a flipped byte at {at} went unnoticed");
    assert!(String::from_utf8_lossy(&damaged.stderr).contains("error: "), "{damaged:?}");
    // Looking did not make it worse: the file is as it was damaged, and reading commands leave it so.
    git(&["status", "--porcelain"]);
    git(&["log", "--oneline"]);
    assert_eq!(std::fs::read(&segment).unwrap(), bytes, "a read-only command changed a damaged store");
  }
  // Put right again, it is sound again.
  std::fs::write(&segment, &original).unwrap();
  assert!(git(&["fsck"]).status.success());
}
