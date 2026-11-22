//! Three workspaces, three stores, one server. Needs the server running:
//!
//!   (cd server && pnpm dev -- --port 8799) &
//!   NV_SERVER=http://localhost:8799 cargo test --test remote
//!
//! Without `NV_SERVER` the test says so and passes.

mod common;

use newvcs::remote::{self, Synced};
use newvcs::store::Store;
use newvcs::workspace::{self, Look, Workspace};
use newvcs::{Id, git};
use std::path::{Path, PathBuf};
use std::process::Command;

const AUTHOR: &str = "t <t@example.com>";

fn sh(dir: &Path, script: &str) -> String {
  let out = common::bounded(
    Command::new("sh")
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
      .env("GIT_CONFIG_SYSTEM", "/dev/null"),
  );
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
  // While it is unsettled nothing moves: a second sync would forget that the file holds both
  // versions, and the next commit and push would publish them. Someone else pushing meanwhile changes nothing.
  cat.sync().unwrap();
  cat.write("unrelated.txt", "cat\n");
  cat.commit("cat, meanwhile");
  cat.push().unwrap();
  for refused in [bob.sync().map(|_| ()), bob.push().map(|_| ())] {
    assert!(refused.unwrap_err().contains("still hold both versions"), "a sync or a push went ahead over an unsettled file");
  }
  assert!(bob.read("unrelated.txt").is_none());
  // One marker left behind is still a file not settled.
  let half: String = text.lines().filter(|l| !l.starts_with("<<<<<<<") && !l.starts_with("=======")).map(|l| format!("{l}\n")).collect();
  bob.write("shared.txt", &half);
  assert_eq!(workspace::unsettled(&bob.ws), vec!["shared.txt".to_string()]);
  bob.write("shared.txt", &text);
  // Until someone settles it, it is known as unsettled, and a commit by the command is refused.
  assert_eq!(workspace::unsettled(&bob.ws), vec!["shared.txt".to_string()]);
  let nv = env!("CARGO_BIN_EXE_nv");
  let refused = common::bounded(Command::new(nv).args(["commit", bob.dir.to_str().unwrap(), "too soon"]));
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
    let mut command = Command::new("sh");
    command.arg("-c").arg(script).env("NV", nv).env("ONE", &dir_one).env("TWO", &dir_two);
    command.env("GIT_AUTHOR_NAME", "a").env("GIT_AUTHOR_EMAIL", "a@example.com").env("GIT_CONFIG_GLOBAL", "/dev/null");
    let out = common::bounded(command.env("GIT_COMMITTER_NAME", "c").env("GIT_COMMITTER_EMAIL", "c@example.com"));
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

#[test]
fn a_merge_keeps_what_each_side_did_to_a_files_mode() {
  let Some((root, url)) = shared_file("mode") else { return };
  let (mut ann, mut bob, mut cat) =
    (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url), Peer::new(&root, "cat", &[], &url));
  ann.write("a.txt", "one\nstart\n");
  ann.commit("start");
  ann.push().unwrap();
  bob.sync().unwrap();
  // Ann makes the file one to run; Bob, who has not seen that, changes a line of it.
  sh(&ann.dir, "chmod +x shared.txt");
  ann.commit("ann: executable");
  ann.push().unwrap();
  replace_line(&bob, "shared.txt", 15, "bob's line");
  bob.commit("bob: a line");
  assert!(bob.push().is_err());
  assert_eq!(bob.sync().unwrap().merged, 1);
  bob.push().unwrap();
  for peer in [&mut ann, &mut cat] {
    peer.sync().unwrap();
    assert_eq!(sh(&peer.dir, "test -x shared.txt && grep -c \"bob's line\" shared.txt").trim(), "1", "both the mode and the line");
  }
  // Where the text has to be settled by a person, the mode does not: it is on the file already.
  sh(&ann.dir, "chmod -x shared.txt");
  replace_line(&ann, "shared.txt", 8, "ann's eight");
  ann.commit("ann: not executable, and a line");
  ann.push().unwrap();
  replace_line(&bob, "shared.txt", 8, "bob's eight");
  bob.commit("bob: the same line");
  assert!(bob.push().is_err());
  assert_eq!(bob.sync().unwrap().conflicts, vec!["shared.txt".to_string()]);
  assert_eq!(sh(&bob.dir, "test -x shared.txt && echo runs || echo plain").trim(), "plain");
  let settled: String = bob
    .read("shared.txt")
    .unwrap()
    .lines()
    .filter(|l| !l.starts_with("<<<<<<<") && !l.starts_with("=======") && !l.starts_with(">>>>>>>") && *l != "ann's eight")
    .map(|l| format!("{l}\n"))
    .collect();
  bob.write("shared.txt", &settled);
  bob.commit("settled");
  bob.push().unwrap();
  cat.sync().unwrap();
  assert_eq!(sh(&cat.dir, "test -x shared.txt && echo runs || echo plain; grep -c \"bob's eight\" shared.txt").trim(), "plain\n1");
  sh(&cat.dir, "chmod +x shared.txt");
  cat.commit("cat: executable again");
  cat.push().unwrap();
  for peer in [&mut ann, &mut bob] {
    peer.sync().unwrap();
  }
  // The other way about: the one who syncs changed the mode, the other the text.
  sh(&cat.dir, "chmod -x shared.txt");
  cat.commit("cat: not executable");
  replace_line(&ann, "shared.txt", 3, "ann's line");
  ann.commit("ann: a line");
  ann.push().unwrap();
  assert!(cat.push().is_err());
  cat.sync().unwrap();
  cat.push().unwrap();
  bob.sync().unwrap();
  assert_eq!(sh(&bob.dir, "test -x shared.txt || grep -c \"ann's line\" shared.txt").trim(), "1");
}

