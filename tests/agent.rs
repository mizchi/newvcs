//! The commands a writer that changes files all day leans on: going back to
//! a recorded state, naming one, being told of writes by a hook, and
//! publishing into a Git repository in the same directory.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const NV: &str = env!("CARGO_BIN_EXE_nv");

fn sh_with(dir: &Path, script: &str, stdin: &str) -> Output {
  let mut c = Command::new("sh");
  {
    use std::os::unix::process::CommandExt;
    c.process_group(0);
  }
  c.arg("-c").arg(script).current_dir(dir).env("NV", NV).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
  c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null").env_remove("NV_WRITING").env_remove("NV_GIT");
  for (key, value) in
    [("AUTHOR_NAME", "A"), ("AUTHOR_EMAIL", "a@example.com"), ("COMMITTER_NAME", "C"), ("COMMITTER_EMAIL", "c@example.com")]
  {
    c.env(format!("GIT_{key}"), value);
  }
  let mut child = c.spawn().unwrap();
  std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
  common::finish(child)
}

fn text(out: &Output) -> String {
  format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn ok(dir: &Path, script: &str) -> String {
  let out = sh_with(dir, script, "");
  assert!(out.status.success(), "`{script}` failed:\n{}", text(&out));
  String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The exit code of a script and what it printed.
fn code_of(dir: &Path, script: &str) -> (i32, String) {
  let out = sh_with(dir, script, "");
  (out.status.code().unwrap_or(-1), text(&out))
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
  // And where Git moved on its own, nv takes that in and publishes on top of it.
  ok(&dir, "echo raw > b.txt && git commit -q -am 'raw git'");
  let said = sh_with(&dir, "echo a9 > a.txt && $NV commit . 'on where Git went'", "");
  assert!(said.status.success() && text(&said).contains("took in Git's commits"), "{}", text(&said));
  assert_eq!(ok(&dir, "git log --format=%s"), "on where Git went\nraw git\ntwo changes, published\nbase");
  assert_eq!(ok(&dir, "git status --porcelain; $NV check ."), "in step");
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

#[test]
fn a_command_killed_at_any_moment_leaves_a_workspace_that_works() {
  let dir = workspace("killed");
  ok(&dir, "echo good > a.txt && $NV mark . green");
  // Killed two hundred times, somewhere in its first few milliseconds.
  ok(
    &dir,
    "i=0; while [ $i -lt 200 ]; do i=$((i+1)); echo \"edit $i\" >> a.txt; echo \"edit $i\" >> b.txt; $NV snapshot . >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,rand(0.004))'; kill -9 $pid 2>/dev/null; wait $pid 2>/dev/null; done; true",
  );
  let state = ok(&dir, "cat .nv/workspace");
  assert!(state.contains("store ") && state.contains("head ") && state.contains("base "), "the workspace file is whole:\n{state}");
  // What is on disk is recorded and published, and nothing recorded before is out of reach.
  ok(&dir, "$NV snapshot . && $NV commit . 'after two hundred kills'");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "");
  assert_eq!(ok(&dir, "$NV git show HEAD:a.txt | tail -1"), "edit 200");
  ok(&dir, "$NV restore . green >/dev/null");
  assert_eq!(ok(&dir, "cat a.txt b.txt"), "good\nb1");
  // No store appeared where the command was run.
  assert!(!dir.join("objects.seg").exists() && !dir.join("git.map").exists());
}

#[test]
fn looking_in_the_wrong_place_creates_nothing() {
  let dir = workspace("wrong-place");
  ok(&dir, "mkdir empty");
  for command in
    ["$NV verify empty", "$NV stats empty", "$NV stats nowhere", "$NV verify .", "$NV names empty", "$NV checkout nowhere HEAD out"]
  {
    let out = sh_with(&dir, command, "");
    assert!(!out.status.success(), "`{command}` passed: {}", text(&out));
    assert!(text(&out).contains("is not a store"), "`{command}`: {}", text(&out));
  }
  assert_eq!(ok(&dir, "ls empty | wc -l").trim(), "0");
  assert!(!dir.join("nowhere").exists() && !dir.join("out").exists() && !dir.join("objects.seg").exists());
}

#[test]
fn only_paths_inside_the_workspace_can_be_reported() {
  let dir = workspace("outside");
  ok(&dir, "echo secret > ../agent-outside-secret.txt && echo a2 > a.txt && rm b.txt");
  for bad in ["../agent-outside-secret.txt", "/etc/hosts", "d/../../agent-outside-secret.txt", ".nv/workspace", ""] {
    let out = sh_with(&dir, &format!("$NV record . '{bad}'"), "");
    assert!(!out.status.success(), "`{bad}` was taken: {}", text(&out));
  }
  // A directory that is a link out of the workspace is not a way out of it either.
  ok(&dir, "ln -s .. way-out");
  let out = sh_with(&dir, "$NV record . way-out/agent-outside-secret.txt", "");
  assert!(!out.status.success() && text(&out).contains("beyond a symbolic link"), "{}", text(&out));
  let out = sh_with(&dir, "$NV record . \"$PWD/way-out/agent-outside-secret.txt\"", "");
  assert!(!out.status.success(), "{}", text(&out));
  ok(&dir, "rm way-out && $NV record . d/../a.txt");
  // A path inside it, given from the root of the file system, a removed file, and a path through `.`: all fine.
  ok(&dir, "$NV record . \"$PWD/a.txt\" b.txt ./d/x.txt && $NV commit . 'reported' --recorded");
  assert_eq!(ok(&dir, "$NV git ls-tree -r --name-only HEAD"), "a.txt\nd/x.txt");
  let _ = std::fs::remove_file(dir.join("../agent-outside-secret.txt"));
}

#[test]
fn a_workspace_can_be_moved() {
  let dir = workspace("moved-from");
  let to = dir.with_file_name("agent-moved-to");
  let _ = std::fs::remove_dir_all(&to);
  ok(&dir, "echo a2 > a.txt && $NV snapshot . >/dev/null");
  std::fs::rename(&dir, &to).unwrap();
  ok(&to, "echo a3 > a.txt && $NV commit . 'after the move' >/dev/null");
  assert_eq!(ok(&to, "$NV git show HEAD:a.txt"), "a3");
  assert!(!dir.exists(), "nothing was made where the workspace used to be");
}

#[test]
fn forks_that_commit_at_once_share_a_store_without_harming_it() {
  let dir = fresh("forks");
  ok(
    &dir,
    "mkdir main && cd main && $NV git init -q -b main . && for i in $(seq 1 60); do echo \"line $i\" > f$i.txt; done && $NV git add -A && $NV git commit -q -m base",
  );
  ok(&dir, "for k in 1 2 3 4 5 6 7 8; do $NV fork main fork$k >/dev/null; done");
  // Eight forks, six commits each, all at the same time, each to files of its own and one they all have.
  let out = sh_with(
    &dir,
    "for k in 1 2 3 4 5 6 7 8; do ( for n in 1 2 3 4 5 6; do echo \"fork $k commit $n\" >> fork$k/f$k.txt; echo \"$k.$n\" >> fork$k/f60.txt; $NV commit fork$k \"fork $k commit $n\" || echo \"FAILED $k $n\"; done ) & done; wait",
    "",
  );
  assert!(!text(&out).contains("FAILED") && !text(&out).contains("cannot") && !text(&out).contains("not in the store"), "{}", text(&out));
  for k in 1..=8 {
    assert_eq!(ok(&dir, &format!("$NV git -C fork{k} log --format=%s | head -6 | wc -l")).trim(), "6", "fork {k}");
    assert_eq!(ok(&dir, &format!("$NV git -C fork{k} show HEAD:f{k}.txt | tail -1")), format!("fork {k} commit 6"));
    assert_eq!(ok(&dir, &format!("$NV git -C fork{k} show HEAD:f60.txt | wc -l")).trim(), "7");
    ok(&dir, &format!("$NV git -C fork{k} fsck"));
    assert_eq!(ok(&dir, &format!("$NV status fork{k} 2>/dev/null")), "");
  }
}

#[test]
fn files_written_outside_a_view_are_pointed_out() {
  let dir = fresh("outside-view");
  ok(
    &dir,
    "git init -q -b main repo && cd repo && mkdir -p pkg/core pkg/ui docs && echo a > pkg/core/a.ts && echo b > pkg/ui/b.ts && echo g > docs/g.md && echo r > README.md && git add -A && git commit -q -m first",
  );
  ok(&dir, "$NV import repo store >/dev/null && $NV checkout store $(git -C repo rev-parse HEAD) ws pkg/core >/dev/null");
  // Inside the view nothing is said. Outside it, each thing written is named once, at the top of what is new.
  let quiet = sh_with(&dir, "echo a2 > ws/pkg/core/a.ts && $NV status ws", "");
  assert!(!text(&quiet).contains("outside"), "{}", text(&quiet));
  ok(&dir, "mkdir -p ws/docs/deep ws/pkg/ui && echo x > ws/docs/deep/g.md && echo y > ws/README.md && echo z > ws/pkg/ui/b.ts");
  for command in ["$NV status ws", "$NV snapshot ws", "$NV commit ws 'inside only'"] {
    let said = text(&sh_with(&dir, command, ""));
    assert!(said.contains("not recorded: README.md, docs/, pkg/ui/") && said.contains("the view is pkg/core"), "`{command}`: {said}");
  }
}

#[test]
fn a_command_given_wrongly_says_what_was_wrong_and_does_nothing() {
  let dir = workspace("usage");
  ok(&dir, "echo a2 > a.txt");
  // A misspelt option is not silently the slow way of doing the same thing.
  for (command, complaint) in [
    ("$NV snapshot . --recoded", "unknown option `--recoded` (it takes --recorded, --tracked)"),
    ("$NV commit .", "a commit needs a message"),
    ("$NV commit . ''", "a message that says something"),
    ("$NV log . extra", "too many arguments"),
    ("$NV frobnicate", "is not a command"),
    ("$NV restore . ab", "too short"),
    ("$NV hook", "nv hook claude"),
  ] {
    let out = sh_with(&dir, command, "");
    assert_eq!(out.status.code(), Some(2), "`{command}`: {}", text(&out));
    assert!(text(&out).contains(complaint), "`{command}`: {}", text(&out));
    assert!(text(&out).lines().count() < 8, "`{command}` printed the whole usage: {}", text(&out));
  }
  assert_eq!(ok(&dir, "$NV log . | grep -c snapshot || true"), "0", "none of those recorded anything");
  // Help and version are answers, not errors.
  assert!(ok(&dir, "$NV --version").starts_with("nv "));
  assert!(ok(&dir, "$NV help").contains("nv snapshot <dir>") && ok(&dir, "$NV --help").contains("nv commit <dir>"));
  assert_eq!(ok(&dir, "$NV snapshot --help | head -1"), "nv snapshot <dir> [--recorded | --tracked]");
  assert_eq!(ok(&dir, "$NV help snapshot | head -1"), "nv snapshot <dir> [--recorded | --tracked]");
  // States counted back reach behind the first one recorded here, to the commit the work began on.
  ok(&dir, "$NV snapshot . >/dev/null && echo a3 > a.txt && $NV snapshot . >/dev/null && $NV restore . '~1' >/dev/null");
  assert_eq!(ok(&dir, "cat a.txt"), "a2");
  ok(&dir, "$NV restore . '~2' >/dev/null");
  assert_eq!(ok(&dir, "cat a.txt"), "a1");
  assert!(!sh_with(&dir, "$NV restore . '~40'", "").status.success());
}

#[test]
fn a_name_that_differs_only_in_case_or_in_how_its_letters_are_built_is_the_same_file() {
  let dir = workspace("names");
  if !dir.join(".NV").exists() {
    eprintln!("this file system tells names apart by case: nothing to check");
    return;
  }
  ok(&dir, "$NV mark . h >/dev/null && mv a.txt A.TXT");
  // The file system says it is the same file, and so does Git on such a file system: nothing changed.
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null; $NV since . h"), "");
  ok(&dir, "echo changed > A.TXT");
  assert_eq!(
    ok(&dir, "$NV status . 2>/dev/null; $NV record . A.TXT && $NV snapshot . --recorded >/dev/null && $NV since . h"),
    "M a.txt\nM a.txt"
  );
  // Going back writes the file; it does not write it and then remove it under its other name.
  ok(&dir, "$NV restore . h >/dev/null");
  assert_eq!(ok(&dir, "cat a.txt; ls | wc -l | tr -d ' '"), "a1\n3");
  // A name the file system hands back with its accents apart is the name Git has with them joined.
  let apart = "cafe\u{301}.txt";
  let joined = "caf\u{e9}.txt";
  ok(&dir, &format!("echo one > '{apart}' && $NV commit . 'an accent' >/dev/null && $NV status . 2>/dev/null"));
  assert_eq!(
    ok(&dir, "$NV git ls-tree --name-only HEAD | grep caf | od -c | head -1 | grep -c '314 201' || true"),
    "0",
    "stored as the joined name"
  );
  ok(&dir, &format!("echo two > '{joined}'"));
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null | wc -l").trim(), "1", "one file, changed: not one changed and one new");
}

#[test]
fn going_back_is_refused_whole_where_a_file_could_not_be_removed() {
  let dir = workspace("stuck");
  ok(&dir, "$NV mark . g >/dev/null && mkdir ro && echo c > ro/x.txt && echo a2 > a.txt && $NV mark . h >/dev/null && chmod 555 ro");
  let out = sh_with(&dir, "$NV restore . g", "");
  ok(&dir, "chmod 755 ro");
  // A directory nothing can be removed from is found before anything is written: named, and nothing done.
  assert!(!out.status.success() && text(&out).contains("ro"), "{}", text(&out));
  assert_eq!(ok(&dir, "cat a.txt; $NV status . 2>/dev/null"), "a2");
  ok(&dir, "$NV restore . g >/dev/null");
  assert_eq!(ok(&dir, "cat a.txt; ls"), "a1\na.txt\nb.txt\nd");
}

#[test]
fn a_commit_with_no_git_beside_it_is_signed_as_git_would_sign_it() {
  let dir = workspace("signed");
  // The environment first, author and committer each; a time given is the time.
  ok(
    &dir,
    "echo a2 > a.txt && GIT_COMMITTER_NAME=Com GIT_COMMITTER_EMAIL=com@example.com GIT_AUTHOR_DATE='1700000000 +0900' $NV commit . one >/dev/null",
  );
  assert_eq!(ok(&dir, "$NV git log -1 --format='%an <%ae> %at | %cn <%ce>'"), "A <a@example.com> 1700000000 | Com <com@example.com>");
  // Then what the workspace was told, then what the user's Git configuration says.
  ok(&dir, "printf '[user]\\n\\tname = Global\\n\\temail = global@example.com\\n' > ../agent-signed-gitconfig");
  let bare = "env -u GIT_AUTHOR_NAME -u GIT_AUTHOR_EMAIL -u GIT_COMMITTER_NAME -u GIT_COMMITTER_EMAIL GIT_CONFIG_GLOBAL=$PWD/../agent-signed-gitconfig";
  ok(&dir, &format!("echo a3 > a.txt && {bare} $NV commit . two >/dev/null"));
  assert_eq!(ok(&dir, "$NV git log -1 --format='%an <%ae> | %cn'"), "Global <global@example.com> | Global");
  ok(
    &dir,
    &format!(
      "$NV git config user.name Own && $NV git config user.email own@example.com && echo a4 > a.txt && {bare} $NV commit . three >/dev/null"
    ),
  );
  assert_eq!(ok(&dir, "$NV git log -1 --format='%an <%ae>'"), "Own <own@example.com>");
  // A date as most tools write it is read; one that cannot be read is said to be so, and nobody is nobody.
  ok(&dir, "echo a5 > a.txt && GIT_AUTHOR_DATE=2020-01-02T03:04:05+09:00 $NV commit . four >/dev/null");
  assert_eq!(ok(&dir, "$NV git log -1 --format=%at"), "1577901845");
  for (setting, complaint) in [
    ("GIT_AUTHOR_DATE=yesterday", "is not a date nv reads"),
    ("HOME=/nonexistent GIT_CONFIG_GLOBAL=/dev/null", "nobody to sign the commit as"),
  ] {
    let unset = "env -u GIT_AUTHOR_NAME -u GIT_AUTHOR_EMAIL -u GIT_COMMITTER_NAME -u GIT_COMMITTER_EMAIL";
    let command = if setting.starts_with("HOME") {
      format!("rm .nv/git-config; echo a6 > a.txt && {unset} {setting} $NV commit . five")
    } else {
      format!("echo a6 > a.txt && {setting} $NV commit . five")
    };
    let out = sh_with(&dir, &command, "");
    assert!(!out.status.success() && text(&out).contains(complaint), "{setting}: {}", text(&out));
  }
  ok(&dir, "$NV git config user.name Own && $NV git config user.email own@example.com");
  // nv's own `git commit` looks in the same places.
  ok(&dir, &format!("rm .nv/git-config && {bare} $NV git commit -q -am 'by the user of the machine'"));
  assert_eq!(ok(&dir, "$NV git log -1 --format='%an <%ae>'"), "Global <global@example.com>");
  ok(&dir, "echo a7 > a.txt && $NV commit . six >/dev/null");
  // The time is said in the zone the machine is in.
  let zone = ok(&dir, "date +%z");
  assert!(ok(&dir, "$NV git cat-file -p HEAD | grep '^committer'").ends_with(&zone), "{zone}");
  let _ = std::fs::remove_file(dir.join("../agent-signed-gitconfig"));
}

#[test]
fn smaller_things_a_reviewer_tripped_on() {
  let dir = workspace("smaller");
  // From a directory inside the workspace, with paths told from there.
  ok(&dir, "echo x2 > d/x.txt && cd d && $NV record . x.txt && $NV snapshot . --recorded >/dev/null && $NV since . base");
  assert_eq!(ok(&dir, "cd d && $NV since . base"), "M d/x.txt");
  // A path with a newline in it is printed so that a line is still a path.
  ok(&dir, "printf 'odd' > \"$(printf 'two\\nlines.txt')\"");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "A \"two\\nlines.txt\"");
  ok(&dir, "rm two*");
  // A fork goes beside the workspace; a server is a URL with an owner and a repository in it.
  for (command, complaint) in [
    ("$NV fork . ./inside", "a fork goes beside"),
    ("$NV remote . notaurl main", "http://host/<owner>/<repo>"),
    ("$NV remote . http://localhost:1/only-one main", "http://host/<owner>/<repo>"),
    ("$NV help frobnicate", "is not a command"),
    ("$NV mark . before-sync", "cannot be a name"),
  ] {
    let out = sh_with(&dir, command, "");
    assert!(!out.status.success() && text(&out).contains(complaint), "`{command}`: {}", text(&out));
  }
  assert!(!dir.join("inside").exists());
  assert_eq!(ok(&dir, "$NV help hook | head -1"), "nv hook claude");
  // A name given again moves, and says from where.
  ok(&dir, "$NV mark . spot >/dev/null && echo a9 > a.txt");
  assert!(ok(&dir, "$NV mark . spot").contains("(it was "));
  // What was imported can be checked out by the name it was imported by.
  ok(&dir, "git init -q -b main ../agent-smaller-git && cd ../agent-smaller-git && echo g > g.txt && git add -A && git commit -q -m g");
  ok(
    &dir,
    "$NV import ../agent-smaller-git ../agent-smaller-store >/dev/null && $NV checkout ../agent-smaller-store HEAD ../agent-smaller-ws >/dev/null && cat ../agent-smaller-ws/g.txt",
  );
  for left in ["agent-smaller-git", "agent-smaller-store", "agent-smaller-ws"] {
    let _ = std::fs::remove_dir_all(dir.join("..").join(left));
  }
}

#[test]
fn an_edit_that_keeps_a_files_size_and_time_is_still_an_edit() {
  let dir = workspace("same-size");
  ok(&dir, "printf 'v1 config\\n' > a.txt && $NV mark . start >/dev/null && printf 'v2 config\\n' > a.txt && $NV mark . mid >/dev/null");
  // Long enough ago that the file's time is trusted, then: other bytes, the same length, the same time.
  ok(
    &dir,
    "sleep 1.2 && $NV snapshot . >/dev/null; cp -p a.txt ../agent-same-size-keep && printf 'v3 CONFIG\\n' > a.txt && touch -r ../agent-same-size-keep a.txt && rm ../agent-same-size-keep",
  );
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null; $NV since . mid"), "M a.txt\nM a.txt");
  // Reported or not, walked or not, it is seen; and going back keeps it.
  assert_eq!(ok(&dir, "$NV record . a.txt && $NV since . mid --recorded"), "M a.txt");
  let said = ok(&dir, "$NV restore . start");
  assert!(said.contains("what was on disk is kept as"), "{said}");
  assert_eq!(ok(&dir, "cat a.txt && $NV restore . '~0' >/dev/null && cat a.txt"), "v1 config\nv3 CONFIG");
  // Written in place, the file itself the same one: still seen.
  ok(
    &dir,
    "sleep 1.2 && $NV snapshot . >/dev/null; cp -p a.txt ../agent-same-size-keep && printf 'v4 config\\n' | dd of=a.txt conv=notrunc 2>/dev/null && touch -r ../agent-same-size-keep a.txt && rm ../agent-same-size-keep",
  );
  assert_eq!(ok(&dir, "$NV snapshot . --tracked >/dev/null && $NV git show HEAD:a.txt; $NV since . mid"), "a1\nM a.txt");
  // A fork does not have every file read again for it, and an edit made in it afterwards, with
  // the size and the time put back, is an edit: the index it came with is about other files.
  ok(&dir, "$NV commit . settled >/dev/null && sleep 1.2 && $NV status . >/dev/null 2>&1 && $NV fork . ../agent-same-size-fork >/dev/null");
  let fork = dir.join("../agent-same-size-fork");
  let looked = ok(&fork, "$NV status . 2>&1 | grep '^looked at'");
  assert!(looked.contains("read 0"), "{looked}");
  for copy in ["agent-same-size-fork", "agent-same-size-copy"] {
    if copy.ends_with("copy") {
      // A clone where the file system makes one (`-c` is macOS's), a copy keeping the times elsewhere.
      ok(&dir, "cp -c -R -p . ../agent-same-size-copy 2>/dev/null || cp -R -p . ../agent-same-size-copy");
    }
    let at = dir.join("..").join(copy);
    ok(
      &at,
      "cp -p a.txt ../agent-same-size-keep && printf 'V5 CONFIG\\n' > a.txt && touch -r ../agent-same-size-keep a.txt && rm ../agent-same-size-keep",
    );
    assert_eq!(ok(&at, "$NV status . 2>/dev/null"), "M a.txt", "in {copy}");
    let said = ok(&at, "$NV restore . start");
    assert!(said.contains("what was on disk is kept as"), "in {copy}: {said}");
    assert_eq!(ok(&at, "$NV restore . '~0' >/dev/null && cat a.txt"), "V5 CONFIG", "in {copy}");
    let _ = std::fs::remove_dir_all(&at);
  }
}

#[test]
fn nothing_is_recorded_over_a_sync_that_was_cut_short() {
  let dir = workspace("cut-sync");
  // As a kill leaves it: the note that files of the ref were being written here.
  ok(&dir, "echo half > a.txt && printf '%064d 3\\n' 0 > .nv/arriving");
  for command in ["$NV snapshot .", "$NV mark . m", "$NV commit . c", "$NV restore . base"] {
    let out = sh_with(&dir, command, "");
    assert!(!out.status.success() && text(&out).contains("nv sync"), "`{command}`: {}", text(&out));
  }
  // Looking is still looking, and the hook neither fails nor records.
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "M a.txt");
  assert_eq!(ok(&dir, "$NV log . | grep -c snapshot || true"), "0");
}

