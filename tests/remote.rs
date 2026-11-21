//! Three workspaces, three stores, one server. Needs the server running:
//!
//!   (cd server && pnpm dev -- --port 8799) &
//!   NV_SERVER=http://localhost:8799 cargo test --test remote
//!
//! Without `NV_SERVER` the test says so and passes.

use newvcs::remote::{self, Synced};
use newvcs::store::Store;
use newvcs::workspace::{self, Look, Workspace};
use newvcs::{Id, git};
use std::path::{Path, PathBuf};
use std::process::Command;

const AUTHOR: &str = "t <t@example.com>";

fn sh(dir: &Path, script: &str) -> String {
  let out = Command::new("sh")
    .arg("-ec")
    .arg(script)
    .current_dir(dir)
    .env("GIT_AUTHOR_NAME", "a")
    .env("GIT_AUTHOR_EMAIL", "a@example.com")
    .env("GIT_COMMITTER_NAME", "c")
    .env("GIT_COMMITTER_EMAIL", "c@example.com")
    .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
    .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .output()
    .unwrap();
  assert!(out.status.success(), "{script}\n{}", String::from_utf8_lossy(&out.stderr));
  String::from_utf8(out.stdout).unwrap()
}

/// One participant: its own import of the Git repository, and a workspace of it.
struct Peer {
  store: Store,
  ws: Workspace,
  dir: PathBuf,
}

impl Peer {
  fn new(root: &Path, name: &str, prefixes: &[&str], url: &str) -> Peer {
    let mut store = Store::create(&root.join(format!("store-{name}"))).unwrap();
    git::import(&mut store, &root.join("repo"), "HEAD", None).unwrap();
    let head = store.resolve(sh(&root.join("repo"), "git rev-parse HEAD").trim()).unwrap();
    let dir = root.join(format!("ws-{name}"));
    workspace::checkout(&mut store, head, &dir, prefixes.iter().map(|p| p.as_bytes().to_vec()).collect()).unwrap();
    let mut ws = Workspace::open(&dir).unwrap();
    ws.set_remote(url, "main").unwrap();
    Peer { store, ws, dir }
  }
  fn write(&self, path: &str, text: &str) {
    std::fs::create_dir_all(self.dir.join(path).parent().unwrap()).unwrap();
    std::fs::write(self.dir.join(path), text).unwrap();
  }
  fn read(&self, path: &str) -> Option<String> {
    std::fs::read_to_string(self.dir.join(path)).ok()
  }
  fn commit(&mut self, message: &str) -> Id {
    workspace::commit(&mut self.store, &mut self.ws, AUTHOR, message, Look::Everywhere).unwrap().expect("there is something to commit")
  }
  /// Starting a ref needs a commit of the workspace's own; with nothing changed there is none to make.
  fn commit_empty_is_none(&mut self) {
    assert_eq!(workspace::commit(&mut self.store, &mut self.ws, AUTHOR, "nothing", Look::Everywhere).unwrap(), None);
  }
  fn push(&mut self) -> Result<remote::Pushed, String> {
    remote::push(&mut self.store, &mut self.ws, AUTHOR)
  }
  fn sync(&mut self) -> Result<Synced, String> {
    remote::sync(&mut self.store, &mut self.ws, AUTHOR)
  }
}