/// The messages of the ref's commits as a fresh workspace finds them, newest first.
fn history(peer: &mut Peer) -> Vec<String> {
  let mut out = Vec::new();
  let mut at = peer.ws.pushed;
  while let Some(id) = at {
    let commit = newvcs::commit::Commit::load(&mut peer.store, &id).unwrap();
    out.push(String::from_utf8_lossy(&commit.rest).lines().last().unwrap_or_default().to_string());
    at = commit.parents.first().copied();
  }
  out
}

#[test]
fn a_push_leaves_what_was_edited_after_the_commit_as_it_is() {
  let Some((root, url)) = shared_file("after-commit") else { return };
  let mut ann = Peer::new(&root, "ann", &[], &url);
  ann.write("seed.txt", "s\n");
  ann.commit("start");
  ann.push().unwrap();
  // Committed, then edited again, then pushed: the edit is not a conflict with one's own commit.
  ann.write("f.txt", "v1\n");
  ann.commit("c1");
  ann.write("f.txt", "v2\n");
  let pushed = ann.push().unwrap();
  assert!(pushed.synced.conflicts.is_empty(), "{pushed:?}");
  assert_eq!(ann.read("f.txt").as_deref(), Some("v2\n"));
  assert_eq!(workspace::status(&ann.ws, Look::Everywhere).unwrap().0.len(), 0, "the edit is recorded, on top of the commit");
  assert_ne!(ann.ws.head, ann.ws.base);
  // Committed, then removed, then pushed: the file stays removed, and nothing is pushed twice.
  ann.commit("c1 again as v2");
  ann.push().unwrap();
  ann.write("g.txt", "g\n");
  ann.commit("c2");
  std::fs::remove_file(ann.dir.join("g.txt")).unwrap();
  ann.push().unwrap();
  assert_eq!(ann.read("g.txt"), None);
  let again = ann.push().unwrap();
  assert_eq!((again.updated, again.rebased), (0, 0), "{again:?}");
  // The same with bytes that are not text: the push is not reported refused when it landed.
  ann.commit("g removed");
  ann.push().unwrap();
  std::fs::write(ann.dir.join("x.bin"), [0u8, 1, 2, 3]).unwrap();
  ann.commit("bin");
  std::fs::write(ann.dir.join("x.bin"), [0u8, 9, 9, 9]).unwrap();
  ann.push().unwrap();
  assert_eq!(std::fs::read(ann.dir.join("x.bin")).unwrap(), [0u8, 9, 9, 9]);
  ann.commit("bin 2");
  ann.push().unwrap();
  let mut fresh = Peer::new(&root, "fresh", &[], &url);
  fresh.sync().unwrap();
  assert_eq!(std::fs::read(fresh.dir.join("x.bin")).unwrap(), [0u8, 9, 9, 9]);
  assert_eq!(fresh.read("f.txt").as_deref(), Some("v2\n"));
  assert_eq!(fresh.read("g.txt"), None);
  assert_eq!(history(&mut fresh)[..7], ["bin 2", "bin", "g removed", "c2", "c1 again as v2", "c1", "start"]);
}

#[test]
fn a_workspace_moved_back_does_not_undo_what_others_pushed() {
  let Some((root, url)) = shared_file("moved-back") else { return };
  let (mut ann, mut bob) = (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url));
  ann.write("one.txt", "one\n");
  ann.commit("a: one");
  ann.push().unwrap();
  bob.sync().unwrap();
  let at_one = bob.ws.head.unwrap();
  ann.write("two.txt", "two\n");
  ann.commit("a: two");
  ann.push().unwrap();
  bob.sync().unwrap();
  // Bob goes back a commit, as `git reset --hard HEAD~1` does.
  workspace::switch(&mut bob.store, &mut bob.ws, at_one).unwrap();
  assert_eq!(bob.read("two.txt"), None);
  ann.write("three.txt", "three\n");
  ann.commit("a: three");
  ann.push().unwrap();
  // Catching up brings back what the ref has; Bob's own commit then goes on top, and removes nothing.
  bob.sync().unwrap();
  assert_eq!(bob.read("two.txt").as_deref(), Some("two\n"));
  bob.write("mine.txt", "mine\n");
  bob.commit("b: mine");
  bob.push().unwrap();
  ann.sync().unwrap();
  assert_eq!((ann.read("two.txt").as_deref(), ann.read("mine.txt").as_deref()), (Some("two\n"), Some("mine\n")));
  assert_eq!(history(&mut ann)[..4], ["b: mine", "a: three", "a: two", "a: one"]);
}

#[test]
fn work_that_cannot_go_on_top_is_set_aside_and_the_workspace_goes_on() {
  let Some((root, url)) = shared_file("set-aside") else { return };
  let (mut ann, mut bob) = (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url));
  ann.write("f.txt", "1\n2\n3\n");
  ann.commit("add f");
  ann.push().unwrap();
  bob.sync().unwrap();
  // Bob removes the file and Ann changes it: not something to merge as text.
  std::fs::remove_file(bob.dir.join("f.txt")).unwrap();
  bob.write("other.txt", "bob\n");
  bob.commit("b");
  ann.write("f.txt", "1\n2\n3\n4\n");
  ann.commit("a");
  ann.push().unwrap();
  assert!(bob.push().is_err());
  let refused = bob.sync().unwrap_err();
  assert!(refused.contains("nv restore <dir> pushed"), "{refused}");
  // Setting the work aside is the way out the message names, and it works.
  let aside = workspace::set_aside(&mut bob.store, &mut bob.ws, AUTHOR, Look::Everywhere).unwrap();
  let kept = aside.kept.expect("the work is kept under an id");
  assert_eq!((bob.read("f.txt").as_deref(), bob.read("other.txt")), (Some("1\n2\n3\n"), None));
  bob.sync().unwrap();
  assert_eq!(bob.read("f.txt").as_deref(), Some("1\n2\n3\n4\n"));
  // What was set aside can still be read.
  let listed = workspace::since(&mut bob.store, &bob.ws, kept, Look::Everywhere).unwrap();
  assert!(listed.iter().any(|(path, _)| path == b"other.txt"), "{listed:?}");
  bob.write("other.txt", "bob\n");
  bob.commit("b again");
  bob.push().unwrap();
}