#[test]
fn what_a_reviewer_broke_in_a_fourth_round_stays_fixed() {
  let dir = workspace("fourth");
  // A directory of files nv does not track, where a file has to go: refused, and the files stay.
  ok(
    &dir,
    "echo '*.log' > .gitignore && echo o > out && $NV mark . with-out >/dev/null && rm out && mkdir out && echo precious > out/build.log",
  );
  let (code, said) = code_of(&dir, "$NV restore . with-out 2>&1");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("out/build.log"), "{said}");
  assert_eq!(ok(&dir, "cat out/build.log"), "precious");
  ok(&dir, "rm -r out && $NV restore . with-out >/dev/null && cat out");
  // A pipe in the tree is not a file: nothing waits on it.
  ok(&dir, "mkfifo pipe && $NV status . >/dev/null 2>&1 && $NV snapshot . >/dev/null; rm pipe");
  // A file that may not be read does not stop the others from being recorded.
  ok(&dir, "echo s > secret.txt && $NV snapshot . >/dev/null && chmod 000 secret.txt && echo more >> a.txt");
  let said = ok(&dir, "$NV snapshot . 2>&1; chmod 644 secret.txt");
  assert!(said.contains("may not be read"), "{said}");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "");
  // Two ways of looking at once is a command given wrongly.
  assert_eq!(code_of(&dir, "$NV snapshot . --recorded --tracked 2>/dev/null").0, 2);
  // One bit of the store flipped: nothing is restored from it, and verify says so.
  ok(&dir, "for i in 1 2 3; do head -c 3000 /dev/urandom | xxd -p > big.txt; $NV mark . v$i >/dev/null; done");
  let store = String::from_utf8(std::fs::read(dir.join(".nv/workspace")).unwrap()).unwrap();
  let store = store.lines().find_map(|line| line.strip_prefix("store ")).map(|at| dir.join(at)).expect("a store");
  let segment = store.join("objects.seg");
  let mut bytes = std::fs::read(&segment).unwrap();
  let before = bytes.clone();
  let mut refused = 0;
  for at in (bytes.len() / 2..bytes.len() - 64).step_by(97) {
    bytes.clone_from(&before);
    bytes[at] ^= 1;
    std::fs::write(&segment, &bytes).unwrap();
    let mut wrong = 0;
    for mark in ["v1", "v2", "v3"] {
      let (code, _) = code_of(&dir, &format!("$NV restore . {mark} >/dev/null 2>&1"));
      if code == 0 {
        // Restored, so what was restored is what was marked.
        let (same, _) = code_of(&dir, &format!("test -z \"$($NV since . {mark} 2>/dev/null)\""));
        assert_eq!(same, 0, "a bit flipped at {at}: {mark} was restored as something else");
      } else {
        wrong += 1;
      }
    }
    if wrong > 0 {
      refused += 1;
      assert_ne!(
        code_of(&dir, &format!("$NV verify {} >/dev/null 2>&1", store.display())).0,
        0,
        "verify passed a store with a bit flipped at {at}"
      );
    }
  }
  std::fs::write(&segment, &before).unwrap();
  assert!(refused > 0, "no flipped bit was ever noticed");
}

