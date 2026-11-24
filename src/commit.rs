//! A commit as a list of changes.
//!
//! Git names a commit by the hash of its whole tree, so nobody can make a
//! commit without knowing, at least by hash, every directory of the
//! repository. Here a commit is its parents and the paths it changed
//! relative to its first parent, and nothing else. A writer who holds one
//! subdirectory can commit to it; the full tree is derived afterwards by
//! whoever has the parent's tree (`manifest::apply`), and cached.
//!
//! A change sets a path to a file, removes what is there, or sets it to a
//! whole directory by that directory's id. The last is what a link is
//! (`links`): one change however many files the directory has, for which
//! the directory's tree objects have to be in the store.
//!
//! `rest` is everything of a Git commit object after its `tree` and `parent`
//! lines, byte for byte: author, committer, signatures, message. With the
//! derived tree it is enough to write the Git commit back unchanged.

use crate::manifest::{Change, Entry};
use crate::store::{Kind, Store};
use crate::{Id, Result};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Commit {
  pub parents: Vec<Id>,
  pub rest: Vec<u8>,
  pub changes: Vec<Change>,
}

fn put_u32(out: &mut Vec<u8>, n: usize) {
  out.extend_from_slice(&(n as u32).to_le_bytes());
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
  fn take(&mut self, n: usize) -> Result<&'a [u8]> {
    if self.0.len() < n {
      return Err("a commit object is cut short".to_string());
    }
    let (head, tail) = self.0.split_at(n);
    self.0 = tail;
    Ok(head)
  }
  fn u32(&mut self) -> Result<usize> {
    Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
  }
}

impl Commit {
  pub fn encode(&self) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, self.parents.len());
    for p in &self.parents {
      out.extend_from_slice(p);
    }
    put_u32(&mut out, self.rest.len());
    out.extend_from_slice(&self.rest);
    put_u32(&mut out, self.changes.len());
    for (path, entry) in &self.changes {
      put_u32(&mut out, path.len());
      out.extend_from_slice(path);
      match entry {
        Some(e) => {
          out.push(1);
          out.extend_from_slice(&e.mode.to_le_bytes());
          out.extend_from_slice(&e.id);
        }
        None => out.push(0),
      }
    }
    out
  }

  pub fn decode(bytes: &[u8]) -> Result<Commit> {
    let mut c = Cursor(bytes);
    let mut parents = Vec::new();
    for _ in 0..c.u32()? {
      parents.push(c.take(32)?.try_into().unwrap());
    }
    let len = c.u32()?;
    let rest = c.take(len)?.to_vec();
    let mut changes = Vec::new();
    for _ in 0..c.u32()? {
      let len = c.u32()?;
      let path = c.take(len)?.to_vec();
      let entry = match c.take(1)?[0] {
        0 => None,
        _ => Some(Entry { mode: u32::from_le_bytes(c.take(4)?.try_into().unwrap()), id: c.take(32)?.try_into().unwrap() }),
      };
      // A commit may have come from anyone. Its paths will be written by whoever takes it in.
      if !crate::manifest::path_ok(&path) {
        return Err(format!("a commit names `{}`, which is not a path a tree may have", String::from_utf8_lossy(&path)));
      }
      if entry.is_some_and(|entry| !crate::manifest::mode_ok(entry.mode)) {
        return Err(format!("a commit gives `{}` a mode no file has", String::from_utf8_lossy(&path)));
      }
      changes.push((path, entry));
    }
    let mut named: Vec<&[u8]> = changes.iter().map(|(path, _)| path.as_slice()).collect();
    named.sort_unstable();
    if named.windows(2).any(|pair| pair[0] == pair[1]) {
      return Err("a commit says two things of one path".to_string());
    }
    Ok(Commit { parents, rest, changes })
  }

  pub fn load(store: &mut Store, id: &Id) -> Result<Commit> {
    let bytes = store.get(id)?.ok_or_else(|| format!("commit {} is not in the store", crate::hex(id)))?;
    Commit::decode(&bytes)
  }

  pub fn store(&self, store: &mut Store) -> Result<Id> {
    store.put(Kind::Commit, &self.encode(), None)
  }
}