#[test]
fn a_change_of_mind_after_a_commit_survives_the_commit_moving() {
  let Some((root, url)) = shared_file("change-of-mind") else { return };
  let (mut ann, mut bob) = (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url));
  ann.write("p1", "base1\n");
  ann.write("gone", "g\n");
  ann.commit("base");
  ann.push().unwrap();
  bob.sync().unwrap();
  bob.write("bx", "x\n");
  bob.commit("bx");
  bob.push().unwrap();
  // Committed, then put back as it was and not committed; a file added and then removed again.
  ann.write("p1", "n\n");
  ann.write("added", "a\n");
  std::fs::remove_file(ann.dir.join("gone")).unwrap();
  ann.commit("n");
  ann.write("p1", "base1\n");
  std::fs::remove_file(ann.dir.join("added")).unwrap();
  ann.write("gone", "g\n");
  ann.sync().unwrap();
  // The files are as Ann left them, and nv knows they differ from the commit.
  assert_eq!((ann.read("p1").as_deref(), ann.read("added"), ann.read("gone").as_deref()), (Some("base1\n"), None, Some("g\n")));
  let base = ann.ws.base.unwrap();
  let pending: Vec<String> = workspace::since(&mut ann.store, &ann.ws, base, Look::Everywhere)
    .unwrap()
    .into_iter()
    .map(|(path, what)| format!("{what} {}", String::from_utf8_lossy(&path)))
    .collect();
  assert_eq!(pending, ["D added", "A gone", "M p1"]);
  ann.push().unwrap();
  ann.commit("back");
  ann.push().unwrap();
  bob.sync().unwrap();
  assert_eq!((bob.read("p1").as_deref(), bob.read("added"), bob.read("gone").as_deref()), (Some("base1\n"), None, Some("g\n")));
}

#[test]
fn a_branch_goes_where_a_sync_or_a_push_takes_its_workspace() {
  let Some((root, url)) = shared_file("branch-along") else { return };
  let mut bob = Peer::new(&root, "bob", &[], &url);
  bob.write("start", "s\n");
  bob.commit("start");
  bob.push().unwrap();
  let nv = env!("CARGO_BIN_EXE_nv");
  let g = root.join("g");
  let run = |script: &str| sh(&root, &format!("NV={nv}; cd g 2>/dev/null || true; {script}"));
  sh(&root, &format!("{nv} git clone -q repo g && {nv} remote g {url} main && {nv} sync g >/dev/null"));
  run(
    "$NV git switch -q -c other && $NV git switch -q main && echo 1 > g1 && $NV git add -A && $NV git commit -qm g1 && $NV push . >/dev/null",
  );
  bob.sync().unwrap();
  bob.write("bB.txt", "b\n");
  bob.commit("bB");
  bob.push().unwrap();
  // This push is put on a newer tip: the commit gets another id, and the branch has to follow it.
  run("echo 2 > g2 && $NV git add -A && $NV git commit -qm g2 && $NV push . >/dev/null");
  run("echo 3 > g3 && $NV git add -A && $NV git commit -qm g3 && echo 4 > g4 && $NV git add -A && $NV git commit -qm g4");
  assert_eq!(run("$NV git rev-parse main; $NV git rev-parse HEAD").lines().collect::<std::collections::HashSet<_>>().len(), 1);
  run("$NV git switch -q other && $NV git switch -q main");
  assert_eq!(run("$NV git log --format=%s | head -3 | tr '\\n' ' '; ls g3 g4 bB.txt | tr '\\n' ' '"), "g4 g3 g2 bB.txt g3 g4 ");
  // `nv git add`, another edit, then `nv commit`: the index is not left saying the file is gone.
  run("echo 7 > f7 && $NV git add f7 && echo 7b > f7 && $NV commit . c1 >/dev/null");
  assert_eq!(run("$NV git status --short; echo 8 > f8; $NV git add -A; $NV git status --short"), "A  f8\n");
  run("$NV git commit -qm c2 && $NV push . >/dev/null");
  bob.sync().unwrap();
  assert_eq!((bob.read("f7").as_deref(), bob.read("f8").as_deref()), (Some("7b\n"), Some("8\n")));
  assert!(g.join("f7").exists());
}

fn committed(peer: &mut Peer, path: &str) -> Option<String> {
  let root = newvcs::commit::root_of(&mut peer.store, &peer.ws.base.unwrap()).unwrap()?;
  let entry = newvcs::manifest::lookup(&mut peer.store, root, path.as_bytes()).unwrap()?;
  Some(String::from_utf8_lossy(&peer.store.get(&entry.id).unwrap().unwrap()).into_owned())
}

fn pending(peer: &mut Peer) -> Vec<String> {
  let base = peer.ws.base.unwrap();
  workspace::since(&mut peer.store, &peer.ws, base, Look::Everywhere)
    .unwrap()
    .into_iter()
    .map(|(path, what)| format!("{what} {}", String::from_utf8_lossy(&path)))
    .collect()
}