#[test]
fn a_commit_killed_at_any_moment_leaves_its_branch_with_the_workspace() {
  let dir = workspace("killed-commit");
  // Killed in the middle of committing, three hundred times: afterwards a commit still lands on the branch.
  ok(
    &dir,
    "i=0; while [ $i -lt 300 ]; do i=$((i+1)); echo \"e$i\" >> a.txt; $NV git commit -q -am \"c$i\" >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,rand(0.012))'; kill -9 $pid 2>/dev/null; wait $pid 2>/dev/null; done; true",
  );
  ok(&dir, "echo more > more.txt && $NV git add more.txt && $NV git commit -q -am 'work after the kills'");
  // The branch is where the workspace is: leaving it and coming back loses nothing.
  ok(&dir, "$NV git switch -q -c elsewhere && $NV git switch -q main");
  assert_eq!(
    ok(&dir, "$NV git log -1 --format=%s && cat more.txt && tail -1 a.txt && $NV git status --short && $NV git fsck"),
    "work after the kills\nmore\ne300"
  );
  // What is nv's own is no file of the tree, however it is named.
  ok(&dir, "$NV git add .nv && $NV git add .NV/workspace; $NV git status --short");
  assert_eq!(code_of(&dir, "$NV git mv a.txt .nv/x 2>/dev/null").0, 125);
  assert_eq!(ok(&dir, "$NV git status --short; ls a.txt"), "a.txt");
}

