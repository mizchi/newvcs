//! The commands a writer that changes files all day leans on: going back to
//! a recorded state, naming one, being told of writes by a hook, and
//! publishing into a Git repository in the same directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const NV: &str = env!("CARGO_BIN_EXE_nv");

fn sh_with(dir: &Path, script: &str, stdin: &str) -> Output {
  let mut c = Command::new("sh");
  c.arg("-c").arg(script).current_dir(dir).env("NV", NV).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
  c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null").env_remove("NV_WRITING").env_remove("NV_GIT");
  for (key, value) in
    [("AUTHOR_NAME", "A"), ("AUTHOR_EMAIL", "a@example.com"), ("COMMITTER_NAME", "C"), ("COMMITTER_EMAIL", "c@example.com")]
  {
    c.env(format!("GIT_{key}"), value);
  }
  let mut child = c.spawn().unwrap();
  std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
  child.wait_with_output().unwrap()
}

fn text(out: &Output) -> String {
  format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn ok(dir: &Path, script: &str) -> String {
  let out = sh_with(dir, script, "");
  assert!(out.status.success(), "`{script}` failed:\n{}", text(&out));
  String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn fresh(name: &str) -> PathBuf {
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("agent-{name}"));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

/// A workspace with one published commit: `a.txt`, `b.txt`, `d/x.txt`.
fn workspace(name: &str) -> PathBuf {
  let dir = fresh(name);
  ok(
    &dir,
    "$NV git init -q -b main . && echo a1 > a.txt && echo b1 > b.txt && mkdir d && echo x1 > d/x.txt && $NV git add -A && $NV git commit -q -m base",
  );
  dir
}

/// The id a command printed first.
fn id_of(output: &str) -> String {
  output
    .split_whitespace()
    .find(|word| word.len() == 64 && word.bytes().all(|b| b.is_ascii_hexdigit()))
    .unwrap_or_else(|| panic!("no id in: {output}"))
    .to_string()
}

fn files(dir: &Path) -> String {
  ok(dir, "for f in $(find . -type f -not -path './.nv/*' -not -path './.git/*' | sort); do printf '%s=%s ' \"$f\" \"$(cat \"$f\")\"; done")
}

#[test]
fn a_recorded_state_can_be_gone_back_to_and_nothing_is_lost_by_going() {
  let dir = workspace("restore");
  let first = id_of(&ok(&dir, "echo a2 > a.txt && $NV snapshot ."));
  let second =
    id_of(&ok(&dir, "echo a3 > a.txt && rm b.txt && echo c1 > c.txt && mkdir -p new/deep && echo n > new/deep/n.txt && $NV snapshot ."));
  let at_second = files(&dir);
  // Something not yet recorded, and something nv is told not to see.
  ok(&dir, "echo a4 > a.txt && echo scratch > notes.txt && echo 'out/' > .nv/exclude && mkdir out && echo built > out/bin");
  let restored = ok(&dir, &format!("$NV restore . {first}"));
  assert_eq!(
    files(&dir),
    "./a.txt=a2 ./b.txt=b1 ./d/x.txt=x1 ./out/bin=built",
    "the files of the first snapshot, and what nv does not track left alone"
  );
  assert!(!dir.join("new").exists(), "directories the restore emptied are gone");
  // What was on disk and unrecorded was recorded first: it can be gone back to as well.
  let saved = id_of(restored.split("kept as").nth(1).expect("the restore says where the unrecorded state went"));
  ok(&dir, &format!("$NV restore . {saved}"));
  assert_eq!(files(&dir), "./a.txt=a4 ./c.txt=c1 ./d/x.txt=x1 ./new/deep/n.txt=n ./notes.txt=scratch ./out/bin=built");
  // By a prefix of the id, and by how many recorded states ago.
  ok(&dir, &format!("$NV restore . {}", &second[..10]));
  assert_eq!(files(&dir).replace(" ./out/bin=built", ""), at_second);
  ok(&dir, "$NV restore . base");
  assert_eq!(files(&dir), "./a.txt=a1 ./b.txt=b1 ./d/x.txt=x1 ./out/bin=built");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "", "the workspace is on the state it was restored to");
  // Going back does not publish anything, and what is published afterwards is what is on disk.
  ok(&dir, &format!("$NV restore . {first} && $NV commit . 'only the first change'"));
  assert_eq!(ok(&dir, "$NV git log --format=%s"), "only the first change\nbase");
  assert_eq!(ok(&dir, "$NV git diff --name-status HEAD~1 HEAD"), "M\ta.txt");
  let unknown = sh_with(&dir, "$NV restore . nosuchstate", "");
  assert!(!unknown.status.success() && text(&unknown).contains("nosuchstate"));
}

#[test]
fn a_state_can_be_named_and_the_name_gone_back_to() {
  let dir = workspace("mark");
  ok(&dir, "echo a2 > a.txt && echo c1 > c.txt && $NV mark . green");
  ok(&dir, "echo broken > a.txt && rm d/x.txt && echo e > e.txt && $NV snapshot . && echo worse > b.txt");
  // What has changed since, recorded or not.
  assert_eq!(ok(&dir, "$NV since . green"), "M a.txt\nM b.txt\nD d/x.txt\nA e.txt");
  ok(&dir, "$NV restore . green");
  assert_eq!(files(&dir), "./a.txt=a2 ./b.txt=b1 ./c.txt=c1 ./d/x.txt=x1");
  assert_eq!(ok(&dir, "$NV since . green"), "");
  // A name moves when it is given again.
  ok(&dir, "echo a5 > a.txt && $NV mark . green && echo a6 > a.txt && $NV restore . green");
  assert_eq!(ok(&dir, "cat a.txt"), "a5");
  assert!(!sh_with(&dir, "$NV mark . 'not a name'", "").status.success());
  // A writer that reports its writes does all of this without the tree being walked.
  ok(&dir, "echo a7 > a.txt && $NV record . a.txt && $NV mark . green --recorded");
  ok(&dir, "echo a8 > a.txt && echo b8 > b.txt && $NV record . a.txt b.txt && $NV snapshot . --recorded && echo unreported > a.txt");
  assert_eq!(ok(&dir, "$NV record . a.txt && $NV since . green --recorded"), "M a.txt\nM b.txt");
  let restored = ok(&dir, "$NV restore . green --recorded");
  assert_eq!(ok(&dir, "cat a.txt b.txt"), "a7\nb1");
  // A file about to be overwritten is looked at even if nobody reported it: its content is kept.
  ok(&dir, &format!("$NV restore . {} --recorded", id_of(restored.split("kept as").nth(1).expect("kept"))));
  assert_eq!(ok(&dir, "cat a.txt b.txt"), "unreported\nb8");
}

#[test]
fn a_hook_records_what_a_tool_wrote_and_never_gets_in_the_way() {
  let dir = workspace("hook");
  let full = std::fs::canonicalize(&dir).unwrap();
  let before = ok(&dir, "$NV log . | wc -l").trim().to_string();
  // A tool that says which file it wrote: only that file is looked at.
  std::fs::write(dir.join("a.txt"), "by a tool\n").unwrap();
  std::fs::write(dir.join("b.txt"), "by something that did not say\n").unwrap();
  let event = format!(
    r#"{{"hook_event_name":"PostToolUse","tool_name":"Edit","cwd":"{0}/d","tool_input":{{"file_path":"{0}/a.txt"}}}}"#,
    full.display()
  );
  let out = sh_with(&dir, "$NV hook claude", &event);
  assert!(out.status.success() && text(&out).is_empty(), "a hook says nothing when all is well: {}", text(&out));
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "M b.txt", "the reported file is recorded; the other is still a difference");
  // A tool that does not say: everything is looked at.
  let event =
    format!(r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","cwd":"{}","tool_input":{{"command":"make"}}}}"#, full.display());
  assert!(sh_with(&dir, "$NV hook claude", &event).status.success());
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "");
  assert_eq!(ok(&dir, "$NV log . | wc -l").trim().parse::<usize>().unwrap(), before.parse::<usize>().unwrap() + 2);
  // Outside a workspace, or handed nonsense, it does nothing and does not fail the tool.
  let outside = fresh("hook-outside");
  let event = format!(
    r#"{{"tool_name":"Write","cwd":"{0}","tool_input":{{"file_path":"{0}/z.txt"}}}}"#,
    std::fs::canonicalize(&outside).unwrap().display()
  );
  for (at, stdin) in [(&outside, event.as_str()), (&dir, "not json"), (&dir, "{}"), (&dir, "")] {
    let out = sh_with(at, "$NV hook claude", stdin);
    assert!(out.status.success() && out.stdout.is_empty(), "{stdin:?}: {}", text(&out));
  }
  // The settings to paste say to run exactly this.
  let settings = ok(&dir, "$NV agent-init claude");
  let parsed: serde_json::Value = serde_json::from_str(&settings).expect("valid JSON");
  let hooks = parsed["hooks"]["PostToolUse"].as_array().expect("PostToolUse hooks");
  assert!(hooks.iter().all(|h| h["hooks"][0]["command"] == "nv hook claude" && h["hooks"][0]["type"] == "command"));
  assert!(hooks.iter().any(|h| h["matcher"].as_str().is_some_and(|m| m.contains("Edit") && m.contains("Write"))));
  assert!(hooks.iter().any(|h| h["matcher"].as_str().is_some_and(|m| m.contains("Bash"))));
}