#[test]
fn a_sync_merges_what_was_committed_and_keeps_what_was_not_apart() {
  let Some((root, url)) = shared_file("two-layers") else { return };
  let (mut ann, mut bob) = (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url));
  ann.write("start", "s\n");
  ann.commit("start");
  ann.push().unwrap();
  bob.sync().unwrap();
  // Bob changes a line near the bottom of the shared file and pushes.
  replace_line(&bob, "shared.txt", 19, "line 19 bob");
  bob.commit("b 19");
  bob.push().unwrap();
  // Ann commits a change near the top, then changes her mind without committing; and edits further down, uncommitted.
  replace_line(&ann, "shared.txt", 2, "line 2 ann");
  ann.commit("a commits line 2");
  replace_line(&ann, "shared.txt", 2, "line 2");
  replace_line(&ann, "shared.txt", 10, "line 10 not committed");
  let synced = ann.sync().unwrap();
  assert!(synced.conflicts.is_empty(), "{synced:?}");
  // The commit holds what was committed, merged with Bob's; the files hold Ann's later edits on top, as pending changes.
  let in_commit = committed(&mut ann, "shared.txt").unwrap();
  assert!(in_commit.contains("line 2 ann\n") && in_commit.contains("line 19 bob\n") && !in_commit.contains("not committed"), "{in_commit}");
  let on_disk = ann.read("shared.txt").unwrap();
  assert!(on_disk.contains("line 2\n") && on_disk.contains("line 10 not committed\n") && on_disk.contains("line 19 bob\n"), "{on_disk}");
  assert_eq!(pending(&mut ann), ["M shared.txt"]);
  // Pushed, the ref has the commit and not the uncommitted edits.
  ann.push().unwrap();
  bob.sync().unwrap();
  let bobs = bob.read("shared.txt").unwrap();
  assert!(bobs.contains("line 2 ann\n") && !bobs.contains("not committed"), "{bobs}");
  assert_eq!(pending(&mut ann), ["M shared.txt"]);

  // Both made the file executable; Ann also committed an edit and then took it back, uncommitted.
  let exec = |peer: &Peer| {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(peer.dir.join("a.txt"), std::fs::Permissions::from_mode(0o755)).unwrap();
  };
  ann.commit("a: the rest");
  ann.push().unwrap();
  bob.sync().unwrap();
  exec(&bob);
  bob.commit("b +x");
  bob.push().unwrap();
  exec(&ann);
  let before = ann.read("a.txt").unwrap();
  ann.write("a.txt", &format!("{before}ann\n"));
  ann.commit("a: +x and a line");
  ann.write("a.txt", &before);
  ann.sync().unwrap();
  assert_eq!(ann.read("a.txt").unwrap(), before);
  assert_eq!(pending(&mut ann), ["M a.txt"], "the files differ from the commit, and nv knows");

  // Bob removes a file Ann has committed an edit to and then removed without committing: not something to merge.
  ann.commit("a: line taken back");
  ann.push().unwrap();
  bob.sync().unwrap();
  std::fs::remove_file(bob.dir.join("start")).unwrap();
  bob.commit("b removes start");
  bob.push().unwrap();
  ann.write("start", "s\nedited\n");
  ann.commit("a edits start");
  std::fs::remove_file(ann.dir.join("start")).unwrap();
  let refused = ann.sync().unwrap_err();
  assert!(refused.contains("cannot be merged as text") && refused.contains("start"), "{refused}");
  assert_eq!(ann.read("start"), None, "nothing was touched");
}

#[test]
fn what_a_seventh_reviewer_found_in_pushing_stays_fixed() {
  let Some((root, url)) = shared_file("seventh") else { return };
  let (mut ann, mut bob) = (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url));
  // A workspace that looked for the ref before there was one has somewhere to set its work aside to.
  assert_eq!(bob.sync().unwrap(), Synced::default());
  ann.write("a.txt", "one\nann\n");
  ann.commit("a1");
  ann.push().unwrap();
  std::fs::remove_file(bob.dir.join("a.txt")).unwrap();
  bob.commit("b removes");
  assert!(bob.sync().is_err());
  workspace::set_aside(&mut bob.store, &mut bob.ws, AUTHOR, Look::Everywhere).unwrap();
  bob.sync().unwrap();
  assert_eq!(bob.read("a.txt").as_deref(), Some("one\nann\n"));
  // A push that landed says so, though catching up afterwards cannot be done.
  ann.write("shared.txt", "all new\n");
  ann.commit("a rewrites shared");
  ann.push().unwrap();
  bob.write("b.txt", "b\n");
  bob.commit("b adds b.txt");
  std::fs::remove_file(bob.dir.join("shared.txt")).unwrap();
  let said = bob.push().unwrap_err();
  assert!(said.contains("the push landed"), "{said}");
  ann.sync().unwrap();
  assert_eq!(ann.read("b.txt").as_deref(), Some("b\n"));
  bob.write("shared.txt", "all new\n");
  // Sent again, the push gets the answer it got: the commit is on the ref once.
  bob.push().unwrap();
  bob.push().unwrap();
  assert_eq!(history(&mut bob).iter().filter(|said| *said == "b adds b.txt").count(), 1);
  // Undoing a catching-up is not offered once the workspace has committed since.
  ann.write("c.txt", "c\n");
  ann.commit("a adds c");
  ann.push().unwrap();
  bob.write("mine.txt", "m\n");
  bob.sync().unwrap();
  bob.commit("made after the sync");
  let refused = workspace::restore_before_sync(&mut bob.store, &mut bob.ws, AUTHOR, Look::Everywhere).unwrap_err();
  assert!(refused.contains("since it last caught up"), "{refused}");
  assert_eq!(bob.read("mine.txt").as_deref(), Some("m\n"));
}