#[test]
fn what_a_fifth_reviewer_broke_in_recording_and_the_store_stays_fixed() {
  let dir = workspace("fifth");
  let store = String::from_utf8(std::fs::read(dir.join(".nv/workspace")).unwrap()).unwrap();
  let store = store.lines().find_map(|line| line.strip_prefix("store ")).map(|at| dir.join(at)).expect("a store");
  // A file that may not be read, and was never recorded as it is: a restore that would remove it is refused.
  ok(&dir, "$NV mark . clean >/dev/null && rm b.txt && $NV mark . no-b >/dev/null && $NV restore . clean >/dev/null");
  ok(&dir, "echo 'an edit nobody recorded' > b.txt && chmod 000 b.txt");
  let (code, said) = code_of(&dir, "$NV restore . no-b 2>&1");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("b.txt"), "{said}");
  assert_eq!(ok(&dir, "chmod 644 b.txt && cat b.txt"), "an edit nobody recorded");
  ok(&dir, "$NV restore . clean >/dev/null");
  // A file the rules leave out, where a tracked file has to be written: refused, and the file stays.
  ok(
    &dir,
    "echo KEY=committed > .env && $NV mark . with-env >/dev/null && rm .env && echo .env > .gitignore && $NV mark . env-ignored >/dev/null",
  );
  ok(&dir, "echo KEY=local-secret > .env");
  let (code, said) = code_of(&dir, "$NV restore . with-env 2>&1");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains(".env"), "{said}");
  assert_eq!(ok(&dir, "cat .env && rm .env .gitignore && $NV snapshot . >/dev/null; true"), "KEY=local-secret");
  // A directory that may not be listed is left out; the rest is looked at and recorded.
  ok(&dir, "mkdir locked && echo l > locked/l.txt && $NV snapshot . >/dev/null && chmod 000 locked && echo more >> a.txt");
  let said = ok(&dir, "$NV snapshot . 2>&1; $NV status . 2>/dev/null; chmod 755 locked");
  assert!(said.contains("may not be listed") && !said.contains("D locked"), "{said}");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "");
  // The store's ready copy of a file, with a byte changed: not what a restore writes, and verify says so.
  ok(
    &dir,
    "printf 'ready copy v1\\n' > c.txt && $NV mark . c1 >/dev/null && printf 'ready copy v2\\n' > c.txt && $NV mark . c2 >/dev/null && $NV restore . c1 >/dev/null && $NV restore . c2 >/dev/null",
  );
  let copies: Vec<PathBuf> = std::fs::read_dir(store.join("files"))
    .unwrap()
    .flatten()
    .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten().map(|e| e.path()))
    .filter(|p| std::fs::read(p).is_ok_and(|bytes| bytes.starts_with(b"ready copy v1")))
    .collect();
  assert_eq!(copies.len(), 1, "the ready copy of c.txt as it was at c1");
  std::fs::write(&copies[0], "ready copy vX\n").unwrap();
  assert_ne!(code_of(&dir, &format!("$NV verify {} 2>&1", store.display())).0, 0);
  std::fs::write(&copies[0], "ready copy vX\n").unwrap();
  assert_eq!(ok(&dir, "$NV restore . c1 >/dev/null && cat c.txt"), "ready copy v1");
  // A segment cut short, with refs and Git ids still naming what it had: verify does not pass it.
  let segment = store.join("objects.seg");
  let whole = std::fs::read(&segment).unwrap();
  std::fs::write(&segment, b"").unwrap();
  let _ = std::fs::remove_file(store.join("objects.idx"));
  assert_ne!(code_of(&dir, &format!("$NV verify {} >/dev/null 2>&1", store.display())).0, 0);
  std::fs::write(&segment, &whole).unwrap();
  // A fork takes the index as it is after a look at every file: an edit made before the fork,
  // with size and time put back, is an edit in the fork too.
  ok(
    &dir,
    "$NV commit . settled >/dev/null; sleep 1.2; $NV status . >/dev/null 2>&1; printf 'other, longer\\n' > a.txt && $NV mark . m1 >/dev/null && $NV restore . base >/dev/null && sleep 1.2",
  );
  ok(
    &dir,
    "cp -p a.txt ../agent-fifth-keep && tr a-z A-Z < ../agent-fifth-keep > a.txt && touch -r ../agent-fifth-keep a.txt && rm ../agent-fifth-keep && $NV fork . ../agent-fifth-fork >/dev/null",
  );
  let fork = dir.join("../agent-fifth-fork");
  assert_eq!(ok(&fork, "$NV status . 2>/dev/null"), "M a.txt");
  let said = ok(&fork, "$NV restore . m1");
  assert!(said.contains("what was on disk is kept as"), "{said}");
  let _ = std::fs::remove_dir_all(&fork);
  // A fork that cannot be made leaves nothing that would pass for a workspace.
  ok(&dir, "chmod 000 b.txt; $NV fork . ../agent-fifth-fork >/dev/null 2>&1; chmod 644 b.txt");
  assert!(
    !fork.join(".nv").exists() || code_of(&fork, "$NV status . >/dev/null 2>&1").0 == 0 && ok(&fork, "cat b.txt") == ok(&dir, "cat b.txt")
  );
  let _ = std::fs::remove_dir_all(&fork);
}

