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
    said.contains("nothing else came from the ref") && !said.contains("0 files written") && !said.contains("linked directories"),
    "a push that brought nothing, from a workspace of no links, says so and no more: {said}"
  );
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
  assert!(said.contains("once the conditions pass on them, each commit of yours that changed them gets its own version"), "{said}");
  assert!(said.contains("caught up 1 commit:"), "{said}");
  // Until it is settled, `nv pending` says the file is in none of the commits it lists.
  let (_, said) = run("bob", "$NV pending .");
  assert!(said.contains("still holding both versions, and so in none of the commits above: f.txt"), "{said}");
  // A range is refused in words that say what to use instead.
  let (code, said) = run("ann", "$NV git log HEAD~1..HEAD");
  assert_eq!(code, 125, "{said}");
  assert!(said.contains("a range of commits (`HEAD~1..HEAD`)") && said.contains("`nv pending`"), "{said}");
}

/// Two writers who each add an entry to a list `.gitattributes` gives `merge=union` do not leave
/// each other a conflict, nor a sync to make: the server merges the list as `nv sync` would, the
/// push lands, and the conditions are run on what it made (rounds 2 and 3 of `eval/commit-agents`).
#[test]
fn entries_added_to_a_union_list_by_two_writers_are_both_kept_and_checked() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-union");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/union-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src
      printf 'start\n' > list && printf 'start\n' > plain && printf 'list merge=union\n' > .gitattributes
      printf 'auto = true\n[checks.unit]\nrun = "test $(grep -c . list) -ge 2 && ! grep -q \\"<<<<<<<\\" list"\n\n[commit]\neach = true\n' > nv.toml
      git add -A && git commit -q -m seed && cd ..
      for w in ann bob cat dan; do {nv} git clone -q src $w && {nv} remote $w {url} main; done
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
  let read = |dir: &str, path: &str| std::fs::read_to_string(root.join(dir).join(path)).unwrap();
  let (code, said) = run("ann", "echo ann >> list && $NV commit . 'Ann adds an entry' >/dev/null && $NV push .");
  assert_eq!(code, 0, "{said}");
  // Bob added to the list too, and never looked: the server keeps both entries, Bob's first, as
  // the sync would have, and Bob's commit lands on Ann's.
  let (code, said) = run("bob", "echo bob >> list && $NV commit . 'Bob adds an entry' >/dev/null && $NV push .");
  assert_eq!(code, 0, "{said}");
  assert!(
    said.contains("pushed 1 commit: 0 as they were, 1 put on a newer tip") && said.contains("merged on the server (merge=union): list"),
    "{said}"
  );
  assert!(
    said.contains("1 file content sent (and 2 the server merged list with)"),
    "what the base was, and the rules, are counted apart: {said}"
  );
  assert!(said.contains("caught up 1 commit:"), "Ann's, not Bob's own as it landed: {said}");
  assert!(said.contains("the conditions are run on what landed: `nv pending <dir> --wait` says how they end"), "{said}");
  assert_eq!(read("bob", "list"), "start\nbob\nann\n", "the workspace has what landed");
  let tip = run("bob", "$NV git log -1 --format=%h").1;
  assert!(said.contains(&format!("is now at {}", tip.trim())) && !tip.trim().is_empty(), "the push names the tip: {said}");
  assert!(said.contains(&format!("1 put on a newer tip (yours: {})", tip.trim())), "and its own commits as they landed: {said}");
  let (_, said) = run("bob", "$NV pending . --wait");
  assert!(said.starts_with("nothing committed here that is not pushed"), "its own commit is known as landed: {said}");
  assert!(said.ends_with("settled: what is recorded is committed, and passed unit\n"), "what the server made is run on: {said}");
  assert_eq!(run("bob", "$NV git log --format=%s -2").1, "Bob adds an entry\nAnn adds an entry\n");
  // Cat's chain of two, one of which adds to the list, the other to a file nobody else changed.
  let (code, said) = run(
    "cat",
    "echo cat >> list && $NV commit . 'Cat adds an entry' >/dev/null && echo more >> plain && $NV commit . 'Cat edits plain' >/dev/null && $NV push .",
  );
  assert_eq!(code, 0, "{said}");
  assert_eq!(read("cat", "list"), "start\ncat\nbob\nann\n");
  assert_eq!(run("cat", "$NV git log --format=%s -3").1, "Cat edits plain\nCat adds an entry\nBob adds an entry\n");
  // Everyone who syncs gets the same list, and the same ids.
  let (code, said) = run("ann", "$NV sync .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(read("ann", "list"), "start\ncat\nbob\nann\n");
  assert_eq!(run("ann", "$NV git log -1 --format=%H").1, run("cat", "$NV git log -1 --format=%H").1);
  // Where the conditions run behind writes, a sync that puts a commit of one's own on a newer
  // tip says that they are run on it, and where to hear how they end.
  run("cat", "echo c > cat-only && $NV commit . 'Cat adds a file' >/dev/null && $NV push . >/dev/null");
  let (code, said) = run("ann", "echo a > ann-only && $NV commit . 'Ann adds a file' >/dev/null && $NV sync .");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("the conditions are run on what the sync made: `nv pending <dir> --wait` says how they end"), "{said}");
  let (code, said) = run("ann", "$NV pending . --wait");
  assert!(code == 0 && said.ends_with("passed unit\n"), "{said}");
  // A file without the attribute still clashes, and the push is refused as before.
  run("ann", "echo a2 >> plain && $NV commit . 'Ann edits plain' >/dev/null && $NV push .");
  let (code, said) = run("bob", "echo b2 >> plain && $NV commit . 'Bob edits plain' >/dev/null && $NV push .");
  assert_eq!(code, 2, "{said}");
  let (code, said) = run("bob", "$NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("both sides changed the same lines of: plain"), "{said}");
  // Rules that changed under the pusher are not taken on its word: Dan's list was union when he
  // started, the ref's is not any more, and his push is refused.
  let (code, said) = run(
    "ann",
    "$NV sync . >/dev/null; : > .gitattributes && echo ann2 >> list && $NV commit . 'Ann ends the union' >/dev/null && $NV push .",
  );
  assert_eq!(code, 0, "{said}");
  let (code, said) = run("dan", "echo dan >> list && $NV commit . 'Dan adds an entry' >/dev/null && $NV push .");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("has changes to the same paths: list"), "{said}");
}