#[test]
fn a_git_repository_follows_a_sync_and_a_merge_is_not_pushed() {
  let Some((root, url)) = shared_file("adopted-sync") else { return };
  let mut ann = Peer::new(&root, "ann", &[], &url);
  ann.write("start", "s\n");
  ann.commit("start");
  ann.push().unwrap();
  let nv = env!("CARGO_BIN_EXE_nv");
  // A Git repository with nv beside it, on the same ref.
  sh(
    &root,
    &format!(
      "git clone -q repo c && cd c && git config user.name c && git config user.email c@example.com && {nv} adopt . >/dev/null && {nv} remote . {url} main && {nv} sync . >/dev/null"
    ),
  );
  // Two commits to one file arrive at once: Git's history needs both versions of it.
  ann.write("a.txt", "a1\n");
  ann.commit("a1");
  ann.push().unwrap();
  ann.write("a.txt", "a2\n");
  ann.commit("a2");
  ann.push().unwrap();
  assert_eq!(
    sh(
      &root,
      &format!(
        "cd c && {nv} sync . >/dev/null 2>&1; {nv} check . | tail -1; git log --format=%s | head -2 | tr '\\n' ' '; git status --porcelain; cat a.txt"
      )
    ),
    "in step\na2 a1 a2\n"
  );
  // A merge of two local lines of commits is not something a ref can hold: refused, nothing sent.
  sh(&root, &format!("{nv} git clone -q repo g && {nv} remote g {url} main && {nv} sync g >/dev/null"));
  let run = |script: &str| sh(&root, &format!("NV={nv}; cd g; {script}"));
  run(
    "$NV git switch -q -c feat && echo f > bf.txt && $NV git add bf.txt && $NV git commit -qm bf && $NV git switch -q main && echo m > bm.txt && $NV git add bm.txt && $NV git commit -qm bm && $NV git merge -q -m mrg feat",
  );
  assert!(run("$NV push . 2>&1 || true").contains("is a merge"));
  ann.sync().unwrap();
  assert_eq!(ann.read("bf.txt"), None);
}

#[test]
fn what_an_eighth_reviewer_found_in_syncing_stays_fixed() {
  let Some((root, url)) = shared_file("eighth") else { return };
  let (mut ann, mut bob, mut cat) =
    (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url), Peer::new(&root, "cat", &[], &url));
  // Ann looked for the ref before there was one; Bob starts it; Ann's work cannot go on top and is set aside.
  assert_eq!(ann.sync().unwrap(), Synced::default());
  bob.write("shared.txt", "other\n");
  bob.commit("b");
  bob.push().unwrap();
  std::fs::remove_file(ann.dir.join("shared.txt")).unwrap();
  ann.commit("a removes shared");
  assert!(ann.push().is_err() && ann.sync().is_err());
  let pushed = workspace::state_named(&mut ann.store, &ann.ws, "pushed").expect("somewhere to set the work aside to");
  assert_eq!(Some(pushed), ann.ws.start);
  workspace::set_aside(&mut ann.store, &mut ann.ws, AUTHOR, Look::Everywhere).unwrap();
  ann.sync().unwrap();
  assert_eq!(ann.read("shared.txt").as_deref(), Some("other\n"));
  cat.sync().unwrap();

  // A push that landed while catching up could not be done: the next commit stands on it, with no conflict against oneself.
  cat.write("other.txt", "c0\n");
  cat.commit("c: adds other.txt");
  cat.push().unwrap();
  ann.sync().unwrap();
  bob.sync().unwrap();
  cat.write("other.txt", "cat\n");
  cat.commit("c: other.txt");
  cat.push().unwrap();
  bob.write("shared.txt", "other\nv1\n");
  bob.commit("b1: v1");
  // Not committed, and not something to merge with the edit the ref has taken.
  std::fs::remove_file(bob.dir.join("other.txt")).unwrap();
  let said = bob.push().unwrap_err();
  assert!(said.contains("the push landed"), "{said}");
  // The removal is given up, and work goes on.
  let base = bob.ws.base.unwrap();
  workspace::restore_to(&mut bob.store, &mut bob.ws, base, AUTHOR, Look::Everywhere).unwrap();
  bob.write("shared.txt", "other\nv2\n");
  bob.commit("b2: v2");
  let synced = bob.push().or_else(|_| bob.sync().map(|synced| remote::Pushed { synced, ..Default::default() })).unwrap();
  assert!(synced.synced.conflicts.is_empty(), "{synced:?}");
  bob.push().unwrap();
  ann.sync().unwrap();
  assert_eq!((ann.read("shared.txt").as_deref(), ann.read("other.txt").as_deref()), (Some("other\nv2\n"), Some("cat\n")));
  assert_eq!(history(&mut ann)[..3], ["b2: v2", "b1: v1", "c: other.txt"]);

  // A commit that makes the very change the ref already has is not kept as a commit of nothing.
  cat.sync().unwrap();
  cat.write("shared.txt", "other\nsame\n");
  cat.write("more.txt", "m\n");
  cat.commit("c: same, and more");
  cat.push().unwrap();
  ann.write("shared.txt", "other\nsame\n");
  ann.commit("a: the same edit");
  let synced = ann.sync().unwrap();
  assert_eq!(synced.superseded, 1, "{synced:?}");
  assert_eq!(ann.ws.base, ann.ws.pushed);
  let again = ann.push().unwrap();
  assert_eq!((again.updated, again.rebased), (0, 0), "{again:?}");
}