#[test]
fn what_a_sixth_reviewer_broke_in_recording_stays_fixed() {
  let dir = workspace("sixth");
  // A directory that became a file, reported as one path: the files it held go with it, and the way back is open.
  ok(&dir, "$NV mark . m0 >/dev/null && echo e >> a.txt && $NV mark . m1 >/dev/null");
  ok(&dir, "rm -r d && echo file > d && $NV record . d && $NV snapshot . --recorded >/dev/null");
  assert_eq!(ok(&dir, "$NV since . m1"), "M d\nD d/x.txt");
  assert_eq!(ok(&dir, "$NV restore . m0 >/dev/null && cat d/x.txt && $NV status . 2>/dev/null"), "x1");
  // A file that became a directory, reported by a file inside it.
  ok(&dir, "rm b.txt && mkdir b.txt && echo in > b.txt/in.txt && $NV record . b.txt/in.txt && $NV mark . m2 --recorded >/dev/null");
  assert_eq!(ok(&dir, "$NV since . m2"), "");
  assert_eq!(
    ok(&dir, "$NV restore . m0 >/dev/null && cat b.txt && $NV restore . m2 >/dev/null && cat b.txt/in.txt && $NV restore . m0 >/dev/null"),
    "b1\nin"
  );
  // Looking with `nv git` does not make nv forget what was reported and recorded.
  ok(
    &dir,
    "echo new > new.txt && echo e2 >> a.txt && $NV record . new.txt a.txt && $NV snapshot . --recorded >/dev/null && $NV git status --short >/dev/null && $NV git log --oneline >/dev/null",
  );
  ok(&dir, "$NV commit . published --recorded >/dev/null");
  assert_eq!(ok(&dir, "$NV git show --format=%s --name-status HEAD; $NV status . 2>/dev/null"), "published\n\nM\ta.txt\nA\tnew.txt");
  // A rule that no longer leaves a file out: the file is seen without anyone reporting it.
  ok(&dir, "echo '*.log' > .gitignore && echo o > out.log && $NV mark . ignoring >/dev/null");
  ok(&dir, "echo '*.tmp' > .gitignore && $NV record . .gitignore && $NV mark . seeing --recorded >/dev/null");
  assert_eq!(ok(&dir, "$NV since . seeing; $NV since . ignoring"), "M .gitignore\nA out.log");
  // An ignored file where a directory has to be made: refused before anything is written.
  ok(
    &dir,
    "mkdir dd && echo f > dd/f.txt && $NV mark . with-dd >/dev/null && rm -r dd && echo dd >> .gitignore && echo e3 >> a.txt && $NV mark . without-dd >/dev/null",
  );
  ok(&dir, "echo SECRET > dd");
  let (code, said) = code_of(&dir, "$NV restore . with-dd 2>&1");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("dd"), "{said}");
  assert_eq!(ok(&dir, "cat dd; tail -1 a.txt; $NV since . without-dd"), "SECRET\ne3");
  // An ignored file that already is what would be written is not in the way.
  ok(&dir, "rm dd && mkdir dd && echo f > dd/f.txt && $NV restore . with-dd >/dev/null && cat dd/f.txt");
}

#[test]
fn nothing_is_written_or_read_beyond_a_link() {
  let dir = workspace("links-seventh");
  let outside = dir.join("../agent-links-seventh-outside");
  let _ = std::fs::remove_dir_all(&outside);
  std::fs::create_dir_all(outside.join("sub")).unwrap();
  // A file replaced by a link to a file that is not there: the link is replaced, nothing is made where it led.
  ok(&dir, "$NV mark . x >/dev/null && rm a.txt && ln -s ../agent-links-seventh-outside/victim.txt a.txt && $NV restore . x >/dev/null");
  assert_eq!(ok(&dir, "cat a.txt; test -L a.txt && echo link; ls ../agent-links-seventh-outside"), "a1\nsub");
  // The same when a tree is built from one beside it.
  ok(
    &dir,
    "ln -s $PWD/../agent-links-seventh-outside/created.txt f && $NV commit . A >/dev/null && rm f && echo regular > f && $NV commit . B >/dev/null",
  );
  let store = String::from_utf8(std::fs::read(dir.join(".nv/workspace")).unwrap()).unwrap();
  let store = store.lines().find_map(|line| line.strip_prefix("store ")).map(|at| dir.join(at)).expect("a store");
  let (a, b) = (ok(&dir, "$NV git rev-parse HEAD~1"), ok(&dir, "$NV git rev-parse HEAD"));
  ok(
    &dir,
    &format!(
      "$NV checkout {s} {a} ../agent-links-seventh-ca >/dev/null && $NV checkout {s} {b} ../agent-links-seventh-cb >/dev/null",
      s = store.display()
    ),
  );
  assert_eq!(
    ok(&dir, "cat ../agent-links-seventh-cb/f; test -L ../agent-links-seventh-cb/f && echo link; ls ../agent-links-seventh-outside"),
    "regular\nsub"
  );
  for left in ["agent-links-seventh-ca", "agent-links-seventh-cb"] {
    let _ = std::fs::remove_dir_all(dir.join("..").join(left));
  }
  // A link has to go where a directory of nothing but directories stands: it goes there.
  ok(
    &dir,
    "ln -s ../agent-links-seventh-outside/sub p && $NV mark . m >/dev/null && echo edited > b.txt && rm p && mkdir -p p/q && $NV restore . m >/dev/null",
  );
  assert_eq!(ok(&dir, "test -L p && echo link; cat b.txt"), "link\nb1");
  // A directory of the tree replaced by a link: its files are gone, not the files the link leads to.
  ok(
    &dir,
    "echo outside > ../agent-links-seventh-outside/x.txt && $NV commit . settled >/dev/null && rm -r d && ln -s ../agent-links-seventh-outside d",
  );
  assert_eq!(ok(&dir, "$NV git status --short; $NV git diff --name-status"), "D d/x.txt\n?? d\nD\td/x.txt");
  ok(&dir, "$NV commit . 'a link for a directory' --tracked >/dev/null");
  assert_eq!(ok(&dir, "$NV git show --format= --name-status HEAD; cat ../agent-links-seventh-outside/x.txt"), "D\td/x.txt\noutside");
  // An ignored link nobody recorded, where a tracked file has to go: refused.
  ok(
    &dir,
    "rm d && echo 'tracked env' > .env && $NV mark . with-env >/dev/null && rm .env && echo .env > .gitignore && $NV snapshot . >/dev/null && ln -s ../agent-links-seventh-outside/x.txt .env",
  );
  assert_eq!(code_of(&dir, "$NV restore . with-env 2>/dev/null").0, 2);
  assert_eq!(ok(&dir, "test -L .env && echo link; rm .env .gitignore"), "link");
  // A link named as a rule for directories names them is a link, and is recorded when reported.
  ok(
    &dir,
    "echo 'build/' > .gitignore && $NV commit . rules >/dev/null && ln -s ../agent-links-seventh-outside build && $NV record . build && $NV mark . g --recorded >/dev/null",
  );
  assert_eq!(ok(&dir, "$NV since . g"), "");
  // Looking with `nv git` in between changes nothing of what a later commit publishes.
  ok(
    &dir,
    "echo f > fresh.txt && $NV snapshot . >/dev/null && $NV git status >/dev/null && echo f2 >> fresh.txt && echo more >> a.txt && $NV commit . both --tracked >/dev/null",
  );
  assert_eq!(ok(&dir, "$NV git show --format= --name-status HEAD | sort"), "A\tbuild\nA\tfresh.txt\nM\ta.txt");
  let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn what_an_eighth_reviewer_found_in_recording_stays_fixed() {
  let dir = workspace("eighth");
  let outside = dir.join("../agent-eighth-outside");
  let _ = std::fs::remove_dir_all(&outside);
  std::fs::create_dir_all(outside.join("dir")).unwrap();
  std::fs::write(outside.join("dir/a"), "outside a\n").unwrap();
  let store = String::from_utf8(std::fs::read(dir.join(".nv/workspace")).unwrap()).unwrap();
  let store = store.lines().find_map(|line| line.strip_prefix("store ")).map(|at| dir.join(at)).expect("a store");
  // A checkout makes a workspace: it does not write over one, nor over files that are there.
  let first = ok(&dir, "$NV git rev-parse HEAD");
  ok(&dir, &format!("$NV checkout {} {first} ../agent-eighth-co >/dev/null && echo precious > ../agent-eighth-co/a.txt", store.display()));
  assert_eq!(code_of(&dir, &format!("$NV checkout {} {first} ../agent-eighth-co 2>/dev/null", store.display())).0, 2);
  assert_eq!(ok(&dir, "cat ../agent-eighth-co/a.txt"), "precious");
  let _ = std::fs::remove_dir_all(dir.join("../agent-eighth-co"));
  // A look in Git's terms, between recording and publishing, changes nothing of what is published.
  for look in ["true", "$NV git status >/dev/null && $NV git diff >/dev/null && $NV git log --oneline >/dev/null"] {
    let twin = workspace(if look == "true" { "eighth-plain" } else { "eighth-looked" });
    ok(&twin, "echo s > notes.txt && echo v1 > f && echo v1 > g && $NV record . notes.txt f g && $NV snapshot . --recorded >/dev/null");
    ok(
      &twin,
      &format!(
        "echo notes.txt > .gitignore && echo v2 > f && rm g && {look} && $NV record . .gitignore && $NV commit . c1 --recorded >/dev/null"
      ),
    );
    assert_eq!(
      ok(&twin, "$NV git show --format= --name-status HEAD | sort; $NV git show HEAD:f"),
      "A\t.gitignore\nA\tf\nA\tnotes.txt\nv2",
      "after `{look}`"
    );
  }
  // Two states of this workspace's own, one with a link and one with a directory at the same path: both ways.
  ok(
    &dir,
    "ln -s ../agent-eighth-outside/dir l && $NV mark . linked >/dev/null && rm l && mkdir l && echo mine > l/a && $NV mark . dir >/dev/null",
  );
  ok(&dir, "$NV restore . linked >/dev/null && test -L l && $NV restore . dir >/dev/null && cat l/a && $NV restore . linked >/dev/null");
  assert_eq!(std::fs::read_to_string(outside.join("dir/a")).unwrap(), "outside a\n");
  // A link nobody recorded, standing where a tracked directory was: a restore that would remove it says so first.
  ok(
    &dir,
    "rm l && $NV commit . settled >/dev/null && echo more > d/y.txt && $NV commit . two-in-d >/dev/null && $NV mark . green >/dev/null",
  );
  ok(&dir, "echo x2 > d/x.txt && $NV record . d/x.txt && rm -rf d && ln -s ../agent-eighth-outside/dir d");
  let (code, said) = code_of(&dir, "$NV restore . green --recorded 2>&1");
  assert_eq!(code, 2, "{said}");
  assert_eq!(ok(&dir, "test -L d && echo link"), "link");
  ok(&dir, "rm d && $NV restore . green >/dev/null && cat d/x.txt");
  // A watcher sees a file become an empty directory.
  ok(
    &dir,
    "$NV watch . --start >/dev/null && rm b.txt && mkdir b.txt && sleep 0.4 && $NV mark . emptied --recorded >/dev/null; $NV watch . --stop >/dev/null",
  );
  assert_eq!(ok(&dir, "$NV since . emptied"), "");
  let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn nothing_waits_for_ever() {
  let dir = workspace("limits");
  // A lock held by a process that does not let go: the command gives up, says why, and changes nothing.
  ok(&dir, "echo held > a.txt");
  let started = std::time::Instant::now();
  let (code, said) = code_of(
    &dir,
    "perl -e 'use Fcntl qw(:flock); open(my $f, \">>\", \".nv/lock\") or die; flock($f, LOCK_EX) or die; sleep 30' & holder=$!; sleep 0.3; NV_LOCK_TIMEOUT=1 $NV snapshot . 2>&1; code=$?; kill $holder; exit $code",
  );
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("gave up waiting") && started.elapsed().as_secs() < 10, "{said}");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "M a.txt", "nothing was recorded");
  // A command whose input never ends is stopped at its limit, with exit code 124.
  let started = std::time::Instant::now();
  // (The writer holds the pipe open, and is not waited for: only nv is.)
  let (code, said) = code_of(
    &dir,
    "rm -f ../agent-limits-input && mkfifo ../agent-limits-input && { sleep 30 > ../agent-limits-input 2>/dev/null & writer=$!; } ; NV_TIMEOUT=1 $NV hook claude < ../agent-limits-input 2>&1; code=$?; kill $writer; rm -f ../agent-limits-input; exit $code",
  );
  assert!(started.elapsed().as_secs() < 10, "it ran for {:?}", started.elapsed());
  assert_eq!(code, 124, "{said}");
  assert!(said.contains("was stopped"), "{said}");
  ok(&dir, "$NV snapshot . >/dev/null");
}

