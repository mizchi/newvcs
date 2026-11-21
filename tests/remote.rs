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
    let mut store = Store::open(&root.join(format!("store-{name}"))).unwrap();
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
  let url = format!("{}/remote-test-{unique}", server.trim_end_matches('/'));
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
  assert_eq!(got, Synced { commits: 1, written: 1, removed: 0, contents_fetched: 1, outside_view: 0 });
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
  assert!(cat.sync().unwrap_err().contains("README.md"));

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
  let url = format!("{}/remote-race-{unique}", server.trim_end_matches('/'));
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