#[test]
fn git_beside_nv_is_never_left_where_its_next_commit_undoes_someone() {
  let Some((root, url)) = shared_file("git-along") else { return };
  let mut ann = Peer::new(&root, "ann", &[], &url);
  ann.write("start", "s\n");
  ann.commit("start");
  ann.push().unwrap();
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      "git clone -q repo c && cd c && git config user.name c && git config user.email c@example.com && {nv} adopt . >/dev/null && {nv} remote . {url} main && {nv} sync . >/dev/null"
    ),
  );
  let c = |script: &str| sh(&root, &format!("NV={nv}; cd c; {script}"));
  // Ann changes a line; the Git user changes the same one, syncs, and undoes the sync.
  replace_line(&ann, "shared.txt", 5, "line 5 ann");
  ann.commit("a5");
  ann.push().unwrap();
  c(
    "sed -i.orig 's/^line 5$/line 5 c/' shared.txt && rm shared.txt.orig && git commit -qam c5 && { $NV sync . >/dev/null 2>&1 || true; } && $NV restore . before-sync >/dev/null",
  );
  // Git is back where nv is, at once: a plain commit now is a commit on c5, not a revert of a5.
  assert_eq!(c("git log -1 --format=%s; git status --porcelain"), "c5\n");
  c(
    "echo typo >> other.txt && git add other.txt && git commit -qm 'fix typo' && ($NV push . >/dev/null 2>&1 || true) && ($NV sync . >/dev/null 2>&1 || true)",
  );
  ann.sync().ok();
  assert!(
    ann.read("shared.txt").unwrap().contains("line 5 ann") || ann.read("shared.txt").unwrap().contains("<<<<<<<"),
    "Ann's line is not silently gone"
  );

  // A sync killed while it brings Git along: the next command finishes it, index and all. (In a
  // Git repository of its own, with no work of its own: what is checked is only that Git follows.)
  sh(
    &root,
    &format!(
      "git clone -q repo d && cd d && git config user.name d && git config user.email d@example.com && {nv} adopt . >/dev/null && {nv} remote . {url} main && {nv} sync . >/dev/null"
    ),
  );
  let c = |script: &str| sh(&root, &format!("NV={nv}; cd d; {script}"));
  for round in 0..40 {
    ann.sync().unwrap();
    ann.write("f1", &format!("f1 {round}\n"));
    ann.write("f2", &format!("f2 {round}\n"));
    ann.commit(&format!("a {round}"));
    ann.push().unwrap();
    let pause = 20 + (round * 7) % 90;
    c(&format!(
      "$NV sync . >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,{pause}/1000)'; kill -9 $pid 2>/dev/null || true; wait $pid 2>/dev/null || true"
    ));
    c("$NV sync . >/dev/null 2>&1 || $NV restore . before-sync >/dev/null 2>&1 || true");
    let state = c("$NV check . | tail -1; git status --porcelain | grep -v '^??' || true");
    assert_eq!(state, "in step\n", "round {round}: Git is not where nv is");
  }
}

#[test]
fn a_push_that_landed_unheard_is_known_for_this_workspaces_own() {
  let Some((root, url)) = shared_file("unheard") else { return };
  let (mut ann, mut bob) = (Peer::new(&root, "ann", &[], &url), Peer::new(&root, "bob", &[], &url));
  ann.write("afile", "A\n");
  ann.commit("a0");
  ann.push().unwrap();
  bob.sync().unwrap();
  bob.write("fb", "v0\n");
  bob.commit("b0");
  bob.push().unwrap();
  ann.sync().unwrap();
  ann.write("afile", "A2\n");
  ann.commit("a1");
  ann.push().unwrap();
  // Bob's push lands on the newer tip, and Bob never hears so (killed): his workspace is as before it.
  bob.write("fb", "v1\n");
  bob.commit("b1");
  let before = std::fs::read(bob.dir.join(".nv/workspace")).unwrap();
  bob.push().unwrap();
  std::fs::write(bob.dir.join(".nv/workspace"), &before).unwrap();
  bob.ws = Workspace::open(&bob.dir).unwrap();
  // Another commit of his own on top, and a sync: no conflict with himself.
  bob.write("fb", "v2\n");
  bob.commit("b2");
  let synced = bob.sync().unwrap();
  assert!(synced.conflicts.is_empty(), "{synced:?}");
  assert_eq!(bob.read("fb").as_deref(), Some("v2\n"));
  bob.push().unwrap();
  ann.sync().unwrap();
  assert_eq!((ann.read("fb").as_deref(), ann.read("afile").as_deref()), (Some("v2\n"), Some("A2\n")));
  let said = history(&mut ann);
  assert_eq!(said.iter().filter(|s| *s == "b1").count(), 1, "{said:?}");
  assert_eq!(said[0], "b2", "{said:?}");
}

#[test]
fn what_a_tenth_reviewer_found_in_pushing_stays_fixed() {
  let Some((root, url)) = shared_file("tenth") else { return };
  let nv = env!("CARGO_BIN_EXE_nv");
  let mut bob = Peer::new(&root, "bob", &[], &url);
  bob.write("start", "s\n");
  bob.commit("start");
  bob.push().unwrap();
  sh(&root, &format!("{nv} git clone -q repo a && {nv} remote a {url} main && {nv} sync a >/dev/null"));
  let a = |script: &str| sh(&root, &format!("NV={nv}; cd a; {script}"));
  // After a look in Git's terms, a command killed in its first moments never makes what is not
  // committed the workspace's commit.
  a("sed -i.orig 's/^line 1$/committed/' shared.txt && rm shared.txt.orig && $NV commit . c1 >/dev/null && $NV push . >/dev/null");
  for round in 0..60 {
    a(&format!(
      "sed -i.orig 's/^line 3.*$/line 3 not committed {round}/' shared.txt && rm shared.txt.orig && $NV mark . m >/dev/null && $NV git status --short >/dev/null"
    ));
    let pause = round % 7;
    a(&format!(
      "$NV push . >/dev/null 2>&1 & p=$!; perl -e 'select(undef,undef,undef,{pause}/1000)'; kill -9 $p 2>/dev/null || true; wait $p 2>/dev/null || true"
    ));
    assert_eq!(a("$NV git log -1 --format=%s"), "c1\n", "round {round}: the last commit is not the one made");
    assert!(a("$NV since . base").contains("shared.txt"), "round {round}: the edit is no longer pending");
  }
  a("$NV push . >/dev/null 2>&1 || true");
  bob.sync().unwrap();
  assert!(!bob.read("shared.txt").unwrap().contains("not committed"), "nothing uncommitted reached the ref");

  // A commit a sync put on another's tip, pushed and landed unheard, then a commit of the same lines: no conflict with oneself.
  a("($NV restore . base >/dev/null 2>&1 || true) && ($NV sync . >/dev/null 2>&1 || true)");
  bob.sync().unwrap();
  bob.write("g.txt", "b\n");
  bob.commit("b1");
  bob.push().unwrap();
  a(
    "sed -i.orig 's/^line 7$/A1/' shared.txt && rm shared.txt.orig && $NV commit . a1 >/dev/null && $NV sync . >/dev/null && cp .nv/workspace ../a-before && $NV push . >/dev/null && cp ../a-before .nv/workspace",
  );
  a("sed -i.orig 's/^A1$/A1b/' shared.txt && rm shared.txt.orig && $NV commit . a3 >/dev/null");
  let said = a("$NV push . 2>&1 || $NV sync . 2>&1; true");
  assert!(!said.contains("same lines"), "{said}");
  a("$NV push . >/dev/null 2>&1 || true");
  bob.sync().unwrap();
  assert!(bob.read("shared.txt").unwrap().contains("A1b\n"));

  // A file with a sync's markers, committed with `nv git`, is not pushed.
  a("printf 'x\\n<<<<<<< yours\\nmine\\n=======\\ntheirs\\n>>>>>>> theirs\\n' > m.txt && $NV git add m.txt && $NV git commit -qm markers");
  let refused = a("$NV push . 2>&1 || true");
  assert!(refused.contains("markers"), "{refused}");
}