#[test]
fn three_workspaces_write_to_one_ref() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(root.join("repo")).unwrap();
  sh(
    &root.join("repo"),
    r##"
    git init -q -b main .
    mkdir -p docs pkg/core pkg/ui
    echo "# readme" > README.md && echo "guide" > docs/guide.md
    echo "export const a = 1;" > pkg/core/a.ts && echo "export const b = 2;" > pkg/ui/b.ts
    git add -A && git commit -q -m "first"
    "##,
  );
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/remote-test-{unique}", server.trim_end_matches('/'));
  // Three imports of one history give the same ids, so the three never exchange it.
  let mut ann = Peer::new(&root, "ann", &[], &url);
  let mut bob = Peer::new(&root, "bob", &["pkg/core"], &url);
  let mut cat = Peer::new(&root, "cat", &[], &url);

  // Ann starts the ref.
  ann.write("pkg/ui/b.ts", "export const b = 20;\n");
  ann.commit("ann: ui");
  let pushed = ann.push().unwrap();
  assert_eq!((pushed.updated, pushed.rebased, pushed.contents_sent), (1, 0, 1));

  // Bob, who holds pkg/core alone and has never looked at the ref, has a commit of his own.
  bob.write("pkg/core/a.ts", "export const a = 10;\n");
  bob.commit("bob: core");
  let pushed = bob.push().unwrap();
  assert_eq!((pushed.updated, pushed.rebased), (0, 1), "bob asked where he stood, and the server put his commit on ann's");
  // Ann's change is outside his view: he knows of it and fetched none of it.
  assert!(!bob.dir.join("pkg/ui").exists());
  assert_eq!(bob.ws.base, bob.ws.pushed);

  // Ann catches up and has Bob's file; Cat arrives late and has both.
  let got = ann.sync().unwrap();
  assert_eq!(got, Synced { commits: 1, written: 1, contents_fetched: 1, ..Synced::default() });
  assert_eq!(ann.read("pkg/core/a.ts").unwrap(), "export const a = 10;\n");
  let got = cat.sync().unwrap();
  assert_eq!((got.commits, got.written, got.contents_fetched), (2, 2, 2));
  assert_eq!(cat.read("pkg/ui/b.ts").unwrap(), "export const b = 20;\n");
  assert_eq!(ann.ws.head, cat.ws.head);
  assert_eq!(ann.ws.head, bob.ws.head);

  // Ann and Bob both commit from the same tip. Ann arrives first; Bob does not look before pushing.
  ann.write("docs/guide.md", "a longer guide\n");
  ann.write("docs/old.md", "to be removed\n");
  ann.commit("ann: docs");
  ann.push().unwrap();
  bob.write("pkg/core/extra.ts", "export const e = 5;\n");
  bob.commit("bob: extra");
  let pushed = bob.push().unwrap();
  assert_eq!((pushed.updated, pushed.rebased), (0, 1), "the server put bob's changes on ann's commit");
  assert_eq!(pushed.synced.outside_view, 2, "ann's two docs are not bob's to fetch");
  assert_eq!(bob.read("pkg/core/extra.ts").unwrap(), "export const e = 5;\n");

  // Work that is recorded but not published rides along through a sync.
  ann.push().unwrap();
  ann.write("docs/old.md", "still being written\n");
  cat.write("pkg/ui/b.ts", "export const b = 21;\n");
  cat.commit("cat: ui again");
  cat.push().unwrap();
  let got = ann.sync().unwrap();
  assert_eq!((got.written, got.removed), (1, 0));
  assert_eq!(ann.read("docs/old.md").unwrap(), "still being written\n");
  assert_eq!(ann.read("pkg/ui/b.ts").unwrap(), "export const b = 21;\n");
  assert_ne!(ann.ws.head, ann.ws.base, "her unpublished edit is a snapshot on the new tip");
  ann.commit("ann: old doc");
  ann.push().unwrap();

  // The same file changed on both sides: the later push is refused, and says where.
  cat.sync().unwrap();
  ann.write("README.md", "# ann's readme\n");
  ann.commit("ann: readme");
  ann.push().unwrap();
  cat.write("README.md", "# cat's readme\n");
  std::fs::remove_file(cat.dir.join("docs/old.md")).unwrap();
  let before = cat.commit("cat: readme");
  let refused = cat.push().unwrap_err();
  assert!(refused.contains("README.md") && !refused.contains("docs/old.md"), "{refused}");
  assert_eq!(cat.ws.base, Some(before), "nothing of cat's workspace moved");
  assert_eq!(cat.read("README.md").unwrap(), "# cat's readme\n");
  // Syncing brings ann's in and leaves the one file both rewrote with both versions in it; cat's removal stays cat's.
  let left = cat.sync().unwrap();
  assert_eq!(left.conflicts, vec!["README.md".to_string()], "{left:?}");
  assert!(cat.read("README.md").unwrap().contains("<<<<<<< yours\n# cat's readme\n=======\n# ann's readme\n>>>>>>> theirs"));
  assert!(cat.read("docs/old.md").is_none());

  // Everyone who is caught up has the same commit, and Git takes it.
  bob.sync().unwrap();
  assert_eq!(ann.ws.head, bob.ws.head);
  let tip = ann.ws.head.unwrap();
  git::export(&mut ann.store, &tip, &root.join("repo"), "refs/heads/shared").unwrap();
  let changed = sh(&root.join("repo"), "git diff --name-status main shared");
  assert_eq!(changed, "M\tREADME.md\nM\tdocs/guide.md\nA\tdocs/old.md\nM\tpkg/core/a.ts\nA\tpkg/core/extra.ts\nM\tpkg/ui/b.ts\n");
  assert_eq!(sh(&root.join("repo"), "git rev-list --count main..shared").trim(), "7");
  sh(&root.join("repo"), "git fsck --strict --no-dangling");
  // Bob's store, which never held a file outside pkg/core that changed, agrees on the Git id of the tip's commit chain start.
  assert_eq!(bob.read("README.md"), None);
}