/// With no agent to make the commits again: `nv publish` waits for what is coming, makes the
/// commits not pushed into one at each note, the last under the message it is given, and
/// pushes them.
#[test]
fn publish_makes_a_commit_at_each_note_and_pushes_them() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-publish");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/publish-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && echo start > a
      printf '1\n2\n3\n' > lines
      printf '[checks.unit]\nrun = "true"\n\n[commit]\neach = true\n\n[body]\nrun = "[ -z \\"$NV_BODIES\\" ] || echo \\"$NV_SUBJECT\\" >> \\"$NV_BODIES\\"; echo \\"why $NV_SUBJECT\\"; grep -c ^diff"\n' > nv.toml
      git add -A && git commit -q -m seed && cd ..
      {nv} git clone -q src ws && {nv} remote ws {url} main
      "##
    ),
  );
  let run = |script: &str| -> (i32, String) {
    let out = common::bounded(
      Command::new("sh")
        .arg("-c")
        .arg(format!("{{ {script}; }} 2>&1"))
        .current_dir(root.join("ws"))
        .env("NV", nv)
        .env("GIT_AUTHOR_NAME", "dev")
        .env("GIT_AUTHOR_EMAIL", "dev@example.com")
        .env("GIT_COMMITTER_NAME", "dev")
        .env("GIT_COMMITTER_EMAIL", "dev@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null"),
    );
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
  };
  // Two states before a note, one after it: two commits, the first at the note.
  run("echo 1 > b && $NV checkpoint . >/dev/null && echo 2 > c && $NV note . 'Add b and c' && $NV checkpoint . >/dev/null");
  run("echo 3 > d && $NV checkpoint . >/dev/null");
  assert_eq!(run("$NV pending .").1.matches("  git ").count(), 3);
  let (code, said) = run("$NV publish . --message 'Add d'");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("$NV git log --format=%s -3").1, "Add d\nAdd b and c\nseed\n", "{said}");
  assert_eq!(run("$NV git show --name-only --format= HEAD~1").1, "b\nc\n");
  // A body for each, from the `[body]` hook, given the note and the change.
  assert_eq!(run("$NV git log -1 --format=%B HEAD~1").1, "Add b and c\n\nwhy Add b and c\n2\n\n");
  assert!(run("$NV pending .").1.starts_with("nothing committed here that is not pushed"));
  // Without a message for what came after the last note, it goes with the note's commit.
  run("echo 4 > e && $NV note . 'Add e' && $NV checkpoint . >/dev/null && echo 5 > f && $NV checkpoint . >/dev/null");
  let (code, said) = run("$NV publish .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("$NV git log --format=%s -2").1, "Add e\nAdd d\n", "{said}");
  assert_eq!(run("$NV git show --name-only --format= HEAD").1, "e\nf\n");
  // Nothing to publish is said, and is not an error.
  let (code, said) = run("$NV publish .");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("nothing to publish"), "{said}");
  // A publish that stopped over a clash, and is run again once it is settled: the commit gets
  // one body, about what it is now, not the one it got the first time as well.
  sh(
    &root,
    &format!(
      "{nv} git clone -q src other && {nv} remote other {url} main && cd other && {nv} sync . >/dev/null && echo theirs > a && {nv} commit . 'Theirs' >/dev/null && {nv} push . >/dev/null"
    ),
  );
  run("echo mine > a && $NV note . 'Change a' && $NV checkpoint . >/dev/null");
  let (code, said) = run("$NV publish .");
  assert_eq!(code, 1, "{said}");
  run("echo 'mine and theirs' > a && $NV checkpoint . >/dev/null");
  let (code, said) = run("$NV publish .");
  assert_eq!(code, 0, "{said}");
  // The body written once, and below it what was settled, against whose commit.
  let theirs = run("cd ../other && $NV git log -1 --format=%h").1.trim().to_string();
  assert_eq!(
    run("$NV git log -1 --format=%B").1,
    format!("Change a\n\nwhy Change a\n1\n\nSettled: a with {theirs} (Theirs)\n\n"),
    "{said}"
  );
  // A publish whose push the ref refused and whose sync merged by itself writes the body again,
  // of the change as it lands on the new tip: a body of the old base would say what may no
  // longer hold.
  sh(
    &root,
    &format!(
      "cd other && {nv} sync . >/dev/null && printf 'x\\n2\\n3\\n' > lines && {nv} commit . 'Their lines' >/dev/null && {nv} push . >/dev/null"
    ),
  );
  run("printf '1\\n2\\nmine\\n' > lines && $NV note . 'Add lines' && $NV checkpoint . >/dev/null");
  let (code, said) = run("NV_BODIES=bodies.log $NV publish .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(std::fs::read_to_string(root.join("ws/bodies.log")).unwrap().lines().filter(|l| *l == "Add lines").count(), 2, "{said}");
  assert_eq!(run("cat lines").1, "x\n2\nmine\n");
}

/// A sync leaves out of the workspace's own commit a file it leaves holding both versions; the
/// settling, committed as a checkpoint, used to be a commit of its own, and the commit put on the
/// tip stayed short of its change, under a message that said otherwise, and untested (round 12
/// of `eval/commit-agents`). The settling goes into that commit now, while it is not pushed: the
/// commit is whole again, under its message. Never into a commit that is not the workspace's.
#[test]
fn settling_a_clash_completes_the_commit_the_sync_left_it_out_of() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-settle-into");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/settle-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf '# list\n- (none yet)\n' > README.md
      printf '[checks.unit]\nrun = "! grep -q none README.md"\n\n[commit]\neach = true\n' > nv.toml
      git add -A && git commit -q -m seed && cd ..
      for w in ann bob cat; do {nv} git clone -q src $w && {nv} remote $w {url} main; done
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
  let (code, said) = run(
    "ann",
    "printf '# list\\n- ann\\n' > README.md && echo a > a.py && $NV checkpoint . >/dev/null && $NV rebuild . 1 'Add a' >/dev/null && $NV push .",
  );
  assert_eq!(code, 0, "{said}");
  // Bob's commit holds b.py and his line of the list; the list clashes with Ann's.
  let (code, said) = run(
    "bob",
    "printf '# list\\n- bob\\n' > README.md && echo b > b.py && $NV checkpoint . >/dev/null && $NV rebuild . 1 'Add b' >/dev/null && $NV push .",
  );
  assert_eq!(code, 2, "{said}");
  let (code, said) = run("bob", "$NV sync .");
  assert_eq!(code, 1, "{said}");
  // Bob's message was written before his commit was put on Ann's, which changes a file it
  // changes: what it says may not hold any more, and that is said until it is written again.
  assert!(said.contains("now on top of commits that change the same files: Add a"), "{said}");
  let moved = "its message was written before it was put on top of: Add a";
  let said = run("bob", "$NV pending .").1;
  // It is for the committer to read, not a stop: what to do of it is said, and that a push goes.
  assert!(said.contains(moved) && said.contains("a push does not wait on it"), "{said}");
  // Settled, and committed as the conditions pass: one commit, whole, under its own message.
  let (code, said) = run("bob", "printf '# list\\n- ann\\n- bob\\n' > README.md && $NV checkpoint . >/dev/null && $NV pending .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(said.matches("  git ").count(), 1, "the settling went into Bob's commit: {said}");
  assert!(said.contains("Add b  (passed unit)") && said.contains("M README.md") && said.contains("A b.py"), "{said}");
  assert!(said.contains(moved), "the settling does not answer for the message: {said}");
  let (_, said) = run("bob", "$NV rebuild . 1 'Add b' >/dev/null && $NV pending .");
  assert!(!said.contains(moved), "a message written again is written on the new base: {said}");
  // A sync that puts Bob's commit on a newer tip cleanly says that the conditions now run on it.
  let (code, _) = run("ann", "echo z > z.py && $NV checkpoint . >/dev/null && $NV rebuild . 1 'Add z' >/dev/null && $NV push .");
  assert_eq!(code, 0);
  let (code, said) = run("bob", "$NV sync .");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("the conditions have not run on what the sync made: `nv checkpoint <dir>` runs them"), "{said}");
  let (code, said) = run("bob", "$NV push .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("bob", "$NV git show --name-only --format=%s HEAD").1, "Add b\n\nREADME.md\nb.py\n");
  // The history says what was settled, and against whose commit, through the rebuild: what a
  // reader of it could not find out in the twelfth round's reader experiment.
  let ann_add = run("ann", "$NV git log --format='%h %s' | grep 'Add a$' | cut -d' ' -f1").1.trim().to_string();
  let said = run("bob", "$NV git log -1 --format=%B").1;
  assert!(said.contains(&format!("Settled: README.md with {ann_add} (Add a)")), "{said}");
  // Cat clashes with nothing of his own committed: what he settles is a commit of his, on the
  // ref's tip, never a change to Bob's.
  let (code, said) = run("cat", "printf '# list\\n- cat\\n' > README.md && $NV sync .");
  assert_eq!(code, 1, "{said}");
  run("cat", "printf '# list\\n- ann\\n- bob\\n- cat\\n' > README.md && $NV checkpoint . >/dev/null");
  assert_eq!(run("cat", "$NV git log --format=%s -2").1, "checkpoint: unit passed\nAdd b\n");
  assert_eq!(run("cat", "$NV git show --name-only --format= HEAD~1").1, "README.md\nb.py\n", "Bob's commit is as he pushed it");
  // A settling that is a commit of its own says what it settled too: against both of theirs.
  let bob_add = run("cat", "$NV git log -1 --format=%h HEAD~1").1.trim().to_string();
  let said = run("cat", "$NV git log -1 --format=%B").1;
  assert!(said.contains(&format!("Settled: README.md with {ann_add} (Add a), {bob_add} (Add b)")), "{said}");
}