#[test]
fn what_an_eleventh_reviewer_found_in_syncing_stays_fixed() {
  let Some((root, url)) = shared_file("eleventh") else { return };
  let nv = env!("CARGO_BIN_EXE_nv");
  let mut bob = Peer::new(&root, "bob", &[], &url);
  bob.write("start", "s\n");
  bob.commit("start");
  bob.push().unwrap();
  for name in ["a", "c"] {
    sh(&root, &format!("{nv} git clone -q repo {name} && {nv} remote {name} {url} main && {nv} sync {name} >/dev/null"));
  }
  let run = |name: &str, script: &str| {
    let out = common::bounded(
      Command::new("sh")
        .arg("-c")
        .arg(format!("NV={nv}; cd {name}; {script}"))
        .current_dir(&root)
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "c")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null"),
    );
    (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
  };
  // After a sync that left a conflict, the commit there is the ref's, made by someone else: amending
  // it would put this workspace's resolution in that writer's commit, and a later sync would take
  // the two for one another's. Refused, as Git refuses it in a merge that stopped.
  replace_line(&bob, "shared.txt", 1, "B1");
  replace_line(&bob, "shared.txt", 3, "B1");
  bob.commit("b");
  bob.push().unwrap();
  let line = |n: usize, to: &str| format!("sed -i.orig '{n}s/.*/{to}/' shared.txt && rm shared.txt.orig");
  run("a", &format!("{} && $NV commit . a1 >/dev/null", line(1, "A1")));
  assert_ne!(run("a", "$NV push . 2>&1").0, 0);
  let (code, said) = run("a", "$NV sync . 2>&1");
  assert_eq!(code, 1, "{said}");
  // Settled by hand: the conflicting lines become one, and the rest is as the sync merged it.
  run(
    "a",
    "awk '/^<<<<<<< /{print \"A1 resolved\"; skip=1; next} /^>>>>>>> /{skip=0; next} !skip' shared.txt > s && mv s shared.txt && $NV git add -A",
  );
  let (code, said) = run("a", "$NV git commit --amend -m A1resolved 2>&1");
  assert_eq!(code, 128, "{said}");
  assert!(said.contains("cannot amend"), "{said}");
  let (code, said) = run("a", "$NV git commit -qm resolved 2>&1 && $NV push . 2>&1");
  assert_eq!(code, 0, "{said}");
  // A third writer's change to another line is not undone by anyone's next sync.
  let (code, said) = run("c", &format!("$NV sync . >/dev/null && {} && $NV commit . c1 >/dev/null && $NV push . 2>&1", line(3, "C1")));
  assert_eq!(code, 0, "{said}");
  let (code, said) =
    run("a", &format!("{} && $NV commit . a2 >/dev/null && ($NV push . 2>&1 || ($NV sync . 2>&1 && $NV push . 2>&1))", line(5, "A2")));
  assert_eq!(code, 0, "{said}");
  bob.sync().unwrap();
  let shared = bob.read("shared.txt").unwrap();
  assert_eq!(shared.lines().take(5).collect::<Vec<_>>(), ["A1 resolved", "line 2", "C1", "line 4", "A2"], "{shared}");

  // Markers committed, and a commit after them: the push says what gets them out of the chain, and it does.
  run(
    "a",
    "printf 'x\\n<<<<<<< yours\\nmine\\n=======\\ntheirs\\n>>>>>>> theirs\\n' > m.txt && $NV git add m.txt && $NV git commit -qm markers",
  );
  run("a", "echo more > more.txt && $NV git add more.txt && $NV git commit -qm more");
  let (code, said) = run("a", "$NV push . 2>&1");
  assert_ne!(code, 0, "{said}");
  assert!(said.contains("nv git reset --soft HEAD~2"), "{said}");
  let (code, said) = run(
    "a",
    "$NV git reset -q --soft HEAD~2 && echo settled > m.txt && $NV git add m.txt && $NV git commit -qm settled && $NV push . 2>&1",
  );
  assert_eq!(code, 0, "{said}");
  bob.sync().unwrap();
  assert_eq!((bob.read("m.txt").as_deref(), bob.read("more.txt").as_deref()), (Some("settled\n"), Some("more\n")));

  // Beside Git, a lock Git left behind: the sync is refused before it writes a file, not after.
  sh(
    &root,
    &format!(
      "git clone -q repo g && cd g && git config user.name g && git config user.email g@example.com && {nv} adopt . >/dev/null && {nv} remote . {url} main && {nv} sync . >/dev/null"
    ),
  );
  bob.write("from-bob.txt", "b\n");
  bob.commit("from bob");
  bob.push().unwrap();
  let before = run("g", "ls; git status --porcelain; git rev-parse HEAD; cksum < .git/index").1;
  let (code, said) = run("g", "touch .git/HEAD.lock && $NV sync . 2>&1");
  assert_ne!(code, 0, "{said}");
  assert!(said.contains(".git/HEAD.lock"), "{said}");
  assert_eq!(run("g", "rm .git/HEAD.lock; ls; git status --porcelain; git rev-parse HEAD; cksum < .git/index").1, before);
  let (code, said) = run("g", "$NV sync . >/dev/null 2>&1 && cat from-bob.txt && git status --porcelain");
  assert_eq!((code, said.as_str()), (0, "b\n"));
}