#[test]
fn writers_who_all_start_the_ref_at_once_all_get_in() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-race");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(root.join("repo")).unwrap();
  sh(
    &root.join("repo"),
    "git init -q -b main . && for i in 0 1 2 3 4 5; do echo $i > f$i.txt; done && git add -A && git commit -q -m first",
  );
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/remote-race-{unique}", server.trim_end_matches('/'));
  // Six workspaces, each with a commit, none of which has seen a ref: there is none yet.
  let mut peers: Vec<Peer> = (0..6).map(|i| Peer::new(&root, &format!("p{i}"), &[], &url)).collect();
  for (i, peer) in peers.iter_mut().enumerate() {
    peer.write(&format!("f{i}.txt"), "changed\n");
    peer.commit(&format!("p{i}"));
  }
  let results: Vec<Result<remote::Pushed, String>> = std::thread::scope(|scope| {
    let handles: Vec<_> = peers.iter_mut().map(|peer| scope.spawn(move || peer.push())).collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
  });
  let pushed: Vec<remote::Pushed> = results.into_iter().map(|r| r.expect("no two touched the same path")).collect();
  // One started the ref; the rest were put on top by the server.
  assert_eq!(pushed.iter().map(|p| p.updated).sum::<usize>(), 1);
  assert_eq!(pushed.iter().map(|p| p.rebased).sum::<usize>(), 5);
  // And each can go on from there.
  for (i, peer) in peers.iter_mut().enumerate() {
    peer.sync().unwrap();
    peer.write(&format!("f{i}.txt"), "changed again\n");
    peer.commit("again");
    peer.push().unwrap();
  }
  for peer in peers.iter_mut() {
    peer.sync().unwrap();
  }
  let head = peers[0].ws.head;
  assert!(peers.iter().all(|p| p.ws.head == head));
  for i in 0..6 {
    assert_eq!(peers[3].read(&format!("f{i}.txt")).unwrap(), "changed again\n");
  }
}