/// What a twelfth-round reviewer lost by killing a sync: an arrival cut short is finished by the
/// next command from what was recorded, and that wrote over a file the killed sync had already
/// written and the writer had edited since. Both kills are made here as a kill leaves them.
#[test]
fn finishing_an_arrival_keeps_what_was_written_after_the_kill() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-arrival-killed");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/arrival-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf 'one\ntwo\nthree\n' > f.txt && printf 'x\n' > g.txt
      git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ann >/dev/null && {nv} remote ann {url} main
      (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  // Ann changes the first line; Bob's store, which is Ann's, has her commit, and Bob's sync to it is killed after
  // it wrote f.txt. Bob goes on working, in f.txt, before anything finishes the arrival.
  let (code, said) = run("ann", "sed -i 's/^one$/ONE/' f.txt && $NV commit . 'Ann edits f' >/dev/null && $NV push .");
  assert_eq!(code, 0, "{said}");
  let field = |name: &str| run("ann", &format!("sed -n 's/^{name} //p' .nv/workspace")).1.trim().to_string();
  let (tip, seq) = (field("pushed"), field("seq"));
  run("bob", &format!("printf '{tip} {seq}\\n' > .nv/arriving && printf 'ONE\\ntwo\\nthree\\n' > f.txt && echo bob-after-kill >> f.txt"));
  let (code, said) = run("bob", "$NV sync .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("bob", "cat f.txt").1, "ONE\ntwo\nthree\nbob-after-kill\n", "the edit made after the kill is kept: {said}");
  assert!(run("bob", "$NV since . base").1.contains("M f.txt"), "and is the workspace's own, not yet committed");
  // Killed later, once the workspace stood on the new commit and before it noted the files the
  // sync left with both versions: finishing must not take the conflict for settled.
  run("ann", "sed -i 's/^two$/ann-two/' f.txt && $NV commit . 'Ann edits two' >/dev/null && $NV push . >/dev/null");
  let (code, said) = run("bob", "sed -i 's/^two$/bob-two/' f.txt && $NV commit . 'Bob edits two' >/dev/null && $NV push . ; $NV sync .");
  assert_ne!(code, 0, "{said}");
  let (tip, seq) = (field("pushed"), field("seq"));
  run("bob", &format!("printf '{tip} {seq}\\n' > .nv/arriving"));
  let (code, said) = run("bob", "$NV push .");
  assert_ne!(code, 0, "a file still holding both versions is not pushed: {said}");
  assert!(run("bob", "cat .nv/conflicts").1.contains("f.txt"), "and the sync still knows it left it so");
  // Killed after it noted the file left with both versions and before the workspace stood on
  // the new commit: the next sync finishes the arrival before it asks for the file to be
  // settled, or nothing could ever finish it (`nv commit` waits for the sync).
  run("bob", "$NV restore . before-sync >/dev/null");
  run("ann", "sed -i 's/^ann-two$/ann-again/' f.txt && $NV commit . 'Ann again' >/dev/null && $NV push . >/dev/null");
  let (tip, seq) = (field("pushed"), field("seq"));
  run("bob", &format!("printf '{tip} {seq}\\n' > .nv/arriving && printf 'f.txt\\n' > .nv/conflicts"));
  let (code, said) = run("bob", "$NV sync .");
  assert_eq!(code, 1, "the arrival is finished, and the clash said: {said}");
  assert!(run("bob", "ls .nv").1.lines().all(|f| f != "arriving"), "{said}");
  assert!(run("bob", "grep -c '^<<<<<<< yours' f.txt").1.trim() == "1", "{said}");
}

/// A sync that clashes, killed at random moments, and finished by the next: wherever the kill
/// fell, a file left with both versions is one the workspace knows it left so, and is not pushed.
/// (Twelfth round: killed after the workspace stood on the new commit and before it noted the
/// file, the next push went through with the markers in it.)
#[test]
fn a_clashing_sync_killed_anywhere_still_knows_what_it_left_unsettled() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-clash-killed");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/clash-killed-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf 'one\ntwo\nthree\n' > f.txt && git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ann >/dev/null && {nv} remote ann {url} main
      (cd ann && {nv} push . >/dev/null)
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
        .env("GIT_AUTHOR_NAME", "w")
        .env("GIT_AUTHOR_EMAIL", "w@example.com")
        .env("GIT_COMMITTER_NAME", "w")
        .env("GIT_COMMITTER_EMAIL", "w@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null"),
    );
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
  };
  let mut unsettled_seen = 0;
  for round in 0..40 {
    let bob = format!("bob{round}");
    run(".", &format!("$NV fork ann {bob} >/dev/null"));
    run(&bob, &format!("sed -i 's/^two.*/bob {round}/' f.txt && $NV commit . 'Bob {round}' >/dev/null"));
    run("ann", &format!("sed -i 's/^two.*/ann {round}/' f.txt && $NV commit . 'Ann {round}' >/dev/null && $NV push . >/dev/null"));
    let pause = 5 + (round * 37) % 60;
    run(
      &bob,
      &format!(
        "$NV sync . >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,{pause}/1000)'; kill -9 $pid 2>/dev/null; wait $pid 2>/dev/null; true"
      ),
    );
    // Whatever the kill left, the next command finishes it.
    run(&bob, "$NV sync . >/dev/null 2>&1; true");
    if run(&bob, "grep -c '^<<<<<<< yours' f.txt").1.trim() != "0" {
      unsettled_seen += 1;
      let (code, said) = run(&bob, "$NV push .");
      assert_ne!(code, 0, "round {round}: a file holding both versions was pushed: {said}");
      assert!(run(&bob, "cat .nv/conflicts").1.contains("f.txt"), "round {round}: the sync forgot it left f.txt unsettled");
    }
    // Ann is the ref's tip again for the next round.
    run("ann", "$NV sync . >/dev/null 2>&1; true");
  }
  assert!(unsettled_seen > 0, "no round left a file unsettled: the test tried nothing");
}

/// A push whose answer was never heard (a publish killed after the push landed): `nv publish`
/// finds out where it stands and goes on, and a sync that finds the ref at the workspace's own
/// last commit says the push landed. (Twelfth round: every later publish and note was refused,
/// and the sync said nothing came.) The kill is made here as it leaves things.
#[test]
fn a_publish_after_a_push_never_heard_finds_where_it_stands() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-unheard-publish");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/unheard-publish-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && echo start > a && printf '[checks.unit]\nrun = "true"\n\n[commit]\neach = true\n' > nv.toml
      git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ws >/dev/null && {nv} remote ws {url} main
      (cd ws && {nv} push . >/dev/null)
      "##
    ),
  );
  let run = |script: &str| -> (i32, String) {
    let out = common::bounded(
      Command::new("sh")
        .arg("-c")
        .arg(format!("{{ {script}; }} 2>&1"))
        .current_dir(root.join("ws"))
        .env("NV", nv)
        .env("GIT_AUTHOR_NAME", "dev")
        .env("GIT_AUTHOR_EMAIL", "dev@example.com")
        .env("GIT_COMMITTER_NAME", "dev")
        .env("GIT_COMMITTER_EMAIL", "dev@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null"),
    );
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
  };
  // Made again and pushed; the push lands, and its answer is lost: the workspace is as it was
  // before the push, with the note a push leaves until it hears back.
  let unheard = |name: &str| {
    run(&format!(
      "echo {name} > {name} && $NV checkpoint . >/dev/null 2>&1 && $NV rebuild . 1 'Add {name}' >/dev/null && cp .nv/workspace ../before"
    ));
    let base = run("sed -n 's/^base //p' .nv/workspace").1.trim().to_string();
    let (code, said) = run("$NV push .");
    assert_eq!(code, 0, "{said}");
    run(&format!("cp ../before .nv/workspace && printf '1 {base}\\n' > .nv/pushing"));
  };
  unheard("b");
  let (code, said) = run("echo c > c && $NV note . 'Add c' >/dev/null; $NV checkpoint . >/dev/null 2>&1; $NV publish .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("$NV git log --format=%s -3").1, "Add c\nAdd b\nseed\n", "{said}");
  assert!(run("ls .nv").1.lines().all(|f| f != "pushing"), "the push is known to have landed");
  // Pointed at the ref anew, the workspace locates itself on it: the ref's tip is its own last
  // commit, and the sync says the push landed rather than leaving everything refused.
  unheard("d");
  let (code, said) = run(&format!("$NV remote . {url} main && $NV sync ."));
  assert_eq!(code, 0, "{said}");
  assert!(run("ls .nv").1.lines().all(|f| f != "pushing"), "the sync found the push on the ref: {said}");
  let (code, said) = run("$NV pending .");
  assert!(code == 0 && said.starts_with("nothing committed here that is not pushed"), "{said}");
}