#[test]
fn what_a_ninth_reviewer_found_in_recording_stays_fixed() {
  let dir = workspace("ninth");
  // A tracked directory that cannot be gone into: refused before anything is written.
  ok(
    &dir,
    "mkdir -p e/d && echo v1 > e/d/a && $NV mark . x >/dev/null && echo v2 > e/d/a && echo v2 > a.txt && $NV mark . y >/dev/null && chmod 000 e && echo v3 > a.txt",
  );
  let (code, said) = code_of(&dir, "$NV restore . x 2>&1");
  ok(&dir, "chmod 755 e");
  assert_eq!(code, 2, "{said}");
  assert_eq!(ok(&dir, "cat a.txt e/d/a"), "v3\nv2", "nothing was written: {said}");
  ok(&dir, "$NV restore . x >/dev/null");
  assert_eq!(ok(&dir, "cat a.txt e/d/a"), "a1\nv1");
  // A file put back with a sync's markers in it is not committed.
  ok(&dir, "printf 'x\\n<<<<<<< yours\\nmine\\n=======\\ntheirs\\n>>>>>>> theirs\\n' > b.txt");
  let (code, said) = code_of(&dir, "$NV commit . 'with markers' 2>&1");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("b.txt"), "{said}");
  ok(&dir, "echo b2 > b.txt && $NV commit . settled >/dev/null");
  // A checkout does not go through a link to a directory.
  ok(&dir, "mkdir -p ../agent-ninth-empty && ln -sfn ../agent-ninth-empty ../agent-ninth-link");
  let store = String::from_utf8(std::fs::read(dir.join(".nv/workspace")).unwrap()).unwrap();
  let store = store.lines().find_map(|line| line.strip_prefix("store ")).map(|at| dir.join(at)).expect("a store");
  let (code, _) = code_of(&dir, &format!("$NV checkout {} $($NV git rev-parse HEAD) ../agent-ninth-link 2>/dev/null", store.display()));
  assert_eq!(code, 2);
  assert_eq!(ok(&dir, "ls -A ../agent-ninth-empty | wc -l | tr -d ' '"), "0");
  let _ = std::fs::remove_dir_all(dir.join("../agent-ninth-empty"));
  let _ = std::fs::remove_file(dir.join("../agent-ninth-link"));
  // After a look in Git's terms, `nv status` lists against the last snapshot, as before it.
  ok(&dir, "echo new > n.txt && $NV snapshot . >/dev/null && $NV git status >/dev/null && rm n.txt");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "D n.txt");
  // A watcher sees a directory moved away and another made in its place.
  ok(&dir, "mkdir w && echo 1 > w/a && echo 2 > w/b && $NV snapshot . >/dev/null && $NV watch . --start >/dev/null");
  ok(&dir, "mv w w-old && mkdir w && sleep 0.4 && $NV mark . moved --recorded >/dev/null; $NV watch . --stop >/dev/null");
  assert_eq!(ok(&dir, "$NV since . moved"), "");
}

#[test]
fn a_tree_with_names_this_file_system_takes_for_one_is_not_written() {
  // Pairs a case-sensitive file system holds apart and APFS takes for one name: case, and the
  // whole of Unicode case folding (an eleventh reviewer wrote outside through `ß` and `ss`).
  for (n, (link, dir_name)) in
    [("Link", "link"), ("ss", "ß"), ("s", "ſ"), ("fi", "ﬁ"), ("σ", "ς"), ("μ", "µ"), ("SS", "ẞ")].into_iter().enumerate()
  {
    let dir = fresh(&format!("twins-{n}"));
    // A commit a case-sensitive file system can hold: a link to outside, and a file below its twin.
    ok(
      &dir,
      &format!(
        "mkdir outside repo && cd repo && git init -q -b main . && l=$(printf '../outside' | git hash-object -w --stdin) && f=$(echo pwned | git hash-object -w --stdin) && git update-index --add --cacheinfo 120000,$l,'{link}' && git update-index --add --cacheinfo 100644,$f,'{dir_name}/pwned' && git commit -q -m twins"
      ),
    );
    ok(&dir, "$NV import repo store >/dev/null");
    let head = ok(&dir, "git -C repo rev-parse HEAD");
    let folds = ok(&dir, &format!("mkdir -p probe/'{dir_name}' && test -d probe/'{link}' && echo yes || echo no; rm -rf probe"));
    for prefix in ["".to_string(), format!(" '{link}' '{dir_name}'")] {
      let (code, said) = code_of(&dir, &format!("rm -rf co && $NV checkout store {head} co{prefix} 2>&1"));
      if folds == "yes" {
        assert_eq!(code, 2, "`{link}`/`{dir_name}`: {said}");
        assert!(said.contains(link) && said.contains(dir_name), "{said}");
      }
      assert_eq!(ok(&dir, "ls outside | wc -l | tr -d ' '"), "0", "nothing was written outside, through `{link}`/`{dir_name}` `{prefix}`");
    }
  }
}

