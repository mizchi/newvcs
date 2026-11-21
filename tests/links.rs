//! One namespace of repositories in one store, and a directory of one
//! repository standing in another by its id.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NV: &str = env!("CARGO_BIN_EXE_nv");

fn sh(dir: &Path, script: &str) -> Output {
  let mut c = Command::new("sh");
  c.arg("-c").arg(script).current_dir(dir).env("NV", NV).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null");
  for (key, value) in
    [("AUTHOR_NAME", "A"), ("AUTHOR_EMAIL", "a@example.com"), ("COMMITTER_NAME", "C"), ("COMMITTER_EMAIL", "c@example.com")]
  {
    c.env(format!("GIT_{key}"), value);
  }
  common::bounded(&mut c)
}

fn ok(dir: &Path, script: &str) -> String {
  let out = sh(dir, script);
  assert!(out.status.success(), "`{script}` failed:\n{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
  String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Two Git repositories, a library and an application, imported into one store.
fn two_repositories(name: &str) -> PathBuf {
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("links-{name}"));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  ok(
    &dir,
    "git init -q -b main lib && cd lib && mkdir -p src/deep docs && echo 'pub fn a() {}' > src/a.rs && echo 'pub fn b() {}' > src/deep/b.rs && printf '#!/bin/sh\\n' > src/gen.sh && chmod +x src/gen.sh && echo guide > docs/guide.md && git add -A && git commit -q -m 'lib: first'",
  );
  ok(
    &dir,
    "git init -q -b main app && cd app && mkdir -p src && echo 'fn main() {}' > src/main.rs && echo readme > README.md && git add -A && git commit -q -m 'app: first'",
  );
  ok(&dir, "$NV import lib store >/dev/null && $NV import app store >/dev/null");
  ok(&dir, "$NV name store /mizchi/lib $(git -C lib rev-parse HEAD) && $NV name store /mizchi/app $(git -C app rev-parse HEAD)");
  ok(&dir, "$NV checkout store $(git -C app rev-parse HEAD) work >/dev/null");
  dir
}

#[test]
fn repositories_have_names_and_all_of_them_one_hash() {
  let dir = two_repositories("names");
  let names = ok(&dir, "$NV names store");
  let lines: Vec<&str> = names.lines().collect();
  assert!(lines[0].starts_with("/mizchi/app ") && lines[1].starts_with("/mizchi/lib "), "{names}");
  let all = lines.last().unwrap().to_string();
  assert!(all.starts_with("everything "), "{names}");
  // The hash of everything is worked out from the names, and moves when one of them does.
  assert_eq!(ok(&dir, "$NV names store | tail -1"), all);
  ok(
    &dir,
    "cd lib && echo more >> docs/guide.md && git commit -q -am 'lib: second' && cd .. && $NV import lib store >/dev/null && $NV name store /mizchi/lib $(git -C lib rev-parse HEAD)",
  );
  assert_ne!(ok(&dir, "$NV names store | tail -1"), all);
  for bad in ["mizchi/lib", "/mizchi", "/mizchi/lib/more", "/mi zchi/lib", "/../lib"] {
    assert!(!sh(&dir, &format!("$NV name store '{bad}' $(git -C lib rev-parse HEAD)")).status.success(), "{bad} was taken as a name");
  }
}

#[test]
fn a_directory_of_one_repository_stands_in_another_by_its_id() {
  let dir = two_repositories("link");
  let before = ok(&dir, "wc -c < store/objects.seg").trim().parse::<u64>().unwrap();
  ok(&dir, "$NV link work vendor/lib /mizchi/lib src");
  // The files are there, modes and all, and the link is written down in a file that is tracked.
  assert_eq!(
    ok(&dir, "cd work && cat vendor/lib/a.rs vendor/lib/deep/b.rs && test -x vendor/lib/gen.sh && echo exec"),
    "pub fn a() {}\npub fn b() {}\nexec"
  );
  assert!(!dir.join("work/vendor/lib/docs").exists(), "only the directory asked for");
  let manifest = ok(&dir, "cat work/nv.links");
  assert!(manifest.contains("vendor/lib") && manifest.contains("/mizchi/lib") && manifest.contains("src"), "{manifest}");
  // Nothing of the library was stored a second time: a commit, the file that lists links, and the directories on the way.
  let grew = ok(&dir, "wc -c < store/objects.seg").trim().parse::<u64>().unwrap() - before;
  assert!(grew < 1024, "linking stored {grew} bytes");
  assert!(ok(&dir, "$NV links work").contains("as linked, up to date"), "{}", ok(&dir, "$NV links work"));
  // To Git it is an ordinary directory, and the very tree the library has.
  ok(&dir, "$NV commit work 'link the library' >/dev/null && $NV export work app refs/heads/main >/dev/null");
  ok(&dir, "git -C app fsck --strict");
  assert_eq!(ok(&dir, "git -C app rev-parse main:vendor/lib"), ok(&dir, "git -C lib rev-parse main:src"));
  assert_eq!(
    ok(&dir, "git -C app ls-tree -r --name-only main"),
    "README.md\nnv.links\nsrc/main.rs\nvendor/lib/a.rs\nvendor/lib/deep/b.rs\nvendor/lib/gen.sh"
  );
}

#[test]
fn a_link_follows_its_repository_when_asked_and_only_touches_what_changed() {
  let dir = two_repositories("update");
  ok(&dir, "$NV link work vendor/lib /mizchi/lib src && $NV commit work 'link' >/dev/null");
  // The library moves on: one file changed, one added, one removed.
  ok(
    &dir,
    "cd lib && echo 'pub fn a2() {}' >> src/a.rs && echo new > src/new.rs && git rm -q src/gen.sh && git add -A && git commit -q -m 'lib: second'",
  );
  ok(&dir, "$NV import lib store >/dev/null && $NV name store /mizchi/lib $(git -C lib rev-parse HEAD)");
  assert!(ok(&dir, "$NV links work").contains("behind"), "{}", ok(&dir, "$NV links work"));
  let untouched = ok(&dir, "perl -e 'print((stat shift)[9])' work/vendor/lib/deep/b.rs");
  let updated = ok(&dir, "$NV links work --update");
  assert!(updated.contains("3 paths"), "{updated}");
  assert_eq!(
    ok(&dir, "cd work && tail -1 vendor/lib/a.rs && cat vendor/lib/new.rs && ls vendor/lib | tr '\\n' ' '"),
    "pub fn a2() {}\nnew\na.rs deep new.rs"
  );
  assert_eq!(
    ok(&dir, "perl -e 'print((stat shift)[9])' work/vendor/lib/deep/b.rs"),
    untouched,
    "a file the update did not change was not written"
  );
  assert!(ok(&dir, "$NV links work").contains("as linked, up to date"));
  // Changed here, a link says so, and updating will not write over it.
  ok(&dir, "$NV commit work 'follow' >/dev/null && echo local >> work/vendor/lib/a.rs");
  assert!(ok(&dir, "$NV links work").contains("changed here"), "{}", ok(&dir, "$NV links work"));
  ok(
    &dir,
    "cd lib && echo 'pub fn a3() {}' >> src/a.rs && git commit -q -am 'lib: third' && cd .. && $NV import lib store >/dev/null && $NV name store /mizchi/lib $(git -C lib rev-parse HEAD)",
  );
  let refused = sh(&dir, "$NV links work --update");
  let said = String::from_utf8_lossy(&refused.stderr).into_owned();
  assert!(
    !refused.status.success() && said.contains("nv link <dir> vendor/lib /mizchi/lib src") && said.contains("nv unlink <dir> vendor/lib"),
    "{said}"
  );
  assert_eq!(ok(&dir, "tail -1 work/vendor/lib/a.rs"), "local");
  // Both ways out work: the directory kept and no longer followed, or the repository's taken in its place.
  ok(
    &dir,
    "$NV commit work 'my change' >/dev/null && $NV link work vendor/again /mizchi/lib src >/dev/null && $NV unlink work vendor/again",
  );
  assert_eq!(
    ok(&dir, "cat work/nv.links | grep -c again; ls work/vendor/again | head -1; $NV links work | grep -c again || true"),
    "0\na.rs\n0"
  );
  ok(&dir, "$NV link work vendor/lib /mizchi/lib src >/dev/null");
  assert_eq!(ok(&dir, "tail -1 work/vendor/lib/a.rs; $NV links work | grep vendor/lib | cut -f3"), "pub fn a3() {}\nas linked, up to date");
  assert!(!sh(&dir, "$NV unlink work no/such").status.success());
  // The links file names Git's id of the commit too, for whoever has only Git.
  assert!(ok(&dir, "cut -f5 work/nv.links | tail -1").len() == 40);
  // A whole repository can be linked, and a name that is not there cannot.
  ok(&dir, "$NV link work third-party/lib-all /mizchi/lib");
  assert_eq!(ok(&dir, "cat work/third-party/lib-all/docs/guide.md"), "guide");
  assert!(!sh(&dir, "$NV link work x /mizchi/nothing").status.success());
  for bad in ["/tmp/abs", ".nv/x", "../escape", ".", "a/../../b"] {
    assert!(!sh(&dir, &format!("$NV link work '{bad}' /mizchi/lib")).status.success(), "`{bad}` was taken as a place for a link");
  }
  assert!(!dir.join("work/tmp").exists() && !dir.join("work/.nv/x").exists());
  assert!(!sh(&dir, "$NV link work y /mizchi/lib no/such/dir").status.success());
}

#[test]
fn a_link_is_one_change_however_many_files_the_directory_has() {
  let dir = two_repositories("one-change");
  ok(
    &dir,
    "cd lib && mkdir -p src/many && for i in $(seq 1 1500); do echo \"pub fn f$i() {}\" > src/many/file_number_$i.rs; done && git add -A && git commit -q -m 'lib: many' && cd .. && $NV import lib store >/dev/null && $NV name store /mizchi/lib $(git -C lib rev-parse HEAD)",
  );
  let size = |dir: &Path| ok(dir, "wc -c < store/objects.seg").trim().parse::<u64>().unwrap();
  let before = size(&dir);
  ok(&dir, "$NV link work vendor/lib /mizchi/lib src");
  // The files it wrote are known to be what it wrote, though none of them had been written
  // out of this store before: the next look does not read them all again.
  let looked = ok(&dir, "$NV status work 2>&1 | grep '^looked at'");
  let reads: usize = looked.split("read ").nth(1).and_then(|rest| rest.split(',').next()).and_then(|n| n.parse().ok()).expect(&looked);
  assert!(reads < 600, "after writing 1500 files, a look read {reads} of them: {looked}");
  ok(&dir, "$NV commit work 'link the library' >/dev/null");
  // Recorded and then published: neither commit lists the fifteen hundred files.
  let grew = size(&dir) - before;
  assert!(grew < 2048, "linking and publishing stored {grew} bytes");
  assert_eq!(ok(&dir, "ls work/vendor/lib/many | wc -l").trim(), "1500");
  assert!(ok(&dir, "$NV links work").contains("as linked, up to date"));
  // Work on top of the link is recorded as changes to it, and published without undoing it.
  ok(&dir, "echo mine >> work/vendor/lib/a.rs && echo more >> work/README.md && $NV commit work 'change both' >/dev/null");
  assert!(ok(&dir, "$NV links work").contains("changed here"));
  ok(&dir, "$NV export work app refs/heads/main >/dev/null && git -C app fsck --strict");
  assert_eq!(ok(&dir, "git -C app ls-tree -r --name-only main | wc -l").trim(), "1506");
  assert_eq!(ok(&dir, "git -C app show main:vendor/lib/a.rs | tail -1"), "mine");
  assert_eq!(ok(&dir, "git -C app rev-parse main~1:vendor/lib"), ok(&dir, "git -C lib rev-parse main:src"));
}