/// Commits of one's own that a sync makes one, on the ref's newer tip, keep every line they had
/// (twelfth round: `nv publish` caught up and published two noted steps under the second alone).
#[test]
fn commits_a_sync_makes_one_keep_all_their_lines() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-fold-lines");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/fold-lines-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf '1\n2\n3\n4\n5\n' > f && git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ann >/dev/null && {nv} remote ann {url} main
      (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run("ann", "sed -i 's/^5$/ann/' f && $NV commit . 'Ann edits five' >/dev/null && $NV push . >/dev/null");
  run(
    "bob",
    "sed -i 's/^1$/bob/' f && $NV commit . 'Step A\n\nWhy A.' >/dev/null && echo g > g && $NV commit . 'Step B\n\nWhy B.' >/dev/null",
  );
  let (code, said) = run("bob", "$NV push . ; $NV sync .");
  assert_eq!(code, 0, "{said}");
  // Put on the tip one by one, each with its message: a supervisor's commits stay as it made
  // them (round 17: two noted steps came out of a sync as one).
  assert_eq!(run("bob", "$NV git log -3 --format=%s").1, "Step B\nStep A\nAnn edits five\n", "{said}");
  assert_eq!(run("bob", "$NV git log -1 --format=%b").1.trim(), "Why B.");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD~1").1.trim(), "f");
  assert_eq!(run("bob", "cat f").1, "bob\n2\n3\n4\nann\n");
  assert!(!said.contains("now one commit"), "{said}");
  // Where they cannot be put on it one by one (the first clashes with theirs, the second takes
  // it back), they are one commit as before, under the lines of all of them.
  run("bob", "$NV push . >/dev/null");
  run("ann", "$NV sync . >/dev/null; sed -i 's/^3$/ann3/' f && $NV commit . 'Ann edits three' >/dev/null && $NV push . >/dev/null");
  let (code, said) = run(
    "bob",
    "sed -i 's/^3$/bob3/' f && $NV commit . 'Step C' >/dev/null && sed -i 's/^bob3$/3/' f && echo k > k && $NV commit . 'Step D' >/dev/null && $NV push . ; $NV sync .",
  );
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("bob", "$NV git log -2 --format=%s").1, "Step C; Step D\nAnn edits three\n", "{said}");
  assert!(said.contains("under the lines of all of them"), "{said}");
  // Commits that came are counted though the store, which Ann's fork shares, had them already
  // (twelfth round: "caught up 0 commits", from two reviewers).
  run(
    "ann",
    "$NV sync . >/dev/null; echo 7 > h && $NV commit . 'Ann adds h' >/dev/null && echo 8 > i && $NV commit . 'Ann adds i' >/dev/null && $NV push . >/dev/null",
  );
  let (code, said) = run("bob", "$NV push .");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("caught up 2 commits"), "{said}");
  run("ann", "echo 9 > j && $NV commit . 'Ann adds j' >/dev/null && $NV push . >/dev/null");
  let (code, said) = run("bob", "$NV sync .");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("caught up 1 commit"), "{said}");
}

/// A clash in a file only one of the commits put on the tip changed: the commits stay commits of
/// their own, and the settling goes into the one that changed the file, with those after it made
/// again on top (round 17: the supervisor's two commits were made one by the sync that clashed).
#[test]
fn a_settling_goes_into_the_commit_that_changed_the_file() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-settle-in-place");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/settle-in-place-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf '# list\n- (none yet)\n' > README.md
      printf '[checks.unit]\nrun = "! grep -q none README.md && ! grep -q markers README.md"\n\n[commit]\neach = true\n' > nv.toml
      git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ann >/dev/null && {nv} remote ann {url} main
      (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run(
    "ann",
    "printf '# list\\n- ann\\n' > README.md && echo a > a.py && $NV checkpoint . >/dev/null && $NV rebuild . 1 'Add a' >/dev/null && $NV push . >/dev/null",
  );
  run(
    "bob",
    "printf '# list\\n- bob\\n' > README.md && echo x > x.py && $NV checkpoint . >/dev/null && echo y > y.py && $NV checkpoint . >/dev/null \
     && $NV rebuild . 1 'Add x' 2 'Add y' >/dev/null",
  );
  let (code, said) = run("bob", "$NV push . ; $NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(!said.contains("now one commit"), "{said}");
  let (_, said) = run("bob", "$NV pending .");
  assert_eq!(said.matches("  git ").count(), 2, "still two commits: {said}");
  // Settled, and committed as the conditions pass: into "Add x", which changed README.md.
  run("bob", "printf '# list\\n- ann\\n- bob\\n' > README.md && $NV checkpoint . >/dev/null");
  let (_, said) = run("bob", "$NV pending .");
  assert_eq!(said.matches("  git ").count(), 2, "{said}");
  assert_eq!(run("bob", "$NV git log -3 --format=%s").1, "Add y\nAdd x\nAdd a\n", "{said}");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD~1").1, "README.md\nx.py\n", "{said}");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD").1, "y.py\n");
  assert!(run("bob", "$NV git log -1 --format=%B HEAD~1").1.contains("Settled: README.md with"), "the trailer is on the commit it settled");
  let (code, said) = run("bob", "$NV push .");
  assert_eq!(code, 0, "{said}");
}

