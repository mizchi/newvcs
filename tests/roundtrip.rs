//! End to end against a real Git repository built in a temporary directory.

use newvcs::store::Store;
use newvcs::workspace::{self, Look, Workspace};
use newvcs::{git, hex};
use std::path::{Path, PathBuf};
use std::process::Command;

fn sh(dir: &Path, script: &str) -> String {
  let out = Command::new("sh")
    .arg("-ec")
    .arg(script)
    .current_dir(dir)
    .env("GIT_AUTHOR_NAME", "a")
    .env("GIT_AUTHOR_EMAIL", "a@example.com")
    .env("GIT_COMMITTER_NAME", "c")
    .env("GIT_COMMITTER_EMAIL", "c@example.com")
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .output()
    .unwrap();
  assert!(out.status.success(), "{script}\n{}", String::from_utf8_lossy(&out.stderr));
  String::from_utf8(out.stdout).unwrap()
}

/// A history with what makes Git trees awkward: an executable, a symlink, a
/// file that becomes a directory, names that sort differently as files and
/// directories, a merge, a deletion, and an empty commit.
fn repository(name: &str) -> PathBuf {
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("repo")).unwrap();
  sh(
    &dir.join("repo"),
    r##"
    git init -q -b main .
    mkdir -p docs pkg/core pkg/ui
    echo "# readme" > README.md
    echo "guide" > docs/guide.md
    printf 'export const a = 1;\n' > pkg/core/a.ts
    printf 'export const b = 2;\n' > pkg/ui/b.ts
    printf '#!/bin/sh\necho hi\n' > run.sh && chmod +x run.sh
    ln -s README.md link
    echo x > "a.b" && mkdir a && echo y > a/inner && echo z > "a b"
    git add -A && git commit -q -m "first"
    git commit -q --allow-empty -m "empty"
    git checkout -q -b side
    echo "side" > pkg/ui/side.ts && git add -A && git commit -q -m "side work"
    git checkout -q main
    printf 'export const a = 1;\nexport const c = 3;\n' > pkg/core/a.ts
    git rm -q docs/guide.md && rm "a.b" && mkdir a.b && echo d > a.b/now-a-dir
    git add -A && git commit -q -m "main work"
    git merge -q --no-ff side -m "merge side"
    printf 'export const a = 1;\nexport const c = 4;\n' > pkg/core/a.ts
    git commit -q -am "after merge"
    "##,
  );
  dir
}

#[test]
fn a_git_history_maps_back_to_the_same_ids() {
  let dir = repository("ids");
  let mut store = Store::open(&dir.join("store")).unwrap();
  let done = git::import(&mut store, &dir.join("repo"), "HEAD", None).unwrap();
  assert_eq!(done.commits, 6);
  assert_eq!(done.mismatches, Vec::<String>::new());
  // Importing again finds everything there.
  let again = git::import(&mut store, &dir.join("repo"), "HEAD", None).unwrap();
  assert_eq!((again.commits, again.already), (0, 6));
  drop(store);

  // From the store alone, in another process's shoes: every commit id comes out again.
  let mut store = Store::open(&dir.join("store")).unwrap();
  let (checked, wrong) = git::verify(&mut store).unwrap();
  assert_eq!((checked, wrong), (6, Vec::new()));
}