/// The root tree of a commit: its first parent's tree with its changes
/// applied. Remembered once computed.
pub fn root_of(store: &mut Store, id: &Id) -> Result<Option<Id>> {
  // Walk back to a commit whose root is known, then forward again.
  let mut todo = Vec::new();
  let mut at = *id;
  let mut root = loop {
    if let Some(root) = store.root_known(&at) {
      break root;
    }
    let commit = Commit::load(store, &at)?;
    let parent = commit.parents.first().copied();
    todo.push((at, commit));
    match parent {
      Some(p) => at = p,
      None => break None,
    }
  };
  while let Some((id, commit)) = todo.pop() {
    root = crate::manifest::apply(store, root, &commit.changes)?;
    store.set_root(id, root)?;
  }
  Ok(root)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A commit is bytes that may have come from anyone: one that names a path no tree may have is no commit.
  #[test]
  fn a_commit_that_names_a_path_no_tree_may_have_is_refused() {
    let with = |path: &[u8], mode: u32| {
      Commit { parents: vec![], rest: b"r".to_vec(), changes: vec![(path.to_vec(), Some(Entry { mode, id: [1; 32] }))] }.encode()
    };
    for path in [
      &b""[..],
      b"d/",
      b"../escape.txt",
      b"src/../../escape.txt",
      b"./a.txt",
      b"a//b",
      b"/etc/hosts",
      b".git/hooks/pre-commit",
      b"sub/.GIT/config",
      b".nv/workspace",
      b"sub/.Nv/x",
      b"a\0b",
    ] {
      assert!(Commit::decode(&with(path, 0o100644)).is_err(), "{:?} was taken as a path", String::from_utf8_lossy(path));
    }
    let long = vec![b'x'; 300];
    assert!(Commit::decode(&with(&long, 0o100644)).is_err());
    for mode in [0, 0o777, 0o200644, 0o120777, 0o40755] {
      assert!(Commit::decode(&with(b"a.txt", mode)).is_err(), "{mode:o} was taken as a mode");
    }
    // What Git has: a name that is not text, a file that is a link, a directory, another repository's commit.
    for (path, mode) in [
      (&b"caf\xff.txt"[..], 0o100644),
      (b"bin/run", 0o100755),
      (b"link", 0o120000),
      (b"vendor/lib", 0o40000),
      (b"sub", 0o160000),
      (b".gitignore", 0o100644),
      (b"a/.github/x", 0o100644),
      (b"old", 0o100664),
    ] {
      assert!(Commit::decode(&with(path, mode)).is_ok(), "{:?} was refused", String::from_utf8_lossy(path));
    }
    // One path, said twice.
    let twice = Commit { parents: vec![], rest: b"r".to_vec(), changes: vec![(b"a".to_vec(), None), (b"a".to_vec(), None)] }.encode();
    assert!(Commit::decode(&twice).is_err());
  }

  #[test]
  fn a_commit_survives_encoding() {
    let commit = Commit {
      parents: vec![[1; 32], [2; 32]],
      rest: b"author a <a@example.com> 1 +0000\ncommitter a <a@example.com> 1 +0000\n\nmessage\n".to_vec(),
      changes: vec![(b"a/b".to_vec(), Some(Entry { mode: 0o100755, id: [3; 32] })), (b"gone".to_vec(), None)],
    };
    assert_eq!(Commit::decode(&commit.encode()).unwrap(), commit);
    assert!(Commit::decode(&commit.encode()[..20]).is_err());
  }

  /// The same commit is built in `server/test/commit.test.ts`; both sides must name it alike.
  #[test]
  fn the_id_of_a_commit_is_pinned_for_the_server() {
    let commit = Commit {
      parents: vec![[0x11; 32]],
      rest: b"author a <a@example.com> 1 +0000\ncommitter a <a@example.com> 1 +0000\n\nmessage\n".to_vec(),
      changes: vec![(b"dir/file.txt".to_vec(), Some(Entry { mode: 0o100644, id: [0x22; 32] })), (b"gone".to_vec(), None)],
    };
    let id = crate::store::object_id(Kind::Commit, &commit.encode());
    assert_eq!(crate::hex(&id), "22d0e51d694ac215a59f6b3aeaddcab4bf0b93a02f93d37d2c44d36d808093ae");
  }

  #[test]
  fn roots_are_derived_along_the_first_parent() {
    let dir = std::env::temp_dir().join(format!("newvcs-commit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = Store::create(&dir).unwrap();
    let file = |n: u8| Some(Entry { mode: 0o100644, id: [n; 32] });
    let first = Commit { parents: vec![], rest: b"1".to_vec(), changes: vec![(b"a".to_vec(), file(1)), (b"d/b".to_vec(), file(2))] }
      .store(&mut store)
      .unwrap();
    let second = Commit { parents: vec![first], rest: b"2".to_vec(), changes: vec![(b"a".to_vec(), None)] }.store(&mut store).unwrap();
    let third = Commit { parents: vec![second], rest: b"3".to_vec(), changes: vec![(b"d/b".to_vec(), None)] }.store(&mut store).unwrap();
    // Asked for the newest first: the two before it are derived on the way.
    assert_eq!(root_of(&mut store, &third).unwrap(), None);
    let root = root_of(&mut store, &second).unwrap().unwrap();
    assert_eq!(crate::manifest::lookup(&mut store, root, b"d/b").unwrap(), file(2));
    assert_eq!(crate::manifest::lookup(&mut store, root, b"a").unwrap(), None);
    let _ = std::fs::remove_dir_all(&dir);
  }
}