#[test]
fn snapshots_stay_private_and_a_commit_reaches_git_in_the_same_directory() {
  let dir = fresh("publish");
  ok(&dir, "git init -q -b main . && echo a1 > a.txt && echo b1 > b.txt && git add -A && git commit -q -m base && $NV adopt .");
  let git_before = ok(&dir, "git rev-parse HEAD");
  ok(&dir, "echo a2 > a.txt && $NV snapshot . && echo c1 > c.txt && $NV snapshot . && $NV mark . green");
  // Nothing of that is Git's business, or the business of nv's own Git commands.
  assert_eq!(ok(&dir, "git rev-parse HEAD"), git_before);
  assert_eq!(ok(&dir, "$NV check ."), "in step");
  assert_eq!(ok(&dir, "$NV git rev-parse HEAD"), git_before);
  assert_eq!(ok(&dir, "$NV git status --porcelain"), "M a.txt\n?? c.txt", "(the leading space of ` M` is trimmed by the helper)");
  assert_eq!(ok(&dir, "$NV git log --format=%s"), "base");
  // The recorded states are still there to go back to.
  ok(&dir, "echo broken > a.txt && $NV restore . green");
  assert_eq!(ok(&dir, "cat a.txt c.txt"), "a2\nc1");
  // Publishing makes one commit, on Git's branch too, at once.
  ok(&dir, "$NV commit . 'two changes, published'");
  assert_eq!(ok(&dir, "git log --format='%s|%P'"), format!("two changes, published|{git_before}\nbase|"));
  assert_eq!(ok(&dir, "git rev-parse HEAD"), ok(&dir, "$NV git rev-parse HEAD"));
  assert_eq!(ok(&dir, "git status --porcelain"), "");
  assert_eq!(ok(&dir, "$NV check ."), "in step");
  ok(&dir, "git fsck --strict");
  // And where Git moved on its own, publishing waits for the two to be reconciled.
  ok(&dir, "echo raw > b.txt && git -c core.hooksPath=/dev/null commit -q -am 'raw git'");
  let refused = sh_with(&dir, "echo a9 > a.txt && $NV commit . 'on a head Git left'", "");
  assert!(!refused.status.success() && text(&refused).contains("nv reconcile"), "{}", text(&refused));
  ok(&dir, "$NV reconcile . && $NV commit . 'after reconciling'");
  assert_eq!(ok(&dir, "git log --format=%s"), "after reconciling\nraw git\ntwo changes, published\nbase");
}