/// A path whose `.gitattributes` names a merge driver is merged by a sync as Git merges it: by the
/// command the workspace's configuration gives that driver, and only where it is defined there.
/// A driver that says it could not merge leaves what it wrote, held out as a file with both
/// versions is, for whoever settles it.
#[test]
fn a_sync_merges_by_the_driver_the_attributes_name() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-merge-driver");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/merge-driver-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      printf 'cat "$2" "$3" | awk %s > "$2.new" && mv "$2.new" "$2"\n' "'!seen[\$0]++'" > both.sh
      printf 'echo "could not" > "$1"; exit 1\n' > refuse.sh
      git init -q -b main src && cd src && printf 'a\nb\n' > list.txt && echo one > hard.txt
      printf 'list.txt merge=both\nhard.txt merge=refuse\n' > .gitattributes
      git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ann >/dev/null && {nv} remote ann {url} main
      (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run("ann", "printf 'a\\nb\\nann\\n' > list.txt && $NV commit . 'Ann lists' >/dev/null && $NV push . >/dev/null");
  run("bob", "printf 'a\\nb\\nbob\\n' > list.txt && $NV commit . 'Bob lists' >/dev/null");
  // Undefined here, the driver is no driver: the text merge, which leaves both versions.
  let (code, said) = run("bob", "$NV push . ; $NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("both sides changed the same lines of: list.txt"), "{said}");
  run("bob", "$NV restore . before-sync >/dev/null");
  // Defined, it merges, and the sync is clean.
  let (code, said) = run("bob", "$NV git config merge.both.driver 'sh ../both.sh %O %A %B' && $NV sync .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(std::fs::read_to_string(root.join("bob/list.txt")).unwrap(), "a\nb\nbob\nann\n");
  let (code, said) = run("bob", "$NV push .");
  assert_eq!(code, 0, "{said}");
  // A driver that says it could not merge and leaves no markers is not taken at its word: the
  // file is merged as text, both versions between markers, held out until it is settled.
  run("ann", "$NV sync . >/dev/null; echo 'one by ann' > hard.txt && $NV commit . 'Ann hard' >/dev/null && $NV push . >/dev/null");
  run("bob", "echo 'one by bob' > hard.txt && $NV commit . 'Bob hard' >/dev/null");
  let (code, said) = run("bob", "$NV git config merge.refuse.driver 'sh ../refuse.sh %A' && $NV push . ; $NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("hard.txt"), "{said}");
  assert!(std::fs::read_to_string(root.join("bob/hard.txt")).unwrap().starts_with("<<<<<<< yours\none by bob\n"));
  let (code, said) = run("bob", "$NV commit . 'Bob hard, settled'");
  assert_ne!(code, 0, "held out until it is settled: {said}");
  assert!(said.contains("hard.txt"), "{said}");
  // No file of the driver's is left behind in the workspace.
  assert_eq!(run("bob", "ls .nv | grep -c '^merge-' || true").1.trim(), "0");
}

/// Without a runner behind writes, `nv commit` given no message is how a settling completes the
/// commit it belongs to, as `git rebase --continue` keeps the message of the commit that stopped:
/// no commit is left on the ref without the files the sync held out of it (`eval/agent-bench`,
/// where a writer that gave a message made a commit of the settling after two that fail).
#[test]
fn a_commit_given_no_message_completes_the_commit_a_settling_belongs_to() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-settle-by-commit");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/settle-by-commit-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf '# list\n- (none yet)\n' > README.md
      git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ann >/dev/null && {nv} remote ann {url} main
      (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run("ann", "printf '# list\\n- ann\\n' > README.md && $NV commit . 'Add a' >/dev/null && $NV push . >/dev/null");
  run(
    "bob",
    "printf '# list\\n- x\\n' > README.md && echo x > x.py && $NV commit . 'Add x' >/dev/null \
     && printf '# list\\n- x\\n- y\\n' > README.md && echo y > y.py && $NV commit . 'Add y' >/dev/null",
  );
  let (code, said) = run("bob", "$NV push . ; $NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("`nv commit <dir>` with no message"), "the sync says how: {said}");
  assert!(said.contains("each commit of yours that changed them"), "and what becomes of the commits: {said}");
  // What each commit is held out of until the settling, and where the settling goes.
  let (_, said) = run("bob", "$NV pending .");
  assert_eq!(said.matches("README.md (held out until it is settled)").count(), 2, "{said}");
  assert!(
    said.contains("each commit above that changed them gets its own version of them") && said.contains("Add y the settled one"),
    "{said}"
  );
  // Given no message with nothing to complete, or with more than the settling, it still asks for one.
  let (code, said) = run("bob", "printf '# list\\n- ann\\n- x\\n- y\\n' > README.md && echo z > z.py && $NV commit .");
  assert_ne!(code, 0, "{said}");
  assert!(said.contains("a commit needs a message") && said.contains("z.py"), "{said}");
  let (code, said) = run("bob", "rm z.py && $NV commit .");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("completes Add y"), "{said}");
  assert_eq!(run("bob", "$NV git log -3 --format=%s").1, "Add y\nAdd x\nAdd a\n");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD").1, "README.md\ny.py\n");
  assert!(run("bob", "$NV git log -1 --format=%B").1.contains("Settled: README.md with"));
  // The earlier commit is not left without its line: it has the file as the settling has it, less
  // what the later commit added, and says it was settled as well.
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD~1").1, "README.md\nx.py\n");
  assert_eq!(run("bob", "$NV git show HEAD~1:README.md").1, "# list\n- ann\n- x\n");
  assert!(run("bob", "$NV git log -1 --format=%B HEAD~1").1.contains("Settled: README.md with"));
  assert_eq!(run("bob", "$NV git status --porcelain").1, "");
  let (code, said) = run("bob", "$NV push .");
  assert_eq!(code, 0, "{said}");
  // With nothing left to complete, a commit needs a message as ever.
  let (code, said) = run("bob", "echo w > w.py && $NV commit .");
  assert_ne!(code, 0, "{said}");
  assert!(said.contains("a commit needs a message"), "{said}");
}

/// Where a workspace pushes to a ref of nv's, Git's verbs for the ref are nv's: `nv git push` is
/// `nv push` and `nv git pull --rebase` is `nv sync`, with what they say and Git's exit codes,
/// and not a push to, or a pull from, the repository the workspace was cloned from, which is not
/// where its work goes (`eval/agent-bench`: a writer that knows Git reached for `nv help` first).
/// What nv cannot be (a pull that merges, a fetch) is refused, and says what to use.
#[test]
fn git_verbs_for_the_ref_are_nvs_push_and_sync() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-git-verbs");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/git-verbs-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf 'a\nb\n' > list && echo x > x
      git add -A && git commit -q -m seed && cd ..
      {nv} git clone -q src ann && {nv} remote ann {url} main && (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run("ann", "printf 'a\\nb\\nann\\n' > list && $NV commit . 'Ann lists' >/dev/null && $NV git push");
  run("bob", "printf 'A\\nb\\n' > list && $NV commit . 'Bob capitalises a' >/dev/null");
  // Refused as `nv push` is (both changed `list`), with Git's exit code for a push refused.
  let (code, said) = run("bob", "$NV git push");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("the push was refused"), "{said}");
  // A pull that would merge is not what nv does with a ref: refused, saying what is.
  let (code, said) = run("bob", "$NV git pull");
  assert_eq!(code, 125, "{said}");
  assert!(said.contains("not supported") && said.contains("git pull --rebase"), "{said}");
  let (code, said) = run("bob", "$NV git pull --rebase");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("caught up 1 commit"), "{said}");
  let (code, said) = run("bob", "$NV git push origin main");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("pushed 1 commit"), "{said}");
  run("ann", "$NV git pull --rebase -q >/dev/null");
  assert_eq!(run("ann", "$NV git log --format=%s").1, "Bob capitalises a\nAnn lists\nseed\n");
  assert_eq!(std::fs::read_to_string(root.join("ann/list")).unwrap(), "A\nb\nann\n");
  // A ref it does not push to is not this one.
  let (code, said) = run("bob", "$NV git push origin other");
  assert_eq!(code, 125, "{said}");
  // A commit of nothing but a file that clashes, settled as Git's rebase is: `git add`, then
  // `git rebase --continue`, which completes it under its own message.
  run("bob", "$NV git pull --rebase -q >/dev/null");
  run("ann", "printf 'ANN\\nb\\nann\\n' > list && $NV git commit -q -am 'Ann shouts' && $NV git push -q");
  run("bob", "printf 'BOB\\nb\\nann\\n' > list && $NV git commit -q -am 'Bob shouts'");
  let (code, said) = run("bob", "$NV git push");
  assert_eq!(code, 1, "{said}");
  let (code, said) = run("bob", "$NV git pull --rebase");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("both sides changed the same lines of: list"), "{said}");
  // The files it left with both versions are what Git's rebase would list as unmerged.
  assert_eq!(run("bob", "$NV git diff --name-only --diff-filter=U").1, "list\n");
  // Undone as Git's rebase is, to what was there before.
  let (code, said) = run("bob", "$NV git rebase --abort");
  assert_eq!(code, 0, "{said}");
  assert_eq!(std::fs::read_to_string(root.join("bob/list")).unwrap(), "BOB\nb\nann\n");
  run("bob", "$NV git pull --rebase -q >/dev/null");
  let (code, said) = run("bob", "printf 'ANN BOB\\nb\\nann\\n' > list && $NV git add list && $NV git rebase --continue");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("bob", "$NV git log -2 --format=%s").1, "Bob shouts\nAnn shouts\n");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD").1, "list\n");
  let (code, said) = run("bob", "$NV git push");
  assert_eq!(code, 0, "{said}");
}

/// `git fetch` toward the ref brings the ref's commits into the store and changes no file and
/// nothing the workspace stands on; `origin/<ref>` names the tip it fetched, for log, show, diff
/// and status, until a sync or a push sees a newer one.
#[test]
fn git_fetch_names_the_refs_tip_without_touching_the_files() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-git-fetch");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/git-fetch-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf 'a\nb\n' > list && echo x > x
      git add -A && git commit -q -m seed && cd ..
      {nv} git clone -q src ann && {nv} remote ann {url} main && (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run("ann", "printf 'a\\nb\\nann\\n' > list && $NV git commit -q -am 'Ann lists' && $NV git push -q");
  // Before any fetch, `origin/main` is the ref as the workspace last saw it.
  assert_eq!(run("ann", "$NV git log -1 --format=%s origin/main").1, "Ann lists\n");
  assert_eq!(run("bob", "$NV git log --format=%s origin/main").1, "seed\n");
  let (code, said) = run("bob", "$NV git fetch");
  assert_eq!(code, 0, "{said}");
  assert!(said.contains("main") && said.contains("origin/main"), "{said}");
  // Nothing on disk, and nothing the workspace stands on, moved.
  assert_eq!(std::fs::read_to_string(root.join("bob/list")).unwrap(), "a\nb\n");
  assert_eq!(run("bob", "$NV git log --format=%s").1, "seed\n");
  assert_eq!(run("bob", "$NV git status --porcelain").1, "");
  // What it fetched is there to look at.
  assert_eq!(run("bob", "$NV git log --format=%s origin/main").1, "Ann lists\nseed\n");
  assert_eq!(run("bob", "$NV git show origin/main:list").1, "a\nb\nann\n");
  assert_eq!(run("bob", "$NV git diff --name-only HEAD origin/main").1, "list\n");
  let (_, said) = run("bob", "$NV git status");
  assert!(said.contains("behind") && said.contains("by 1 commit"), "{said}");
  run("bob", "printf 'A\\nb\\n' > list && $NV git commit -q -am 'Bob capitalises a'");
  assert_eq!(run("bob", "$NV git status -sb").1, "## main...origin/main [ahead 1, behind 1]\n");
  // A second fetch with nothing new says nothing and changes nothing.
  let (code, said) = run("bob", "$NV git fetch -q origin");
  assert_eq!((code, said.as_str()), (0, ""));
  // Options it does not speak, and a ref it does not push to, are refused.
  for line in ["$NV git fetch --all", "$NV git fetch origin other"] {
    let (code, said) = run("bob", line);
    assert_eq!(code, 125, "{line}: {said}");
  }
  // A sync takes the newer tip, and a push names it too.
  let (code, said) = run("bob", "$NV git pull --rebase");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("bob", "$NV git status -sb").1, "## main...origin/main [ahead 1]\n");
  run("bob", "$NV git push -q");
  assert_eq!(run("bob", "$NV git log --format=%s origin/main").1, "Bob capitalises a\nAnn lists\nseed\n");
  assert_eq!(run("bob", "$NV git status -sb").1, "## main...origin/main\n");
}