#[test]
fn what_a_tenth_reviewer_found_in_recording_stays_fixed() {
  // Before the first commit: a look in Git's terms changes nothing, and the first snapshot is somewhere to go back to.
  let dir = fresh("tenth-first");
  ok(
    &dir,
    "$NV git init -q . && echo 1 > a && $NV snapshot . >/dev/null && $NV git status >/dev/null && $NV mark . m --recorded >/dev/null",
  );
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "");
  ok(&dir, "$NV commit . c1 --tracked >/dev/null");
  assert_eq!(ok(&dir, "$NV git ls-tree --name-only HEAD"), "a");
  ok(&dir, "echo 2 > a && $NV commit . c2 >/dev/null && $NV restore . m >/dev/null && cat a");
  let dir = workspace("tenth");
  // One file reported under two spellings is one path.
  let folds = ok(&dir, "test -e A.TXT && echo yes || echo no") == "yes";
  if folds {
    ok(&dir, "echo 1 > a.txt && $NV record . a.txt A.TXT && $NV snapshot . --recorded >/dev/null");
    assert_eq!(ok(&dir, "$NV since . base"), "M a.txt");
    ok(&dir, "$NV commit . 'one path' >/dev/null && $NV restore . base >/dev/null && $NV status . 2>/dev/null");
  }
  // A link to a pipe is a link: nothing waits on what it leads to.
  let started = std::time::Instant::now();
  ok(
    &dir,
    "mkfifo ../agent-tenth-pipe; ln -sf ../agent-tenth-pipe lp && NV_TIMEOUT=20 $NV snapshot . >/dev/null && NV_TIMEOUT=20 $NV status . >/dev/null 2>&1",
  );
  assert!(started.elapsed().as_secs() < 10, "it waited {:?}", started.elapsed());
  ok(&dir, "rm lp ../agent-tenth-pipe && $NV snapshot . >/dev/null");
  // A directory nothing can be removed from, where a file has to be: refused before anything is written.
  ok(&dir, "echo s > d2 && $NV commit . 'd2 is a file' >/dev/null && $NV mark . g >/dev/null");
  ok(&dir, "rm d2 && mkdir -p d2/sub && echo f > d2/sub/f && echo zz > a.txt && $NV snapshot . >/dev/null && chmod 500 d2");
  let (code, said) = code_of(&dir, "$NV restore . g 2>&1");
  ok(&dir, "chmod 755 d2");
  assert_eq!(code, 2, "{said}");
  assert_eq!(ok(&dir, "cat a.txt d2/sub/f"), "zz\nf", "nothing was written: {said}");
  ok(&dir, "$NV restore . g >/dev/null && cat d2");
  // A pipe nobody tracks, where a tracked file has to go: refused, and the pipe stays.
  ok(&dir, "$NV mark . with-x >/dev/null; echo 1 > x && $NV mark . with-x >/dev/null && rm x && $NV snapshot . >/dev/null && mkfifo x");
  assert_eq!(code_of(&dir, "$NV restore . with-x 2>/dev/null").0, 2);
  assert_eq!(ok(&dir, "test -p x && echo pipe; rm x"), "pipe");
}

#[test]
fn a_snapshot_killed_at_any_moment_leaves_every_state_it_reached_named() {
  // Killed after the head moved: the state the head is at is in the log, so it is somewhere a
  // restore can go back to, and a line of the log is never torn into the next one.
  let dir = workspace("eleventh-killed");
  ok(&dir, "$NV mark . green >/dev/null");
  ok(
    &dir,
    "i=0; while [ $i -lt 300 ]; do i=$((i+1)); echo \"only copy $i\" > new$i.txt; $NV snapshot . >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,rand(0.006))'; kill -9 $pid 2>/dev/null; wait $pid 2>/dev/null; head=$(sed -n 's/^head //p' .nv/workspace); $NV log . | grep -q \"$head\" || { echo \"head $head is not in the log after kill $i\"; exit 1; }; done",
  );
  let log = ok(&dir, "cat .nv/oplog");
  for line in log.lines() {
    let fields: Vec<&str> = line.split(' ').collect();
    let whole = fields.len() == 3 && fields[2].len() == 64;
    // A torn record is left as a line of its own, which names nothing.
    assert!(whole || (fields.len() <= 3 && fields.get(2).is_none_or(|id| id.len() < 64)), "a torn line runs into another: {line}");
  }
  for line in ok(&dir, "$NV log .").lines() {
    assert_eq!(line.split_whitespace().nth(2).map(str::len), Some(64), "{line}");
  }
  // Every file written is in some state the log names: going back loses none of them.
  ok(&dir, "$NV snapshot . >/dev/null; $NV restore . green >/dev/null");
  ok(&dir, "$NV restore . '~0' >/dev/null 2>&1 || $NV restore . \"$(sed -n 's/^head //p' .nv/workspace)\" >/dev/null");
  ok(&dir, "for i in $(seq 1 300); do grep -q \"only copy $i\" new$i.txt || { echo missing new$i.txt; exit 1; }; done");
}

#[test]
fn what_an_eleventh_reviewer_found_in_publishing_stays_fixed() {
  // What a commit looks through for a sync's markers is read as the tree has it: a link as a link,
  // and what is not a file not at all. Nothing waits on a pipe, here or beyond a link.
  let dir = workspace("eleventh-publish");
  ok(&dir, "rm -rf ../agent-eleventh-outside && mkdir ../agent-eleventh-outside && mkfifo ../agent-eleventh-outside/fifo");
  let started = std::time::Instant::now();
  ok(
    &dir,
    "printf 1 > a && $NV commit . c1 >/dev/null && ln -s ../agent-eleventh-outside/fifo l && NV_TIMEOUT=20 $NV commit . c2 >/dev/null",
  );
  ok(&dir, "printf 2 > g && $NV snapshot . >/dev/null && rm g && mkfifo g && NV_TIMEOUT=20 $NV commit . c3 >/dev/null 2>&1; rm g");
  assert!(started.elapsed().as_secs() < 10, "it waited {:?}", started.elapsed());
  // A link to a file outside that holds markers is a link, not that file.
  ok(
    &dir,
    "printf '<<<<<<< yours\\na\\n=======\\nb\\n>>>>>>> theirs\\n' > ../agent-eleventh-outside/marked && ln -s ../agent-eleventh-outside/marked m",
  );
  ok(&dir, "$NV commit . 'a link to markers' >/dev/null");
  // What is looked through for markers is what the commit publishes, found by the same look: with
  // `--recorded` the paths reported, and not the whole tree.
  ok(&dir, "printf '<<<<<<< yours\na\n=======\nb\n>>>>>>> theirs\n' > unreported && echo 2 > a && $NV record . a");
  ok(&dir, "$NV commit . 'what was reported' --recorded >/dev/null");
  assert_eq!(ok(&dir, "$NV git show --name-only --format= HEAD"), "a");
  let (code, said) = code_of(&dir, "$NV commit . 'all of it' 2>&1");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("unreported"), "{said}");
}

#[test]
fn an_amend_killed_at_any_moment_leaves_one_commit_in_place_of_the_other() {
  // Killed while amending, three hundred times: the branch has the commit amended or the one that
  // replaced it, never both one on the other, and the workspace names a commit the store has.
  let dir = workspace("killed-amend");
  ok(&dir, "echo e0 >> a.txt && $NV git commit -q -am c0");
  ok(
    &dir,
    "i=0; while [ $i -lt 300 ]; do i=$((i+1)); echo \"e$i\" >> a.txt; $NV git commit -q -a --amend -m \"c$i\" >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,rand(0.012))'; kill -9 $pid 2>/dev/null; wait $pid 2>/dev/null; subjects=$($NV git log --format=%s | tr '\\n' ' '); case \"$subjects\" in 'c'*' base ') ;; *) echo \"after kill $i: $subjects\"; exit 1;; esac; case \"$subjects\" in *' c'*' base ') echo \"after kill $i, one on the other: $subjects\"; exit 1;; esac; done",
  );
  ok(&dir, "$NV git commit -q -a --amend -m last && $NV git fsck");
  assert_eq!(ok(&dir, "$NV git log --format=%s && tail -1 a.txt && $NV git status --short"), "last\nbase\ne300");
}