#[test]
fn a_large_file_goes_to_the_bucket_by_itself_and_comes_back_whole() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-large");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(root.join("repo")).unwrap();
  sh(&root.join("repo"), "git init -q -b main . && echo readme > README.md && git add -A && git commit -q -m first");
  let url = format!("{server}/t/large-{}", std::process::id());
  let (mut writer, mut reader) = (Peer::new(&root, "writer", &[], &url), Peer::new(&root, "reader", &[], &url));
  writer.commit_empty_is_none();
  // Fifty small files and one of three megabytes that does not compress.
  for i in 0..50 {
    writer.write(&format!("src/f{i}.txt"), &format!("file {i}\n"));
  }
  let mut big = Vec::with_capacity(3 << 20);
  let mut state = 0x9E37_79B9_7F4A_7C15u64;
  while big.len() < 3 << 20 {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    big.extend_from_slice(&state.to_le_bytes());
  }
  std::fs::write(writer.dir.join("model.bin"), &big).unwrap();
  writer.commit("fifty small files and a large one");
  let pushed = writer.push().unwrap();
  assert_eq!((pushed.contents_sent, pushed.large_sent), (51, 1), "the large file went by itself");
  // The repository has fifty-one contents in two objects of the bucket: one pack, one large file.
  let storage: serde_json::Value = serde_json::from_str(&sh(&root, &format!("curl -s {url}/storage/now"))).unwrap();
  assert_eq!(
    (storage["contents"].as_u64(), storage["packs"].as_u64(), storage["direct"].as_u64()),
    (Some(51), Some(1), Some(1)),
    "{storage}"
  );
  let synced = reader.sync().unwrap();
  assert_eq!((synced.contents_fetched, synced.large_fetched), (51, 1));
  assert_eq!(std::fs::read(reader.dir.join("model.bin")).unwrap(), big);
  assert_eq!(reader.read("src/f49.txt").as_deref(), Some("file 49\n"));
}

#[test]
fn a_linked_directory_is_pushed_as_one_change_and_its_files_are_not_sent_again() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let server = server.trim_end_matches('/');
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-link");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(root.join("repo")).unwrap();
  sh(&root.join("repo"), "git init -q -b main . && echo readme > README.md && git add -A && git commit -q -m first");
  let unique =
    format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
  let (lib_name, app_url) = (format!("/t/lib-{unique}"), format!("{server}/t/app-{unique}"));

  // A library: thirty files and one large one, on the server under its own name.
  let mut one = Peer::new(&root, "one", &[], &format!("{server}{lib_name}"));
  one.commit_empty_is_none();
  for i in 0..30 {
    one.write(&format!("lib/src/f{i}.rs"), &format!("pub fn f{i}() {{}}\n"));
  }
  let big: Vec<u8> = (0..(3u32 << 19)).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
  std::fs::write(one.dir.join("lib/src/data.bin"), &big).unwrap();
  let lib_commit = one.commit("the library");
  assert_eq!(one.push().unwrap().contents_sent, 31);
  newvcs::links::name(&one.store, &lib_name, lib_commit).unwrap();

  // An application in the same store links the library's directory, and pushes to its own repository.
  let head = one.store.resolve(sh(&root.join("repo"), "git rev-parse HEAD").trim()).unwrap();
  workspace::checkout(&mut one.store, head, &root.join("ws-app"), vec![]).unwrap();
  let mut app = Workspace::open(&root.join("ws-app")).unwrap();
  app.set_remote(&app_url, "main").unwrap();
  assert_eq!(newvcs::links::link(&mut one.store, &mut app, "vendor/lib", &lib_name, "lib/src", AUTHOR).unwrap(), 31);
  workspace::commit(&mut one.store, &mut app, AUTHOR, "link the library", Look::Everywhere).unwrap().unwrap();
  let pushed = remote::push(&mut one.store, &mut app, AUTHOR).unwrap();
  // Only the file that lists the links was sent. The library's files changed hands on the server.
  assert_eq!((pushed.contents_sent, pushed.large_sent, pushed.adopted), (1, 0, 31), "{pushed:?}");

  // Someone who has never seen the library gets the directory, large file and all.
  let mut two = Peer::new(&root, "two", &[], &app_url);
  let synced = two.sync().unwrap();
  assert_eq!((synced.written, synced.contents_fetched, synced.large_fetched), (32, 32, 1), "{synced:?}");
  assert_eq!(two.read("vendor/lib/f7.rs").as_deref(), Some("pub fn f7() {}\n"));
  assert_eq!(std::fs::read(two.dir.join("vendor/lib/data.bin")).unwrap(), big);

  // A change inside the directory is an ordinary change, and comes back to the one who linked it.
  two.write("vendor/lib/f7.rs", "pub fn f7() { changed() }\n");
  two.commit("change the library here");
  two.push().unwrap();
  remote::sync(&mut one.store, &mut app, AUTHOR).unwrap();
  assert_eq!(std::fs::read_to_string(root.join("ws-app/vendor/lib/f7.rs")).unwrap(), "pub fn f7() { changed() }\n");

  // What the library's repository on the server never had is sent by the writer: here, a file
  // that came with the import and was never pushed.
  assert_eq!(newvcs::links::link(&mut one.store, &mut app, "third/all", &lib_name, "", AUTHOR).unwrap(), 32);
  workspace::commit(&mut one.store, &mut app, AUTHOR, "link all of it", Look::Everywhere).unwrap().unwrap();
  let pushed = remote::push(&mut one.store, &mut app, AUTHOR).unwrap();
  assert_eq!((pushed.contents_sent, pushed.adopted), (2, 0), "{pushed:?}");
  two.sync().unwrap();
  assert_eq!(two.read("third/all/README.md").as_deref(), Some("readme\n"));
  assert_eq!(two.read("third/all/lib/src/f29.rs").as_deref(), Some("pub fn f29() {}\n"));
}