/// A `git pull --rebase` that stopped speaks Git's verbs for what comes next, and `git commit -m`
/// there gives the commit the settling completes a message of its own, as at a rebase stopped at
/// that commit, though commits of this workspace's came after it.
#[test]
fn git_commit_at_a_stopped_pull_gives_the_commit_it_completes_a_message() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-git-commit-stopped");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/git-commit-stopped-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf 'a\nb\n' > list && echo x > x
      git add -A && git commit -q -m seed && cd ..
      {nv} git clone -q src ann && {nv} remote ann {url} main && (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run("ann", "printf 'ANN\\nb\\n' > list && $NV git commit -q -am 'Ann shouts' && $NV git push -q");
  run("bob", "printf 'BOB\\nb\\n' > list && echo y > y && $NV git add -A && $NV git commit -q -m 'Bob shouts and adds y'");
  run("bob", "echo z > z && $NV git add z && $NV git commit -q -m 'Bob adds z'");
  let (code, said) = run("bob", "$NV git pull --rebase");
  assert_eq!(code, 1, "{said}");
  // Told in the verbs it was asked in.
  assert!(said.contains("git add") && said.contains("git rebase --continue") && said.contains("git commit -m"), "{said}");
  let (code, said) =
    run("bob", "printf 'ANN BOB\\nb\\n' > list && $NV git add list && $NV git commit -q -m 'Bob shouts with Ann and adds y'");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("bob", "$NV git log --format=%s").1, "Bob adds z\nBob shouts with Ann and adds y\nAnn shouts\nseed\n");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD~1").1, "list\ny\n");
  assert_eq!(run("bob", "$NV git log -1 --format=%an HEAD~1").1, "bob\n");
  assert_eq!(run("bob", "$NV git status --porcelain").1, "");
  let (code, said) = run("bob", "$NV git push");
  assert_eq!(code, 0, "{said}");
}

/// Where a sync made the commits one, it says so, and not that each will get its version of the
/// files it left with both versions.
#[test]
fn a_sync_that_folds_says_what_the_settling_completes() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-fold-and-clash");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/fold-and-clash-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf '1\n2\n3\n' > f && printf 'one\n' > g
      git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ann >/dev/null && {nv} remote ann {url} main
      (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run("ann", "printf '1\\n2\\nann\\n' > f && echo 'one by ann' > g && $NV commit . 'Ann' >/dev/null && $NV push . >/dev/null");
  run(
    "bob",
    "printf '1\\n2\\nbob\\n' > f && $NV commit . 'Bob tries' >/dev/null && printf '1\\n2\\n3\\n' > f && echo 'one by bob' > g && echo h > h && $NV commit . 'Bob instead' >/dev/null",
  );
  let (code, said) = run("bob", "$NV push . ; $NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(said.contains("now one commit") && said.contains("both sides changed the same lines of: g"), "{said}");
  assert!(!said.contains("each commit of yours"), "{said}");
  assert!(said.contains("they complete the commit of yours"), "{said}");
}

#[test]
fn a_settling_goes_into_the_last_commit_that_changed_the_file() {
  let Ok(server) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  };
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("remote-settle-in-last");
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/settle-in-last-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main src && cd src && printf '# list\n- (none yet)\n' > README.md
      printf '[checks.unit]\nrun = "! grep -q none README.md && ! grep -q markers README.md"\n\n[commit]\neach = true\n' > nv.toml
      git add -A && git commit -q -m seed && cd ..
      {nv} import src store >/dev/null && {nv} checkout store HEAD ann >/dev/null && {nv} remote ann {url} main
      (cd ann && {nv} push . >/dev/null) && {nv} fork ann bob >/dev/null
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
  run(
    "ann",
    "printf '# list\\n- ann\\n' > README.md && echo a > a.py && $NV checkpoint . >/dev/null && $NV rebuild . 1 'Add a' >/dev/null && $NV push . >/dev/null",
  );
  run(
    "bob",
    "printf '# list\\n- x\\n' > README.md && echo x > x.py && $NV checkpoint . >/dev/null \
     && printf '# list\\n- x\\n- y\\n' > README.md && echo y > y.py && $NV checkpoint . >/dev/null \
     && $NV rebuild . 1 'Add x' 2 'Add y' >/dev/null",
  );
  let (code, said) = run("bob", "$NV push . ; $NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(!said.contains("now one commit"), "{said}");
  let (_, said) = run("bob", "$NV pending .");
  assert_eq!(said.matches("  git ").count(), 2, "still two commits: {said}");
  // Both are held out of README.md until it is settled; the later is the one completed.
  assert_eq!(said.matches("README.md (held out until it is settled)").count(), 2, "{said}");
  assert!(said.contains("and Add y the settled one"), "{said}");
  run("bob", "printf '# list\\n- ann\\n- x\\n- y\\n' > README.md && $NV checkpoint . >/dev/null");
  let (_, said) = run("bob", "$NV pending .");
  assert_eq!(said.matches("  git ").count(), 2, "{said}");
  assert_eq!(run("bob", "$NV git log -3 --format=%s").1, "Add y\nAdd x\nAdd a\n", "{said}");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD~1").1, "README.md\nx.py\n", "{said}");
  assert_eq!(run("bob", "$NV git show HEAD~1:README.md").1, "# list\n- ann\n- x\n");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD").1, "README.md\ny.py\n");
  assert!(run("bob", "$NV git log -1 --format=%B HEAD").1.contains("Settled: README.md with"), "the trailer is on the commit it settled");
  assert!(run("bob", "$NV git log -1 --format=%B HEAD~1").1.contains("Settled: README.md with"));
  let (code, said) = run("bob", "$NV push .");
  assert_eq!(code, 0, "{said}");
  // A commit that changed nothing but such a file is not folded into the others either: it is
  // on the tip with nothing in it until the settling, which gives it its version, as a rebase
  // stopped at it would have it settled (`eval/agent-bench`, `bst-cst`: a writer's commit of a
  // shared lemma alone folded all its commits, in every round).
  run(
    "ann",
    "$NV sync . >/dev/null; printf '# list\\n- ann2\\n' > README.md && $NV checkpoint . >/dev/null && $NV rebuild . 1 'Ann again' >/dev/null && $NV push . >/dev/null",
  );
  run(
    "bob",
    "printf '# list\\n- z\\n' > README.md && $NV checkpoint . >/dev/null && printf '# list\\n- z\\n- w\\n' > README.md && echo w > w.py && $NV checkpoint . >/dev/null \
     && $NV rebuild . 1 'Note z' 2 'Add w' >/dev/null",
  );
  let (code, said) = run("bob", "$NV push . ; $NV sync .");
  assert_eq!(code, 1, "{said}");
  assert!(!said.contains("now one commit"), "{said}");
  let (_, said) = run("bob", "$NV pending .");
  assert_eq!(said.matches("  git ").count(), 2, "{said}");
  assert_eq!(said.matches("README.md (held out until it is settled)").count(), 2, "{said}");
  // Settled as a commit of its own, the first would be pushed with nothing in it: refused.
  let (code, said) = run("bob", "printf '# list\\n- ann2\\n- z\\n- w\\n' > README.md && $NV commit . 'Settle the list'");
  assert_ne!(code, 0, "{said}");
  assert!(said.contains("Note z") && said.contains("with no message"), "{said}");
  let (code, said) = run("bob", "$NV commit .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("bob", "$NV git log -3 --format=%s").1, "Add w\nNote z\nAnn again\n");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD~1").1, "README.md\n");
  assert_eq!(run("bob", "$NV git show HEAD~1:README.md").1, "# list\n- ann2\n- z\n");
  assert_eq!(run("bob", "$NV git show --name-only --format= HEAD").1, "README.md\nw.py\n");
  let (code, said) = run("bob", "$NV push .");
  assert_eq!(code, 0, "{said}");
}

/// Two writers on a fresh ref, both made from one Git repository of a six-line `a.txt` and a
/// `b.txt`, and a way to run a script in either, with what it printed, stderr and all.
fn two_writers(name: &str) -> (PathBuf, impl Fn(&str, &str) -> (i32, String) + use<>) {
  let server = std::env::var("NV_SERVER").expect("NV_SERVER");
  let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("remote-{name}"));
  let _ = std::fs::remove_dir_all(&root);
  std::fs::create_dir_all(&root).unwrap();
  let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let url = format!("{}/t/{name}-{unique}", server.trim_end_matches('/'));
  let nv = env!("CARGO_BIN_EXE_nv");
  sh(
    &root,
    &format!(
      r##"
      git init -q -b main seed && cd seed && printf '1\n2\n3\n4\n5\n6\n' > a.txt && echo x > b.txt
      git add -A && git commit -q -m init && cd ..
      {nv} git clone -q seed w1 && {nv} remote w1 {url} main && (cd w1 && {nv} push . >/dev/null)
      {nv} git clone -q seed w2 && {nv} remote w2 {url} main && (cd w2 && {nv} sync . >/dev/null)
      "##
    ),
  );
  let at = root.clone();
  let run = move |dir: &str, script: &str| -> (i32, String) {
    let out = common::bounded(
      Command::new("sh")
        .arg("-c")
        .arg(format!("{{ {script}; }} 2>&1"))
        .current_dir(at.join(dir))
        .env("NV", nv)
        .env("GIT_AUTHOR_NAME", dir)
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", dir)
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null"),
    );
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
  };
  (root, run)
}