#[test]
fn a_workspace_of_one_directory_commits_without_the_rest() {
  let dir = repository("subdir");
  let (repo, store_dir) = (dir.join("repo"), dir.join("store"));
  let mut store = Store::open(&store_dir).unwrap();
  git::import(&mut store, &repo, "HEAD", None).unwrap();
  let head = store.resolve(sh(&repo, "git rev-parse HEAD").trim()).expect("imported");

  // Only pkg/core is written, and only its one blob is read.
  let ws_dir = dir.join("ws");
  let done = workspace::checkout(&mut store, head, &ws_dir, vec![b"pkg/core".to_vec()]).unwrap();
  assert_eq!((done.files, done.materialised), (1, 1));
  assert!(ws_dir.join("pkg/core/a.ts").exists());
  assert!(!ws_dir.join("README.md").exists() && !ws_dir.join("pkg/ui").exists());
  let mut ws = Workspace::open(&ws_dir).unwrap();
  assert!(workspace::status(&ws, Look::Everywhere).unwrap().0.is_empty());
  assert_eq!(workspace::commit(&mut store, &mut ws, "t <t@example.com>", "nothing").unwrap(), None);

  // Edit, add and delete inside the view; a file outside it is not this workspace's business.
  std::fs::write(ws_dir.join("pkg/core/a.ts"), "export const a = 100;\n").unwrap();
  std::fs::write(ws_dir.join("pkg/core/new.ts"), "export const n = 0;\n").unwrap();
  std::fs::write(ws_dir.join("stray.txt"), "outside the view\n").unwrap();
  let marks: Vec<String> = workspace::status(&ws, Look::Everywhere)
    .unwrap()
    .0
    .iter()
    .map(|(p, new, old)| format!("{}{}{}", String::from_utf8_lossy(p), new.is_some() as u8, old.is_some() as u8))
    .collect();
  assert_eq!(marks, ["pkg/core/a.ts11", "pkg/core/new.ts10"]);
  let made = workspace::commit(&mut store, &mut ws, "t <t@example.com>", "from a subdirectory").unwrap().expect("there were changes");

  // Git accepts the result, and sees exactly those two paths changed on top of HEAD.
  let (oid, written) = git::export(&mut store, &made, &repo, "refs/heads/from-nv").unwrap();
  assert_eq!(written, 6, "two blobs, the trees pkg/core, pkg and root, and the commit");
  assert_eq!(sh(&repo, "git diff --name-status main from-nv"), "M\tpkg/core/a.ts\nA\tpkg/core/new.ts\n");
  assert_eq!(sh(&repo, "git rev-parse from-nv^").trim(), sh(&repo, "git rev-parse main").trim());
  assert_eq!(sh(&repo, "git rev-parse from-nv").trim(), hex(&oid));
  assert_eq!(sh(&repo, "git show from-nv:README.md"), "# readme\n");
  sh(&repo, "git fsck --strict --no-dangling");
}

#[test]
fn workspaces_share_files_and_a_fork_keeps_untracked_output() {
  let dir = repository("share");
  let mut store = Store::open(&dir.join("store")).unwrap();
  git::import(&mut store, &dir.join("repo"), "HEAD", None).unwrap();
  let head = store.resolve(sh(&dir.join("repo"), "git rev-parse HEAD").trim()).unwrap();

  let first = workspace::checkout(&mut store, head, &dir.join("one"), Vec::new()).unwrap();
  assert!(first.materialised > 0 && first.files > first.materialised, "the symlink is a file but not a materialised blob");
  // The whole tree, as Git has it: modes and the link included.
  assert_eq!(std::fs::read_link(dir.join("one/link")).unwrap(), Path::new("README.md"));
  assert_eq!(sh(&dir.join("one"), "test -x run.sh && cat a.b/now-a-dir && cat 'a b' && cat a/inner"), "d\nz\ny\n");
  // Let the first checkout's files age past the point where an index trusts their time.
  std::thread::sleep(std::time::Duration::from_millis(40));
  // A second workspace reads nothing from the store: every file is already materialised.
  let second = workspace::checkout(&mut store, head, &dir.join("two"), Vec::new()).unwrap();
  assert_eq!((second.files, second.materialised), (first.files, 0));
  // Writing in one does not show in the other.
  std::fs::write(dir.join("one/README.md"), "changed here\n").unwrap();
  assert_eq!(std::fs::read_to_string(dir.join("two/README.md")).unwrap(), "# readme\n");

  // A fork carries what no commit holds: build output and the like.
  std::fs::create_dir_all(dir.join("two/target")).unwrap();
  std::fs::write(dir.join("two/target/cache.bin"), "warm").unwrap();
  workspace::fork(&dir.join("two"), &dir.join("three")).unwrap();
  assert_eq!(std::fs::read_to_string(dir.join("three/target/cache.bin")).unwrap(), "warm");
  let forked = Workspace::open(&dir.join("three")).unwrap();
  assert_eq!(forked.base, Some(head));
  let (differences, looked) = workspace::status(&forked, Look::Everywhere).unwrap();
  assert!(differences.is_empty());
  // The fork kept the files' times, so its index still vouches for them.
  assert!(looked.reads <= 1, "only the symlink, made at checkout, may need a look: {looked:?}");
  assert!(workspace::fork(&dir.join("two"), &dir.join("three")).is_err());
}

