//! Checkpoints: the states of a workspace that passed the acceptance conditions `nv.toml`
//! declares, the commit message a hook drafts for them, and a commit that is not made of a
//! state the conditions did not pass.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NV: &str = env!("CARGO_BIN_EXE_nv");

fn sh(dir: &Path, script: &str) -> Output {
  let mut c = Command::new("sh");
  c.arg("-c").arg(script).current_dir(dir).env("NV", NV);
  c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null");
  for (key, value) in
    [("AUTHOR_NAME", "A"), ("AUTHOR_EMAIL", "a@example.com"), ("COMMITTER_NAME", "C"), ("COMMITTER_EMAIL", "c@example.com")]
  {
    c.env(format!("GIT_{key}"), value);
  }
  common::bounded(&mut c)
}

fn text(out: &Output) -> String {
  format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn ok(dir: &Path, script: &str) -> String {
  let out = sh(dir, script);
  assert!(out.status.success(), "`{script}` failed:\n{}", text(&out));
  String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn code_of(dir: &Path, script: &str) -> (i32, String) {
  let out = sh(dir, script);
  (out.status.code().unwrap_or(-1), text(&out))
}

/// A workspace with one commit, and the conditions given.
fn workspace(name: &str, conditions: &str) -> PathBuf {
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checks-{name}"));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  std::fs::write(dir.join("nv.toml"), conditions).unwrap();
  ok(&dir, "$NV git init -q -b main . && echo a > a.txt && $NV git add -A && $NV git commit -q -m base");
  dir
}

const PIPELINE: &str = r#"
[checks.unit]
run = "test -f ok"

[checks.later]
run = "grep -q good a.txt"
after = ["unit"]

[message]
run = "echo \"touches $(grep -c '^diff --git' | tr -d ' ') files\""
"#;

#[test]
fn a_state_is_a_checkpoint_as_far_as_its_conditions_pass() {
  let dir = workspace("pipeline", PIPELINE);
  // Nothing passes: no checkpoint, and the one after the failed one is not run.
  let (code, said) = code_of(&dir, "$NV checkpoint . 2>&1");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("unit") && said.contains("failed") && said.contains("later") && said.contains("not run"), "{said}");
  assert_eq!(code_of(&dir, "$NV since . checkpoint-unit 2>&1").0, 2, "no checkpoint was made");
  // The first passes: that state is a checkpoint of it, and the second is run and fails.
  ok(&dir, "touch ok");
  let (code, said) = code_of(&dir, "$NV checkpoint . 2>&1");
  assert_eq!(code, 1, "{said}");
  assert_eq!(ok(&dir, "$NV since . checkpoint-unit"), "", "the state that passed is the checkpoint");
  assert_eq!(code_of(&dir, "$NV since . checkpoint-later 2>&1").0, 2);
  // All of them pass: every checkpoint, and a message drafted for the state.
  ok(&dir, "echo good >> a.txt");
  let said = ok(&dir, "$NV checkpoint . 2>&1");
  assert!(said.contains("passed"), "{said}");
  assert_eq!(ok(&dir, "$NV since . checkpoint-later; $NV since . checkpoint-unit"), "");
  // Committed with no message: the drafted one.
  ok(&dir, "$NV commit . >/dev/null");
  assert_eq!(ok(&dir, "$NV git log -1 --format=%s"), "touches 2 files");
  // An explicit message is the one used.
  ok(&dir, "echo good again >> a.txt && $NV commit . 'said so' >/dev/null");
  assert_eq!(ok(&dir, "$NV git log -1 --format=%s"), "said so");
}

#[test]
fn a_commit_is_not_made_of_a_state_its_conditions_do_not_pass() {
  let dir = workspace("gate", PIPELINE);
  ok(&dir, "touch ok && echo bad >> a.txt");
  let (code, said) = code_of(&dir, "$NV commit . 'not yet' 2>&1");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("later"), "{said}");
  assert_eq!(ok(&dir, "$NV git log -1 --format=%s"), "base");
  // Asked for in so many words, it is made all the same.
  ok(&dir, "$NV commit . 'anyway' --no-checks >/dev/null");
  assert_eq!(ok(&dir, "$NV git log -1 --format=%s"), "anyway");
  // With no conditions declared, a commit is a commit, as before.
  let plain = workspace("plain", "");
  ok(&plain, "echo b >> a.txt && $NV commit . plain >/dev/null");
  let (code, said) = code_of(&plain, "echo c >> a.txt && $NV commit . 2>&1");
  assert_eq!(code, 2, "with no message and no hook to draft one: {said}");
}

