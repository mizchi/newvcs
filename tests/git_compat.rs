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

/// A tag of the name of a branch only origin has: Git checks the tag out with no branch, which nv
/// does not do, and `switch` says a branch is expected (fourth pass: nv made the branch).
#[test]
fn a_tag_named_like_a_branch_of_origins_is_not_that_branch() {
  let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("tag-like-origin");
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  let sh = |script: &str| {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script).current_dir(&dir).env("NV", env!("CARGO_BIN_EXE_nv"));
    for (key, value) in [
      ("GIT_AUTHOR_NAME", "a"),
      ("GIT_AUTHOR_EMAIL", "a@example.com"),
      ("GIT_COMMITTER_NAME", "c"),
      ("GIT_COMMITTER_EMAIL", "c@example.com"),
    ] {
      command.env(key, value);
    }
    common::bounded(&mut command)
  };
  let setup = "git init -q --bare -b main o.git && git clone -q o.git s 2>/dev/null && cd s && echo a > a.txt && git add a.txt && git commit -qm a \
    && git push -q origin main 2>/dev/null && git checkout -q -b dev && echo d > d.txt && git add d.txt && git commit -qm d && git push -q origin dev 2>/dev/null \
    && cd .. && $NV git clone -q o.git w && cd w && $NV git tag dev main";
  assert!(sh(setup).status.success());
  let checkout = sh("cd w && $NV git checkout -q dev");
  assert_eq!(checkout.status.code(), Some(125), "{checkout:?}");
  let switch = sh("cd w && $NV git switch -q dev");
  assert_eq!(switch.status.code(), Some(128), "{switch:?}");
  assert!(String::from_utf8_lossy(&switch.stderr).contains("a branch is expected, got tag 'dev'"), "{switch:?}");
  let after = sh("cd w && $NV git branch --show-current && $NV git branch && ls");
  assert_eq!(String::from_utf8_lossy(&after.stdout), "main\n* main\na.txt\n");
}

/// A `commit -a` refused for a date nv does not read stages nothing, as one Git refuses does not
/// (fifth pass: the files were left staged, and the next commit of others published them).
#[test]
fn a_refused_commit_stages_nothing() {
  let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("refused-commit-a");
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  let sh = |script: &str| {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script).current_dir(&dir).env("NV", env!("CARGO_BIN_EXE_nv"));
    for (key, value) in [
      ("GIT_AUTHOR_NAME", "a"),
      ("GIT_AUTHOR_EMAIL", "a@example.com"),
      ("GIT_COMMITTER_NAME", "c"),
      ("GIT_COMMITTER_EMAIL", "c@example.com"),
    ] {
      command.env(key, value);
    }
    common::bounded(&mut command)
  };
  assert!(sh("$NV git init -q -b main && echo a > a.txt && $NV git add a.txt && $NV git commit -qm a && echo b >> a.txt").status.success());
  let refused = sh("GIT_AUTHOR_DATE='Tue, 14 Nov 2023 22:13:20 +0100' $NV git commit -qam b");
  assert_eq!(refused.status.code(), Some(125), "{refused:?}");
  assert_eq!(String::from_utf8_lossy(&sh("$NV git status --short").stdout), " M a.txt\n");
}