#[test]
fn reported_writes_are_published_without_a_walk_and_ignored_ones_are_not_recorded() {
  let dir = workspace("recorded");
  ok(&dir, "echo 'out/' > .nv/exclude && mkdir out && echo built > out/bin && echo a2 > a.txt && echo unreported > b.txt");
  // Reporting a path the rules ignore records nothing.
  assert!(ok(&dir, "$NV record . out/bin && $NV snapshot . --recorded").contains("nothing changed"));
  ok(&dir, "$NV record . a.txt && $NV commit . 'what was reported' --recorded");
  assert_eq!(ok(&dir, "$NV git diff --name-status HEAD~1 HEAD"), "M\ta.txt");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "M b.txt", "what nobody reported is still a difference, and still on disk");
}

#[test]
fn many_writers_at_once_leave_one_consistent_workspace() {
  let dir = workspace("at-once");
  let full = std::fs::canonicalize(&dir).unwrap();
  // Twenty hooks, a few snapshots and marks, all at the same moment.
  let mut script = String::new();
  for i in 0..20 {
    let event = format!(r#"{{"tool_name":"Write","cwd":"{0}","tool_input":{{"file_path":"{0}/f{i}.txt"}}}}"#, full.display());
    script.push_str(&format!("(echo {i} > f{i}.txt; echo '{event}' | $NV hook claude) &\n"));
    if i % 5 == 0 {
      script.push_str(&format!("$NV snapshot . >/dev/null & $NV mark . m{i} >/dev/null &\n"));
    }
  }
  script.push_str("wait\n$NV snapshot . >/dev/null");
  ok(&dir, &script);
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "");
  assert_eq!(ok(&dir, "$NV since . base | wc -l").trim(), "20");
  ok(&dir, "$NV git fsck");
  for i in 0..20 {
    assert_eq!(std::fs::read_to_string(dir.join(format!("f{i}.txt"))).unwrap().trim(), i.to_string());
  }
  // Every state named along the way can still be gone back to.
  ok(&dir, "$NV restore . m0 && $NV restore . m15 && $NV restore . '~0'");
}