/// A server, a repository with `shared.txt` of twenty lines, and peers of it on a new ref.
fn shared_file(name: &str) -> Option<(PathBuf, String)> {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return None;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("remote-{name}"));
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(root.join("repo")).unwrap();
  sh(
    &root.join("repo"),
    "git init -q -b main . && for i in $(seq 1 20); do echo \"line $i\"; done > shared.txt && echo one > a.txt && git add -A && git commit -q -m first",
  );
  let unique =
    format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
  Some((root, format!("{}/t/{name}-{unique}", server.trim_end_matches('/'))))
}

fn replace_line(peer: &Peer, path: &str, line: usize, with: &str) {
  let text = peer.read(path).unwrap();
  let lines: Vec<String> = text.lines().enumerate().map(|(i, l)| if i + 1 == line { with.to_string() } else { l.to_string() }).collect();
  peer.write(path, &(lines.join("\n") + "\n"));
}

#[test]
fn a_push_of_several_commits_cannot_write_over_what_landed_meanwhile() {
  let Some((root, url)) = shared_file("lost-update") else { return };
  let (mut ann, mut bob, mut cat) =
    (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url), Peer::new(&root, "cat", &[], &url));
  ann.write("a.txt", "one\ninit\n");
  ann.commit("start");
  ann.push().unwrap();
  bob.sync().unwrap();
  // Ann changes a line near the top. Bob, who has not seen that, makes two commits; the second changes a line near the bottom.
  replace_line(&ann, "shared.txt", 2, "ANN IMPORTANT");
  ann.commit("ann: shared");
  ann.push().unwrap();
  bob.write("b.txt", "b\n");
  bob.commit("bob: b");
  replace_line(&bob, "shared.txt", 19, "BOB");
  bob.commit("bob: shared");
  let refused = bob.push().unwrap_err();
  assert!(refused.contains("shared.txt") && refused.contains("nv sync"), "{refused}");
  // The ref has not moved, and nothing of Bob's is gone.
  cat.sync().unwrap();
  assert!(cat.read("shared.txt").unwrap().contains("ANN IMPORTANT") && !cat.read("shared.txt").unwrap().contains("BOB"));
  assert!(cat.read("b.txt").is_none());
  // Syncing merges the two, and then the push goes through.
  let synced = bob.sync().unwrap();
  assert_eq!((synced.merged, synced.conflicts.len()), (1, 0), "{synced:?}");
  assert!(synced.before.is_some());
  let merged = bob.read("shared.txt").unwrap();
  assert!(merged.contains("ANN IMPORTANT") && merged.contains("BOB"), "{merged}");
  // The sync can be undone, and then it really is undone: the files, with a file that was never
  // recorded among them, and what the workspace believes of the ref. Its push is refused as before,
  // and nothing of ann's is undone by a commit made here.
  bob.write("wip.txt", "not recorded\n");
  workspace::restore_before_sync(&mut bob.store, &mut bob.ws, AUTHOR, Look::Everywhere).unwrap();
  assert!(!bob.read("shared.txt").unwrap().contains("ANN IMPORTANT") && bob.read("shared.txt").unwrap().contains("BOB"));
  bob.write("b.txt", "b again\n");
  bob.commit("bob continues");
  assert!(bob.push().unwrap_err().contains("shared.txt"));
  cat.sync().unwrap();
  assert!(cat.read("shared.txt").unwrap().contains("ANN IMPORTANT"), "ann's change is still everyone's");
  let synced = bob.sync().unwrap();
  assert_eq!((synced.merged, synced.conflicts.len()), (1, 0), "{synced:?}");
  let merged = bob.read("shared.txt").unwrap();
  assert!(merged.contains("ANN IMPORTANT") && merged.contains("BOB"), "{merged}");
  bob.push().unwrap();
  cat.sync().unwrap();
  assert_eq!(cat.read("shared.txt").unwrap(), merged);
  assert_eq!(cat.read("b.txt").as_deref(), Some("b again\n"));
  ann.sync().unwrap();
  assert_eq!(ann.read("shared.txt").unwrap(), merged);
}