#[test]
fn snapshots_record_work_and_a_commit_publishes_it_as_one() {
  let dir = repository("snapshots");
  let (repo, mut store) = (dir.join("repo"), Store::open(&dir.join("store")).unwrap());
  git::import(&mut store, &repo, "HEAD", None).unwrap();
  let head = store.resolve(sh(&repo, "git rev-parse HEAD").trim()).unwrap();
  let ws_dir = dir.join("ws");
  workspace::checkout(&mut store, head, &ws_dir, Vec::new()).unwrap();
  let mut ws = Workspace::open(&ws_dir).unwrap();
  let author = "t <t@example.com>";
  // Let the checkout's writes age past the point where the index trusts a file's time.
  std::thread::sleep(std::time::Duration::from_millis(40));
  assert_eq!(workspace::snapshot(&mut store, &mut ws, author, Look::Everywhere).unwrap(), None);

  // Three steps of work, a snapshot after each. One of them is undone by the next.
  std::fs::write(ws_dir.join("pkg/core/a.ts"), "step one\n").unwrap();
  let one = workspace::snapshot(&mut store, &mut ws, author, Look::Everywhere).unwrap().expect("changed");
  std::fs::write(ws_dir.join("scratch.txt"), "temporary\n").unwrap();
  std::fs::write(ws_dir.join("pkg/core/a.ts"), "step two\n").unwrap();
  let two = workspace::snapshot(&mut store, &mut ws, author, Look::Everywhere).unwrap().expect("changed");
  std::fs::remove_file(ws_dir.join("scratch.txt")).unwrap();
  std::fs::remove_file(ws_dir.join("run.sh")).unwrap();
  let three = workspace::snapshot(&mut store, &mut ws, author, Look::Everywhere).unwrap().expect("changed");
  assert!(one != two && two != three);
  assert_eq!((ws.base, ws.head), (Some(head), Some(three)));

  // Published: one commit on the imported head, with the net change only.
  let published = workspace::commit(&mut store, &mut ws, author, "three steps").unwrap().expect("there is work to publish");
  assert_eq!((ws.base, ws.head), (Some(published), Some(published)));
  git::export(&mut store, &published, &repo, "refs/heads/published").unwrap();
  assert_eq!(sh(&repo, "git diff --name-status main published"), "M\tpkg/core/a.ts\nD\trun.sh\n");
  assert_eq!(sh(&repo, "git rev-list --count main..published").trim(), "1");
  assert_eq!(sh(&repo, "git show published:pkg/core/a.ts"), "step two\n");
  // The steps are still there to go back to.
  let kinds: Vec<String> = ws.operations().into_iter().map(|(_, what, _)| what).collect();
  assert_eq!(kinds, ["snapshot", "snapshot", "snapshot", "commit"]);
  assert_eq!(workspace::commit(&mut store, &mut ws, author, "again").unwrap(), None);
}