#[test]
fn a_watcher_reports_what_no_tool_did() {
  let dir = workspace("watch");
  ok(&dir, "echo 'out/' > .nv/exclude && $NV mark . start");
  let mut watcher = Command::new(NV).args(["watch", "."]).current_dir(&dir).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
  let mut ready = String::new();
  // Kept open to the end: a watcher whose output nobody reads any more is stopped by its next line.
  let mut said = std::io::BufReader::new(watcher.stdout.take().unwrap());
  std::io::BufRead::read_line(&mut said, &mut ready).unwrap();
  assert!(ready.contains("watching"), "{ready}");
  // A shell at work: an edit, a new file in a new directory, a removal, a directory renamed, build output.
  ok(
    &dir,
    "echo a2 > a.txt && mkdir -p new/deep && echo n > new/deep/n.txt && rm b.txt && mv d moved && mkdir out && echo built > out/bin",
  );
  // Asked at once, and without walking the tree: the command waits for the watcher to have caught up.
  let wanted = "M a.txt\nD b.txt\nD d/x.txt\nA moved/x.txt\nA new/deep/n.txt";
  let seen = ok(&dir, "$NV since . start --recorded");
  assert_eq!(seen, wanted);
  // And again straight after another write.
  assert_eq!(ok(&dir, "echo late > late.txt && $NV since . start --recorded | grep -c late.txt"), "1");
  ok(&dir, "rm late.txt && $NV since . start --recorded >/dev/null");
  // The watcher recorded all of it: a walk of the tree finds nothing left over.
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "");
  ok(&dir, "$NV restore . start --recorded");
  assert_eq!(files(&dir), "./a.txt=a1 ./b.txt=b1 ./d/x.txt=x1 ./out/bin=built");
  watcher.kill().unwrap();
  watcher.wait().unwrap();
  drop(said);
  // With the watcher gone, nobody is waited for.
  ok(&dir, "$NV since . start --recorded");
}

#[test]
fn a_watcher_that_stopped_is_noticed_and_what_it_missed_is_found() {
  let dir = workspace("watch-down");
  let full = std::fs::canonicalize(&dir).unwrap();
  ok(&dir, "$NV mark . start");
  assert!(ok(&dir, "$NV watch . --status").contains("not watched"));
  // Started, it stays after the command that started it returns.
  ok(&dir, "$NV watch . --start");
  let status = ok(&dir, "$NV watch . --status");
  assert!(status.contains("watched by"), "{status}");
  assert!(text(&sh_with(&dir, "$NV watch . --start", "")).contains("already"));
  assert_eq!(ok(&dir, "echo a2 > a.txt && $NV since . start --recorded"), "M a.txt");
  // It is killed, and something is written while nobody is watching.
  ok(&dir, "kill -9 $(cat .nv/watch.pid) && sleep 0.2 && echo while-down > down.txt");
  let check = sh_with(&dir, "$NV check .", "");
  assert_eq!(check.status.code(), Some(1));
  assert!(text(&check).contains("watcher is not running"), "{}", text(&check));
  // A command that trusts reported writes does not go on trusting: the watcher is started again, and looks at everything once.
  assert_eq!(ok(&dir, "$NV since . start --recorded"), "M a.txt\nA down.txt");
  assert!(ok(&dir, "$NV watch . --status").contains("watched by"));
  assert!(ok(&dir, "$NV check .").contains("watched by"), "check says how the workspace is watched too");
  // A hook starts it again too.
  ok(&dir, "kill -9 $(cat .nv/watch.pid) && sleep 0.2 && echo again > again.txt");
  let event = format!(r#"{{"tool_name":"Write","cwd":"{0}","tool_input":{{"file_path":"{0}/a.txt"}}}}"#, full.display());
  assert!(sh_with(&dir, "$NV hook claude", &event).status.success());
  assert!(ok(&dir, "$NV watch . --status").contains("watched by"));
  assert_eq!(ok(&dir, "$NV since . start --recorded"), "M a.txt\nA again.txt\nA down.txt");
  // Stopped on purpose, it is not expected, and nobody starts it.
  ok(&dir, "$NV watch . --stop");
  assert!(ok(&dir, "$NV watch . --status").contains("not watched"));
  ok(&dir, "echo quiet > quiet.txt && $NV since . start --recorded");
  assert!(ok(&dir, "$NV watch . --status").contains("not watched"));
  assert!(ok(&dir, "$NV check .").contains("no Git repository"));
}