/// What a thirteenth reviewer found with several writers on one ref, through Git's verbs: what
/// `git add` staged is not carried across a pull to be committed over what the pull brought; a
/// pull that stopped is finished or undone before anything moves; undoing it undoes its staging;
/// and finishing it commits what was staged, whatever else is in the directory.
#[test]
fn what_a_thirteenth_reviewer_found_with_many_writers_stays_fixed() {
  if std::env::var("NV_SERVER").is_err() {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  }
  // Staged, then pulled: Git refuses the pull, and so does nv, where the ref changed the file.
  let (_, run) = two_writers("thirteenth-staged");
  assert_eq!(run("w1", "sed -i 's/^1$/1 by w1/' a.txt && $NV git commit -qam w1 && $NV git push -q").0, 0);
  assert_eq!(run("w2", "sed -i 's/^6$/6 by w2/' a.txt && $NV git add a.txt").0, 0);
  let (code, said) = run("w2", "$NV git pull --rebase");
  assert_eq!(code, 128, "{said}");
  assert!(said.contains("cannot pull with rebase: Your index contains uncommitted changes."), "{said}");
  let (code, said) = run("w2", "$NV sync .");
  assert_ne!(code, 0, "{said}");
  assert!(said.contains("staged") && said.contains("a.txt"), "{said}");
  assert_eq!(run("w2", "cat a.txt").1, "1\n2\n3\n4\n5\n6 by w2\n");
  assert_eq!(run("w2", "$NV git status --porcelain").1, "M  a.txt\n");
  let (code, said) = run("w2", "$NV git commit -qm w2 && $NV git pull --rebase -q && $NV git push -q");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("w1", "$NV git pull --rebase -q && cat a.txt").1, "1 by w1\n2\n3\n4\n5\n6 by w2\n");
  // A file staged that the ref did not change is no reason to refuse nv's own sync: it stays staged.
  assert_eq!(run("w2", "echo y > b.txt && $NV git add b.txt").0, 0);
  assert_eq!(run("w1", "echo 7 >> a.txt && $NV git commit -qam seven && $NV git push -q").0, 0);
  let (code, said) = run("w2", "$NV sync .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("w2", "$NV git status --porcelain").1, "M  b.txt\n");

  // A pull that stopped, settled and added: nothing moves until it is finished, and finishing it
  // commits what was added, untracked files or not.
  let (_, run) = two_writers("thirteenth-stopped");
  assert_eq!(run("w1", "echo W1a >> a.txt && $NV git commit -qam w1a && $NV git push -q").0, 0);
  assert_eq!(run("w2", "echo W2a >> a.txt && $NV git commit -qam w2a").0, 0);
  assert_eq!(run("w2", "$NV git pull --rebase").0, 1);
  assert_eq!(run("w2", "sed -i '/^[<=>]\\{7\\}/d' a.txt").0, 0);
  assert_eq!(run("w1", "echo W1b >> b.txt && $NV git commit -qam w1b && $NV git push -q").0, 0);
  // Settled on disk and not added: as unfinished as added and not committed.
  for (what, step) in [("settled", ":"), ("added", "$NV git add a.txt")] {
    assert_eq!(run("w2", step).0, 0);
    for verb in ["$NV git pull --rebase", "$NV git push", "$NV push .", "$NV sync ."] {
      let (code, said) = run("w2", verb);
      assert_ne!(code, 0, "{what}: {verb}\n{said}");
      assert!(said.contains("a.txt"), "{what}: {verb}\n{said}");
    }
  }
  assert_eq!(run("w2", "$NV git log --format=%s").1, "w2a\nw1a\ninit\n");
  assert_eq!(run("w2", "echo notes > u.txt && $NV git rebase --continue").0, 0);
  assert_eq!(run("w2", "$NV git status --porcelain").1, "?? u.txt\n");
  assert_eq!(run("w2", "$NV git log --format=%s").1, "w2a\nw1a\ninit\n");
  let (code, said) = run("w2", "$NV git pull --rebase -q && $NV git push -q");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("w1", "$NV git pull --rebase -q && cat a.txt b.txt").1, "1\n2\n3\n4\n5\n6\nW2a\nW1a\nx\nW1b\n");

  // And given a message there: that message, with only what was added.
  let (_, run) = two_writers("thirteenth-reworded");
  assert_eq!(run("w1", "echo W1a >> a.txt && $NV git commit -qam w1a && $NV git push -q").0, 0);
  assert_eq!(run("w2", "echo W2a >> a.txt && $NV git commit -qam w2a").0, 0);
  assert_eq!(run("w2", "$NV git pull --rebase").0, 1);
  let (code, said) =
    run("w2", "sed -i '/^[<=>]\\{7\\}/d' a.txt && $NV git add a.txt && echo notes > u.txt && $NV git commit -q -m 'w2a, settled'");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("w2", "$NV git status --porcelain").1, "?? u.txt\n");
  assert_eq!(run("w2", "$NV git log --format=%s").1, "w2a, settled\nw1a\ninit\n");

  // Undone instead: the files, HEAD and the index as they were before the pull.
  for undo in ["$NV git rebase --abort", "$NV restore . before-sync"] {
    let (_, run) = two_writers("thirteenth-undone");
    assert_eq!(run("w1", "echo W1a >> a.txt && $NV git commit -qam w1a && $NV git push -q").0, 0);
    assert_eq!(run("w2", "echo W2a >> a.txt && $NV git commit -qam w2a").0, 0);
    assert_eq!(run("w2", "$NV git pull --rebase").0, 1);
    assert_eq!(run("w2", "sed -i '/^[<=>]\\{7\\}/d' a.txt && $NV git add a.txt && echo n > n.txt && $NV git add n.txt").0, 0);
    let (code, said) = run("w2", undo);
    assert_eq!(code, 0, "{undo}: {said}");
    let (_, status) = run("w2", "$NV git status --porcelain");
    assert!(status.lines().all(|l| l.starts_with("??")), "{undo}: {status}");
    assert_eq!(run("w2", "cat a.txt").1, "1\n2\n3\n4\n5\n6\nW2a\n", "{undo}");
    assert_eq!(run("w2", "$NV git commit -qm next").0, 1, "{undo}");
  }
}

/// A name with a newline in it is a name like any other to what a sync notes of the files it left
/// with both versions: a push is refused while it holds them.
#[test]
fn a_file_left_unsettled_under_a_name_with_a_newline_is_still_unsettled() {
  if std::env::var("NV_SERVER").is_err() {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  }
  let (_, run) = two_writers("thirteenth-newline");
  assert_eq!(run("w1", "printf 'a\\n' > 'new\nline.txt' && $NV commit . 'adds it' >/dev/null && $NV push . >/dev/null").0, 0);
  assert_eq!(run("w2", "$NV sync . >/dev/null").0, 0);
  assert_eq!(run("w1", "printf 'w1\\n' > 'new\nline.txt' && $NV commit . w1 >/dev/null && $NV push . >/dev/null").0, 0);
  assert_eq!(run("w2", "printf 'w2\\n' > 'new\nline.txt' && $NV commit . w2 >/dev/null").0, 0);
  let (code, said) = run("w2", "$NV sync .");
  assert_ne!(code, 0, "{said}");
  let (code, said) = run("w2", "$NV push .");
  assert_ne!(code, 0, "{said}");
  // Settled, it completes the commit the sync held it out of.
  let (code, said) = run("w2", "printf 'w1 w2\\n' > 'new\nline.txt' && $NV commit .");
  assert_eq!(code, 0, "{said}");
  assert_eq!(run("w2", "$NV push . >/dev/null").0, 0);
  assert_eq!(run("w1", "$NV sync . >/dev/null && cat 'new\nline.txt'").1, "w1 w2\n");
}

/// `git fetch` brings what `log origin/<ref>` needs to name each commit that came, and names the
/// ref's tip even where the workspace learnt only then where it stands on it.
#[test]
fn git_fetch_gives_origin_every_commit_and_the_tip() {
  if std::env::var("NV_SERVER").is_err() {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  }
  let (_, run) = two_writers("thirteenth-fetch");
  assert_eq!(
    run("w1", "echo W1a >> a.txt && $NV git commit -qam w1a && echo W1b >> a.txt && $NV git commit -qam w1b && $NV git push -q").0,
    0
  );
  let (code, said) = run("w2", "$NV git fetch -q && $NV git log --format=%s origin/main");
  assert_eq!((code, said.as_str()), (0, "w1b\nw1a\ninit\n"));
  assert_eq!(run("w2", "$NV git status -sb").1.lines().next().unwrap(), "## main...origin/main [behind 2]");
  // A ref this workspace's own push started, whose answer it never heard: it does not yet know
  // it stands on the ref, and the fetch is where it learns that.
  let (_, run) = two_writers("thirteenth-fetch-unheard");
  assert_eq!(run("w2", "echo W2 >> b.txt && $NV git commit -qam w2 && $NV git push -q").0, 0);
  assert_eq!(run("w2", "sed -i '/^pushed /d; s/^seq .*/seq 0/' .nv/workspace").0, 0);
  let (code, said) = run("w2", "$NV git fetch -q && $NV git rev-parse origin/main && $NV git rev-parse HEAD");
  assert_eq!(code, 0, "{said}");
  let ids: Vec<&str> = said.lines().collect();
  assert_eq!(ids[0], ids[1], "{said}");
}