#[test]
fn unchanged_files_are_not_read() {
  let dir = repository("index");
  let mut store = Store::open(&dir.join("store")).unwrap();
  git::import(&mut store, &dir.join("repo"), "HEAD", None).unwrap();
  let head = store.resolve(sh(&dir.join("repo"), "git rev-parse HEAD").trim()).unwrap();
  let ws_dir = dir.join("ws");
  let done = workspace::checkout(&mut store, head, &ws_dir, Vec::new()).unwrap();
  let ws = Workspace::open(&ws_dir).unwrap();

  // Files whose blob was materialised a moment ago have a time too fresh to
  // trust, so the first look reads them; nothing is reported either way.
  let (found, first) = workspace::status(&ws, Look::Everywhere).unwrap();
  assert!(found.is_empty());
  // Having read them and found them unchanged, it does not read them a second time.
  std::thread::sleep(std::time::Duration::from_millis(40));
  let (_, again) = workspace::status(&ws, Look::Everywhere).unwrap();
  let (_, third) = workspace::status(&ws, Look::Everywhere).unwrap();
  assert!(first.reads >= again.reads && third.reads == 0, "{first:?} {again:?} {third:?}");

  // A second workspace clones files written before: their times are old, and none is read.
  std::thread::sleep(std::time::Duration::from_millis(40));
  workspace::checkout(&mut store, head, &dir.join("ws2"), Vec::new()).unwrap();
  let ws2 = Workspace::open(&dir.join("ws2")).unwrap();
  let (found, looked) = workspace::status(&ws2, Look::Everywhere).unwrap();
  assert!(found.is_empty());
  assert_eq!(looked.stats, done.files);
  // Only the symlink, made at checkout, is too fresh to take on trust.
  assert!(looked.reads <= 1, "{looked:?}");

  // A change of the same length is still seen: the time moved.
  std::fs::write(dir.join("ws2/README.md"), "# README\n").unwrap();
  let (found, looked) = workspace::status(&ws2, Look::Everywhere).unwrap();
  assert_eq!(found.len(), 1);
  assert!(looked.reads <= 2, "{looked:?}");
  // And a file touched but not changed is read, found the same, and not reported.
  let now = std::time::SystemTime::now();
  std::fs::File::options().write(true).open(dir.join("ws2/pkg/ui/b.ts")).unwrap().set_modified(now).unwrap();
  assert_eq!(workspace::status(&ws2, Look::Everywhere).unwrap().0.len(), 1);
}

#[test]
fn a_writer_that_reports_its_writes_is_not_walked_after() {
  let dir = repository("recorded");
  let mut store = Store::open(&dir.join("store")).unwrap();
  git::import(&mut store, &dir.join("repo"), "HEAD", None).unwrap();
  let head = store.resolve(sh(&dir.join("repo"), "git rev-parse HEAD").trim()).unwrap();
  let ws_dir = dir.join("ws");
  workspace::checkout(&mut store, head, &ws_dir, Vec::new()).unwrap();
  let mut ws = Workspace::open(&ws_dir).unwrap();

  // Two writes go through the tool; a third is made behind its back.
  std::fs::write(ws_dir.join("pkg/ui/b.ts"), "reported\n").unwrap();
  ws.record(b"pkg/ui/b.ts").unwrap();
  std::fs::write(ws_dir.join("new.txt"), "reported too\n").unwrap();
  ws.record(b"new.txt").unwrap();
  std::fs::write(ws_dir.join("README.md"), "not reported\n").unwrap();

  let (found, looked) = workspace::status(&ws, Look::Recorded).unwrap();
  let paths: Vec<String> = found.iter().map(|d| String::from_utf8_lossy(&d.0).into_owned()).collect();
  assert_eq!(paths, ["new.txt", "pkg/ui/b.ts"]);
  assert_eq!(looked.stats, 2, "only the reported paths are looked at");
  workspace::snapshot(&mut store, &mut ws, "t <t@example.com>", Look::Recorded).unwrap().expect("changed");
  // The unreported write is still found by a walk, so nothing is lost: it is only later.
  let (found, _) = workspace::status(&ws, Look::Everywhere).unwrap();
  assert_eq!(found.len(), 1);
  assert_eq!(found[0].0, b"README.md");
}