#[test]
fn the_same_lines_changed_on_both_sides_are_left_for_a_person_with_both_versions() {
  let Some((root, url)) = shared_file("conflict") else { return };
  let (mut ann, mut bob, mut cat) =
    (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url), Peer::new(&root, "cat", &[], &url));
  replace_line(&ann, "shared.txt", 5, "ann's five");
  ann.commit("ann");
  ann.push().unwrap();
  bob.sync().unwrap();
  replace_line(&ann, "shared.txt", 10, "ann's ten");
  ann.commit("ann again");
  ann.push().unwrap();
  replace_line(&bob, "shared.txt", 10, "bob's ten");
  bob.write("other.txt", "other\n");
  bob.commit("bob");
  assert!(bob.push().is_err());
  let synced = bob.sync().unwrap();
  assert_eq!((synced.merged, synced.conflicts.clone()), (0, vec!["shared.txt".to_string()]), "{synced:?}");
  let text = bob.read("shared.txt").unwrap();
  assert!(text.contains("<<<<<<< yours\nbob's ten\n=======\nann's ten\n>>>>>>> theirs\n"), "{text}");
  // Until someone settles it, it is known as unsettled, and a commit by the command is refused.
  assert_eq!(workspace::unsettled(&bob.ws), vec!["shared.txt".to_string()]);
  let nv = env!("CARGO_BIN_EXE_nv");
  let refused = Command::new(nv).args(["commit", bob.dir.to_str().unwrap(), "too soon"]).output().unwrap();
  assert!(!refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains("shared.txt"));
  // Bob's other file is still his commit. He settles the file, commits and pushes.
  assert_eq!(bob.read("other.txt").as_deref(), Some("other\n"));
  replace_line(&bob, "shared.txt", 10, "REMOVE");
  let settled: String = bob
    .read("shared.txt")
    .unwrap()
    .lines()
    .filter(|l| !l.starts_with("=======") && !l.starts_with(">>>>>>>") && *l != "REMOVE" && *l != "ann's ten")
    .map(|l| format!("{l}\n"))
    .collect();
  bob.write("shared.txt", &settled);
  bob.commit("settle");
  bob.push().unwrap();
  cat.sync().unwrap();
  assert_eq!(cat.read("shared.txt").unwrap(), settled);
  assert!(settled.contains("bob's ten") && settled.contains("ann's five") && !settled.contains("<<<<<<<"), "{settled}");
  assert_eq!(cat.read("other.txt").as_deref(), Some("other\n"));
  // Bob's files as they were before the sync are not somewhere to go back to by themselves: on top
  // of what the workspace is now on, they would be ann's work undone, and the next commit would say so.
  let refused = workspace::restore_to(&mut bob.store, &mut bob.ws, synced.before.unwrap(), AUTHOR, Look::Everywhere).unwrap_err();
  assert!(refused.contains("before-sync"), "{refused}");
  assert!(bob.read("shared.txt").unwrap().contains("ann's five"));
}