/// `restore` names the state that keeps what it wrote over, whether that state was recorded by
/// the restore or before it, by a push.
#[test]
fn restore_after_a_push_names_where_the_edit_is_kept() {
  if std::env::var("NV_SERVER").is_err() {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  }
  let (_, run) = two_writers("thirteenth-restore");
  assert_eq!(
    run("w2", "echo c >> a.txt && $NV git commit -qam c1 && $NV mark . m1 >/dev/null && echo EDIT >> b.txt && $NV git push -q").0,
    0
  );
  let (code, said) = run("w2", "$NV restore . m1");
  assert_eq!(code, 0, "{said}");
  let id = said.split("kept as ").nth(1).map(str::trim).unwrap_or_else(|| panic!("{said}")).to_string();
  assert_eq!(run("w2", &format!("$NV restore . {id} >/dev/null && cat b.txt")).1, "x\nEDIT\n");
}

/// What a thirteenth reviewer found of merge drivers under a sync: one that never returns is
/// stopped with the command and writes nothing after it; one killed by a signal is no result; and
/// markers a driver leaves are a file to settle, whatever it says.
#[test]
fn what_a_thirteenth_reviewer_found_in_drivers_stays_fixed() {
  if std::env::var("NV_SERVER").is_err() {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  }
  let clash = |name: &str, driver: &str| {
    let (root, run) = two_writers(name);
    assert_eq!(run("w1", "echo 'a.txt merge=x' > .gitattributes && $NV commit . rules >/dev/null && $NV push . >/dev/null").0, 0);
    assert_eq!(run("w2", "$NV sync . >/dev/null").0, 0);
    assert_eq!(run("w1", "sed -i 's/^1$/w1/' a.txt && $NV commit . w1 >/dev/null && $NV push . >/dev/null").0, 0);
    assert_eq!(
      run("w2", &format!("sed -i 's/^1$/w2/' a.txt && $NV commit . w2 >/dev/null && $NV git config merge.x.driver {driver:?}")).0,
      0
    );
    (root, run)
  };
  // Never returns, then writes: stopped with the sync, and with it, its writing.
  let (root, run) = clash("thirteenth-driver-hangs", "sleep 3; echo LATE > b.txt; cat %B > %A");
  let (code, said) = run("w2", "NV_TIMEOUT=1 $NV sync .");
  assert_eq!(code, 124, "{said}");
  std::thread::sleep(std::time::Duration::from_secs(4));
  assert_eq!(std::fs::read_to_string(root.join("w2/b.txt")).unwrap(), "x\n");
  // What it left of its own goes when a driver next runs.
  assert_eq!(run("w2", "$NV git config merge.x.driver 'cat %B > %A' && $NV sync . >/dev/null && ls .nv | grep -c merge-").1, "0\n");

  // Killed by a signal half way: the sync merges as text.
  let (root, run) = clash("thirteenth-driver-killed", "echo half > %A; kill -9 $$");
  let (code, said) = run("w2", "$NV sync .");
  assert_eq!(code, 1, "{said}");
  let text = std::fs::read_to_string(root.join("w2/a.txt")).unwrap();
  assert!(text.contains("<<<<<<< yours") && text.contains("w1") && !text.contains("half"), "{text}");

  // Out of its process group in a session of its own, and still stopped with it (second pass).
  let (root, run) = clash("thirteenth-driver-setsid", "setsid sh -c 'sleep 2; echo LATE > b.txt' & sleep 100");
  let (code, said) = run("w2", "NV_MERGE_DRIVER_TIMEOUT=1 $NV sync .");
  assert_eq!(code, 2, "{said}");
  std::thread::sleep(std::time::Duration::from_secs(3));
  assert_eq!(std::fs::read_to_string(root.join("w2/b.txt")).unwrap(), "x\n");

  // Killed inside a shell, which says so by an exit above 128: no result, as Git has it.
  let (root, run) = clash("thirteenth-driver-wrapped", "echo half > %A; exit 137");
  let (code, said) = run(
    "w2",
    "$NV git checkout -q -b other HEAD~1 && sed -i 's/^1$/other/' a.txt && $NV git commit -qam other && $NV git checkout -q main && $NV git merge -q other",
  );
  assert_ne!(code, 0, "{said}");
  let text = std::fs::read_to_string(root.join("w2/a.txt")).unwrap();
  assert!(!text.contains("half") && text.starts_with("w2\n"), "{text}");

  // Exit 0 with markers: a file to settle, and no push until it is.
  let (_, run) = clash("thirteenth-driver-marks", "printf '<<<<<<< yours\\nA\\n=======\\nB\\n>>>>>>> theirs\\n' > %A");
  let (code, said) = run("w2", "$NV sync .");
  assert_eq!(code, 1, "{said}");
  assert_ne!(run("w2", "$NV push .").0, 0);
}

/// What the thirteenth round's second pass found with several writers: what was added at a pull
/// that stopped stays committed whatever commit comes after; a completion by `nv commit` spends
/// the staging; and what a push's catching-up unstages is kept.
#[test]
fn what_a_second_pass_found_with_many_writers_stays_fixed() {
  if std::env::var("NV_SERVER").is_err() {
    eprintln!("NV_SERVER is not set: skipping the test against a server");
    return;
  }
  let stopped = |name: &str| {
    let (root, run) = two_writers(name);
    assert_eq!(run("w1", "echo W1a >> a.txt && $NV git commit -qam w1a && $NV git push -q").0, 0);
    assert_eq!(run("w2", "echo W2a >> a.txt && $NV git commit -qam w2a && echo W2b >> b.txt && $NV git commit -qam w2b").0, 0);
    assert_eq!(run("w2", "$NV git pull --rebase").0, 1);
    assert_eq!(run("w2", "sed -i '/^[<=>]\\{7\\}/d' a.txt").0, 0);
    (root, run)
  };
  // Added to a file a later commit changed: committed, not taken back out by that commit.
  for finish in ["$NV git rebase --continue", "$NV git commit -qm 'w2a again'"] {
    let (_, run) = stopped("second-pass-added");
    let (code, said) = run("w2", &format!("echo ADDED >> b.txt && $NV git add a.txt b.txt && {finish}"));
    assert_eq!(code, 0, "{finish}: {said}");
    assert_eq!(run("w2", "$NV git show HEAD:b.txt").1, "x\nW2b\nADDED\n", "{finish}");
    assert_eq!(run("w2", "$NV git status --porcelain").1, "", "{finish}");
  }
  // Completed by `nv commit`, with files added on the way: nothing is left staged.
  let (_, run) = stopped("second-pass-nv-commit");
  assert_eq!(run("w2", "$NV git add a.txt && $NV commit . >/dev/null").0, 0);
  assert_eq!(run("w1", "echo c > c.txt && $NV git add c.txt && $NV git commit -qm w1c && $NV git push -q").0, 0);
  let (code, said) = run("w2", "$NV git pull --rebase -q");
  assert_eq!(code, 0, "{said}");

  // Staged, then taken off disk, then a push that catches up on that file: the staged copy is kept.
  let (_, run) = two_writers("second-pass-unstaged");
  assert_eq!(run("w1", "sed -i 's/^1$/w1/' a.txt && $NV git commit -qam w1 && $NV git push -q").0, 0);
  assert_eq!(
    run("w2", "echo y > b.txt && $NV git commit -qam w2 && echo STAGED >> a.txt && $NV git add a.txt && sed -i '/STAGED/d' a.txt").0,
    0
  );
  let (code, said) = run("w2", "$NV push .");
  assert_eq!(code, 0, "{said}");
  let kept = said.split("is kept as ").nth(1).map(|t| t.split_whitespace().next().unwrap().to_string()).unwrap_or_else(|| panic!("{said}"));
  assert!(!said.contains("before-sync"), "{said}");
  let (_, shown) = run("w2", &format!("$NV since . {kept}"));
  assert!(shown.contains("a.txt"), "{shown}");
}