/// Found by the model in `formal/sync`: a push that landed unheard, then more work on the same
/// lines, then a sync after another writer changed the file elsewhere.
#[test]
fn work_after_a_push_that_landed_unheard_is_merged_on_what_that_push_left() {
  let Some((root, url)) = shared_file("model-landed") else { return };
  let nv = env!("CARGO_BIN_EXE_nv");
  let mut bob = Peer::new(&root, "bob", &[], &url);
  bob.write("start", "s\n");
  bob.commit("start");
  bob.push().unwrap();
  sh(&root, &format!("{nv} git clone -q repo a && {nv} remote a {url} main && {nv} sync a >/dev/null"));
  let a = |script: &str| sh(&root, &format!("NV={nv}; cd a; {script}"));
  let line = |n: usize, to: &str| format!("sed -i.orig '{n}s/.*/{to}/' shared.txt && rm shared.txt.orig");
  // The push lands, and this workspace never hears so.
  let unheard = "cp .nv/workspace ../a-before && $NV push . >/dev/null && cp ../a-before .nv/workspace";
  for (round, (again, wanted)) in [("line 1", "line 1"), ("A again", "A again")].into_iter().enumerate() {
    a(&format!("{} && $NV commit . c1 >/dev/null && {unheard}", line(1, &format!("A {round}"))));
    a(&format!("{} && $NV commit . c2 >/dev/null", line(1, again)));
    bob.sync().unwrap();
    replace_line(&bob, "shared.txt", 20, &format!("bob {round}"));
    bob.commit("bob");
    bob.push().unwrap();
    // Line 1 is what this workspace made it last, on top of its own push; nobody else touched it.
    let said = a("($NV sync . 2>&1 || true); head -1 shared.txt; tail -1 shared.txt; cat .nv/conflicts 2>/dev/null || true");
    assert!(said.ends_with(&format!("{wanted}\nbob {round}\n")), "round {round}: {said}");
    a("$NV push . >/dev/null");
    bob.sync().unwrap();
    assert_eq!(bob.read("shared.txt").unwrap().lines().next(), Some(wanted), "round {round}");
  }
}

/// What a committer reads at each step says where the ref is and what to do next, in the words
/// of the commands it is to use: round 1 of `eval/commit-agents` found each of these missing.
#[test]
fn what_a_committer_reads_names_the_ref_and_what_to_do_next() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-words");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/words-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src
      printf 'one\ntwo\nthree\n' > f.txt
      printf '[checks.unit]\nrun = "true"\n\n[commit]\neach = true\n' > nv.toml
      git add -A && git commit -q -m seed && cd ..
      {nv} git clone -q src ann && {nv} git clone -q src bob
      {nv} remote ann {url} main && {nv} remote bob {url} main
      "##
    ),
  );
  let run = |dir: &str, script: &str| -> (i32, String) {
    let out = common::bounded(
      Command::new("sh")
        .arg("-c")
        .arg(format!("{{ {script}; }} 2>&1"))
        .current_dir(root.join(dir))
        .env("NV", nv)
        .env("GIT_AUTHOR_NAME", dir)
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", dir)
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null"),
    );
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
  };
  // Ann starts the ref: the push says how many commits, which ref, and where it is now.
  let (_, said) = run("ann", "sed -i 's/one/ann one/' f.txt && $NV commit . 'Ann edits one' >/dev/null && $NV push .");
  let tip = run("ann", "$NV git log -1 --format=%h").1;
  assert!(
    said.contains(&format!("pushed 1 commit: 1 as they were, 0 put on a newer tip; `main` on {url} is now at {}", tip.trim())),
    "{said}"
  );
  // Git's status speaks of the ref nv pushes to, not of the repository it was cloned from.
  let (_, said) = run("ann", "$NV git status");
  assert!(said.contains(&format!("Your branch is up to date with 'main' at {url}, as last seen.")), "{said}");
  let (_, said) = run("ann", "echo four >> f.txt && $NV commit . 'Ann adds four' >/dev/null && $NV git status");
  assert!(said.contains(&format!("Your branch is ahead of 'main' at {url} by 1 commit, as last seen.\n  (use \"nv push\"")), "{said}");
  // Bob changed the same line: refused, with how far the ref went and what to do.
  let (code, said) = run("bob", "sed -i 's/one/bob one/' f.txt && $NV commit . 'Bob edits one' >/dev/null && $NV push .");
  assert_eq!(code, 2, "{said}");
  assert!(
    said.contains("the ref took 1 commit since this workspace last saw it") && said.contains("f.txt") && said.contains("`nv sync`"),
    "{said}"
  );
  // The sync leaves both versions, and says how the settling becomes a commit where each
  // passing state is committed by itself.
  let (code, said) = run("bob", "$NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("once the conditions pass on them they are committed") && said.contains("`nv rebuild`"), "{said}");
  assert!(said.contains("caught up 1 commit:"), "{said}");
  // Until it is settled, `nv pending` says the file is in none of the commits it lists.
  let (_, said) = run("bob", "$NV pending .");
  assert!(said.contains("still holding both versions, and so in none of the commits above: f.txt"), "{said}");
  // A range is refused in words that say what to use instead.
  let (code, said) = run("ann", "$NV git log HEAD~1..HEAD");
  assert_eq!(code, 125, "{said}");
  assert!(said.contains("a range of commits (`HEAD~1..HEAD`)") && said.contains("`nv pending`"), "{said}");
}