#[test]
fn a_ref_started_with_several_commits_has_them_all() {
  let Some((root, url)) = shared_file("start-many") else { return };
  let (mut ann, mut bob) = (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url));
  ann.write("c1.txt", "c1\n");
  ann.commit("c1");
  ann.write("c2.txt", "c2\n");
  ann.commit("c2");
  let pushed = ann.push().unwrap();
  assert_eq!((pushed.updated, pushed.contents_sent), (2, 2), "{pushed:?}");
  let synced = bob.sync().unwrap();
  assert_eq!((synced.commits, synced.written), (2, 2), "{synced:?}");
  assert_eq!((bob.read("c1.txt").as_deref(), bob.read("c2.txt").as_deref()), (Some("c1\n"), Some("c2\n")));
  // Two versions of one file in two commits, synced in one go: the store has the later one only.
  ann.write("c1.txt", "c1 second\n");
  ann.commit("c1 again");
  ann.write("c1.txt", "c1 third\n");
  ann.commit("c1 once more");
  ann.push().unwrap();
  bob.sync().unwrap();
  // Writing the history into Git needs the one in between, and fetches it.
  assert_eq!(remote::fetch_for_export(&mut bob.store, &bob.ws).unwrap(), 1);
  let tip = bob.ws.base.unwrap();
  git::export(&mut bob.store, &tip, &root.join("repo"), "refs/heads/from-bob").unwrap();
  sh(&root.join("repo"), "git fsck --strict --no-dangling");
  assert_eq!(
    sh(&root.join("repo"), "git show from-bob~1:c1.txt && git show from-bob:c1.txt && git rev-list --count main..from-bob"),
    "c1 second\nc1 third\n4\n"
  );
  // And what the second writer then does lands on it.
  bob.write("c3.txt", "c3\n");
  bob.commit("c3");
  bob.push().unwrap();
  ann.sync().unwrap();
  assert_eq!(ann.read("c3.txt").as_deref(), Some("c3\n"));
}

#[test]
fn a_push_or_sync_killed_at_any_moment_is_finished_by_the_next() {
  let Some((root, url)) = shared_file("killed") else { return };
  let (mut one, mut two) = (Peer::new(&root, "one", &[], &url), Peer::new(&root, "two", &[], &url));
  one.write("one-0.txt", "0\n");
  one.commit("start");
  one.push().unwrap();
  two.sync().unwrap();
  let nv = env!("CARGO_BIN_EXE_nv");
  let (dir_one, dir_two) = (one.dir.clone(), two.dir.clone());
  drop((one, two));
  let run = |script: &str| {
    let out = Command::new("sh").arg("-c").arg(script).env("NV", nv).env("ONE", &dir_one).env("TWO", &dir_two).output().unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
  };
  // The ref moves under the first workspace each round, so each of its pushes has to catch up; and each is
  // killed somewhere on the way. Whatever was left, the next push or sync has to get through.
  for round in 1..=40 {
    let (fine, said) = run(&format!(
      "echo {round} > $TWO/two-{round}.txt && echo {round} >> $TWO/shared.txt && $NV commit $TWO two-{round} && $NV push $TWO"
    ));
    assert!(fine, "round {round}, the other writer: {said}");
    let (_, _) = run(&format!(
      "echo {round} > $ONE/one-{round}.txt && $NV commit $ONE one-{round} >/dev/null; $NV push $ONE >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,0.002+rand(0.05))'; kill -9 $pid 2>/dev/null; wait $pid 2>/dev/null; true"
    ));
    let (fine, said) = run("$NV push $ONE || ($NV sync $ONE && $NV push $ONE)");
    assert!(fine, "round {round}, after the kill: {said}");
    let (fine, said) = run("$NV sync $TWO");
    assert!(fine, "round {round}, the other writer catching up: {said}");
  }
  let (_, listing) = run("ls $ONE | grep -c . ; ls $TWO | grep -c . ; cmp $ONE/shared.txt $TWO/shared.txt && echo same");
  assert_eq!(listing, "83\n83\nsame\n", "both have forty files of each writer, the three they began with, and the same shared file");
  let (fine, said) = run("$NV status $ONE 2>/dev/null; $NV status $TWO 2>/dev/null");
  assert!(fine && said.is_empty(), "nothing is left as a difference: {said}");
}