#[test]
fn a_run_the_files_changed_under_is_no_checkpoint_and_a_check_that_hangs_is_stopped() {
  let dir = workspace(
    "moving",
    r#"
[checks.writes]
run = "echo more >> a.txt"

[checks.slow]
run = "sleep 30"
timeout = 1
"#,
  );
  let started = std::time::Instant::now();
  let (code, said) = code_of(&dir, "$NV checkpoint . 2>&1");
  assert!(started.elapsed().as_secs() < 20, "{said}");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("changed while"), "{said}");
  assert!(said.contains("slow") && said.contains("did not finish"), "{said}");
  assert_eq!(code_of(&dir, "$NV since . checkpoint-writes 2>&1").0, 2, "no checkpoint of a state the files are no longer in");
  // Conditions that wait on each other in a circle are refused, saying which.
  let circle = workspace("circle", "[checks.a]\nrun = \"true\"\nafter = [\"b\"]\n[checks.b]\nrun = \"true\"\nafter = [\"a\"]\n");
  let (code, said) = code_of(&circle, "$NV checkpoint . 2>&1");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("circle"), "{said}");
  let unknown = workspace("unknown", "[checks.a]\nrun = \"true\"\nafter = [\"nope\"]\n");
  assert!(code_of(&unknown, "$NV checkpoint . 2>&1").1.contains("nope"));
}

/// A reviewer's finding (twelfth round): the conditions changed while a runner ran, and its next
/// round committed the new state, which declares that nothing passes, as passing the old ones.
#[test]
fn each_run_is_of_the_conditions_its_state_declares() {
  let dir = workspace("declared", "[checks.unit]\nrun = \"sleep 1; true\"\n\n[commit]\neach = true\n");
  ok(
    &dir,
    "echo b > b.txt && { $NV checkpoint . --until-still >runner.out 2>&1 & pid=$!; } && sleep 0.4 \
     && printf '[checks.unit]\\nrun = \"false\"\\n\\n[commit]\\neach = true\\n' > nv.toml && echo c > c.txt \
     && wait $pid",
  );
  // Whatever was committed declares the conditions it passed: none says `false`.
  let committed = ok(&dir, "for c in $($NV git log --format=%h); do $NV git show $c:nv.toml; done");
  assert!(!committed.contains("false"), "a state that declares `false` was committed as passing:\n{committed}");
  assert_eq!(code_of(&dir, "$NV checkpoint . 2>&1").0, 1);
}

#[test]
fn checkpoints_are_found_in_the_background_as_work_is_recorded() {
  let dir = workspace("auto", "auto = true\n\n[checks.unit]\nrun = \"test -f ok\"\n");
  // A tool writes, and its hook records the write: the conditions are run behind it.
  let event = format!(r#"{{"tool_name":"Write","cwd":"{0}","tool_input":{{"file_path":"{0}/ok"}}}}"#, dir.display());
  ok(&dir, &format!("touch ok && printf '%s' '{event}' | $NV hook claude"));
  let mut found = false;
  for _ in 0..100 {
    if code_of(&dir, "$NV since . checkpoint-unit 2>/dev/null").0 == 0 {
      found = true;
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
  }
  assert!(found, "no checkpoint came of the write: {}", ok(&dir, "cat .nv/checks/runner.log 2>/dev/null; true"));
  assert_eq!(ok(&dir, "$NV since . checkpoint-unit"), "");
}