#[test]
fn what_an_eleventh_reviewer_found_in_recording_stays_fixed() {
  let dir = workspace("eleventh-recording");
  // A report with one path that is not the workspace's is refused whole: none of it is noted.
  ok(&dir, "$NV snapshot . >/dev/null; echo 2 > a.txt");
  let (code, said) = code_of(&dir, "$NV record . a.txt ../zz 2>&1");
  assert_eq!(code, 2, "{said}");
  assert_eq!(ok(&dir, "test -s .nv/journal && tr '\\0' ' ' < .nv/journal; true"), "");
  // A file that may not be read, changed since the state gone back to: not recorded, so not
  // restored, and the restore says so instead of saying it is done. Reported, the same.
  ok(&dir, "$NV snapshot . >/dev/null; $NV mark . m >/dev/null && echo changed > b.txt && chmod 000 b.txt");
  for look in ["", "--recorded"] {
    let (code, said) = code_of(&dir, &format!("$NV record . b.txt && $NV restore . m {look} 2>&1"));
    assert_eq!(code, 2, "{look}: {said}");
    assert!(said.contains("b.txt") && !said.starts_with("restored to"), "{look}: {said}");
  }
  assert_eq!(ok(&dir, "chmod 644 b.txt && cat b.txt"), "changed");
  // The rules of what is left out that are not in the tree: a change to them is seen without a
  // report, and reporting one is not refused.
  ok(&dir, "$NV snapshot . >/dev/null; echo '*.log' > .nv/exclude && echo l > x.log && $NV snapshot . >/dev/null");
  ok(&dir, "rm .nv/exclude && $NV record . .nv/exclude && $NV snapshot . --recorded >/dev/null");
  assert_eq!(ok(&dir, "$NV since . base | grep log; true"), "A x.log");
}

/// A writer that never commits: each state its conditions pass on is committed by nv, under a
/// provisional message, and the commits are made again, fewer and better said, before a push.
#[test]
fn each_checkpoint_is_committed_and_the_commits_are_made_again_fewer() {
  // Made from a commit, as a clone is: what is not pushed is what came after it.
  let top = fresh("each-checkpoint");
  ok(
    &top,
    "git init -q -b main src && cd src && echo base > base.txt && printf '[checks.unit]\\nrun = \"test ! -e broken\"\\n\\n[commit]\\neach = true\\n' > nv.toml && git add -A && git -c user.name=a -c user.email=a@x commit -qm conditions && cd .. && $NV git clone -q src ws",
  );
  let dir = top.join("ws");
  assert!(code_of(&dir, "$NV rebuild . 1 x 2>&1").1.contains("not pushed"), "nothing to make again yet");
  // Three states that pass, one that does not between them.
  ok(&dir, "echo 1 > one && $NV checkpoint . >/dev/null 2>&1");
  ok(&dir, "echo x > broken && ($NV checkpoint . >/dev/null 2>&1; true) && rm broken && echo 2 > two && $NV checkpoint . >/dev/null 2>&1");
  ok(&dir, "echo 3 > three && $NV checkpoint . >/dev/null 2>&1");
  let pending = ok(&dir, "$NV pending .");
  assert_eq!(pending.lines().filter(|l| l.contains("checkpoint: unit passed")).count(), 3, "{pending}");
  assert_eq!(pending.matches("(passed unit)").count(), 3, "{pending}");
  assert!(!pending.contains("broken"), "a state the conditions did not pass is not committed: {pending}");
  assert!(pending.contains("A one") && !pending.contains("M one"), "a new file is added: {pending}");
  // Each with the id `nv git` takes, and `nv rebuild` takes that as well.
  let third = ok(&dir, "$NV git log -1 --format=%h");
  assert!(pending.contains(&format!("git {third}")), "{third}: {pending}");
  let said = ok(&dir, &format!("$NV rebuild . 2 'Add one and two' {third} 'Add three' && $NV pending ."));
  assert!(said.contains("1  ") && said.contains("Add one and two") && said.contains("Add three"), "{said}");
  assert_eq!(said.matches("(passed unit)").count(), 2, "each new commit ends where the conditions passed: {said}");
  assert_eq!(ok(&dir, "$NV git log --format=%s | head -3"), "Add three\nAdd one and two\nconditions");
  assert_eq!(ok(&dir, "$NV git show --name-only --format= HEAD~1"), "one\ntwo");
  // A workspace with no end to what is not pushed makes nothing again.
  let plain = workspace("each-checkpoint-plain");
  let (code, said) = code_of(&plain, "$NV rebuild . 1 x 2>&1");
  assert!(code != 0 && said.contains("nothing was changed") || said.contains("Nothing was changed"), "{said}");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null; cat one two three"), "1\n2\n3", "the files are as they were");
  // Refused, and nothing changed: a commit that is not one of them, the plan out of order.
  for plan in ["0123456789ab 'x'", "2 'b' 1 'a'", "1"] {
    let (code, said) = code_of(&dir, &format!("$NV rebuild . {plan} 2>&1"));
    assert_ne!(code, 0, "{plan}: {said}");
  }
  assert_eq!(ok(&dir, "$NV git log --format=%s | head -2"), "Add three\nAdd one and two");
}

/// The state the conditions passed on is committed though the files moved on since: what came
/// after stays recorded, as changes not committed.
#[test]
fn a_state_the_files_have_moved_on_from_is_committed_as_it_was() {
  use newvcs::store::Store;
  use newvcs::workspace::{self, Look, Workspace};
  let dir = workspace("commit-state");
  ok(&dir, "echo passed > a.txt");
  let state = {
    let mut ws = Workspace::open(&dir).unwrap();
    let mut store = Store::open(&ws.store).unwrap();
    workspace::snapshot(&mut store, &mut ws, "t <t@example.com>", Look::Everywhere).unwrap().unwrap()
  };
  ok(&dir, "echo later > c.txt && $NV snapshot . >/dev/null && echo 'not even recorded' > d/x.txt");
  {
    let mut ws = Workspace::open(&dir).unwrap();
    let mut store = Store::open(&ws.store).unwrap();
    let rest = b"author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nwhat passed\n".to_vec();
    assert!(workspace::commit_state(&mut store, &mut ws, state, rest, "t <t@example.com>").unwrap().is_some());
  }
  assert_eq!(ok(&dir, "$NV git show --name-only --format=%s HEAD"), "what passed\n\na.txt");
  assert_eq!(ok(&dir, "$NV since . base"), "A c.txt\nM d/x.txt");
  assert_eq!(ok(&dir, "cat a.txt c.txt d/x.txt"), "passed\nlater\nnot even recorded");
  // A state from before the last commit is not one to commit again.
  let mut ws = Workspace::open(&dir).unwrap();
  let mut store = Store::open(&ws.store).unwrap();
  let rest = b"author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nagain\n".to_vec();
  let refused = workspace::commit_state(&mut store, &mut ws, state, rest, "t").unwrap_err();
  assert!(refused.contains("not a state recorded here since the last commit"), "{refused}");
}

/// With `auto`, a watcher's snapshot starts the conditions as a hook's does, and with `[commit]
/// each` what passes is committed: a writer that only writes files gets commits.
#[test]
fn a_watched_writer_gets_a_commit_for_each_state_that_passes() {
  let dir = workspace("watched-each");
  ok(
    &dir,
    "printf 'auto = true\\n[checks.unit]\\nrun = \"test ! -e broken\"\\n[commit]\\neach = true\\n' > nv.toml && $NV git add nv.toml && $NV git commit -qm conditions",
  );
  ok(&dir, "$NV watch . --start >/dev/null");
  ok(&dir, "echo 1 > one");
  // Waited for, the write is committed once it is said to be settled.
  let waited = ok(&dir, "$NV pending . --wait");
  assert!(waited.contains("checkpoint: unit passed") && waited.contains("A one"), "{waited}");
  assert!(waited.ends_with("settled: what is recorded is committed"), "{waited}");
  // A state that does not pass is settled as well, and said not to have passed.
  ok(&dir, "echo x > broken");
  let waited = ok(&dir, "$NV pending . --wait");
  assert!(waited.contains("did not pass (unit did not)"), "{waited}");
  assert_eq!(waited.matches("checkpoint: unit passed").count(), 1, "{waited}");
  ok(&dir, "$NV watch . --stop >/dev/null");
}
