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

/// What follows a commit's tree and parents in Git's commit (`restOk` on the server): an author
/// line first, and in the header, up to the first empty line, no `tree` or `parent` line and no
/// NUL. What else Git's header may hold (a committer, an encoding, a signature) is kept.
pub fn rest_ok(rest: &[u8]) -> bool {
  let end = rest.windows(2).position(|pair| pair == b"\n\n").unwrap_or(rest.len());
  let header = &rest[..end];
  header.starts_with(b"author ")
    && !header.contains(&0)
    && !header.split(|&b| b == b'\n').any(|line| line.starts_with(b"tree ") || line.starts_with(b"parent "))
}

/// Whether a commit's header is one Git's `fsck` takes, as Git checks an author or committer line
/// (`fsck_ident`): a name, ` <email>`, a date of digits not padded with zeros, and a time zone of a
/// sign and four digits, then the end of the line; the author first, the committer next, neither
/// again, and the header ended. Asked of what a server hands over, not of what a store has: a
/// history Git wrote is read as Git wrote it (second pass of the thirteenth round).
pub fn header_as_git_takes(rest: &[u8]) -> bool {
  let (header, ended) = match rest.windows(2).position(|pair| pair == b"\n\n") {
    Some(end) => (&rest[..end + 1], true),
    None => (rest, rest.ends_with(b"\n")),
  };
  if !ended || !rest_ok(rest) {
    return false;
  }
  let lines: Vec<&[u8]> = header.strip_suffix(b"\n").unwrap_or(header).split(|&b| b == b'\n').collect();
  let ident = |line: &[u8], field: &[u8]| -> bool {
    let Some(p) = line.strip_prefix(field) else { return false };
    let stop = |p: &[u8]| p.iter().position(|b| b"<>\n".contains(b)).unwrap_or(p.len());
    if p.first() == Some(&b'<') {
      return false;
    }
    let at = stop(p);
    if p.get(at) != Some(&b'<') || at == 0 || p[at - 1] != b' ' {
      return false;
    }
    let p = &p[at + 1..];
    let at = stop(p);
    if p.get(at) != Some(&b'>') {
      return false;
    }
    let Some(p) = p[at + 1..].strip_prefix(b" ") else { return false };
    let digits = p.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || (p[0] == b'0' && digits > 1) || std::str::from_utf8(&p[..digits]).ok().and_then(|d| d.parse::<u64>().ok()).is_none() {
      return false;
    }
    let Some(tz) = p[digits..].strip_prefix(b" ") else { return false };
    tz.len() == 5 && (tz[0] == b'+' || tz[0] == b'-') && tz[1..].iter().all(u8::is_ascii_digit)
  };
  lines.len() >= 2
    && ident(lines[0], b"author ")
    && ident(lines[1], b"committer ")
    && !lines[2..].iter().any(|line| line.starts_with(b"author ") || line.starts_with(b"committer "))
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
    // Git's commit is the tree, the parents, and then these bytes: a `parent` line among them would
    // be a parent of the Git commit that the ref does not have (thirteenth round).
    if !rest_ok(&rest) {
      return Err("what a commit says of who made it and why is not a commit's header and message".to_string());
    }
    let mut changes = Vec::new();
    for _ in 0..c.u32()? {
      let len = c.u32()?;
      let path = c.take(len)?.to_vec();
      // A change is spelled one way: 0 removes, 1 sets. Any other byte would give one commit
      // many ids.
      let entry = match c.take(1)?[0] {
        0 => None,
        1 => Some(Entry { mode: u32::from_le_bytes(c.take(4)?.try_into().unwrap()), id: c.take(32)?.try_into().unwrap() }),
        other => return Err(format!("a commit has a change of a kind there is none of ({other})")),
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
    // Nor sets a path as something that is not a directory and sets something below it: no tree
    // holds both, and taking the changes in order would write through a link.
    let leaves: std::collections::HashSet<&[u8]> =
      changes.iter().filter(|(_, e)| e.is_some_and(|e| !e.is_dir())).map(|(path, _)| path.as_slice()).collect();
    for (path, entry) in &changes {
      if entry.is_none() {
        continue;
      }
      let mut at = path.len();
      while let Some(slash) = path[..at].iter().rposition(|&b| b == b'/') {
        if leaves.contains(&path[..slash]) {
          return Err(format!(
            "a commit sets `{}` and something below it, `{}`",
            String::from_utf8_lossy(&path[..slash]),
            String::from_utf8_lossy(path)
          ));
        }
        at = slash;
      }
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
  const TEST_REST: &[u8] = b"author a <a@example.com> 1 +0000\ncommitter a <a@example.com> 1 +0000\n\nm\n";

  /// Second pass: headers that pass `rest_ok` and that Git's `fsck` refuses.
  #[test]
  fn a_header_is_held_to_what_git_takes_where_a_server_hands_it_over() {
    assert!(header_as_git_takes(TEST_REST));
    assert!(header_as_git_takes(b"author  <a@b> 0 +0000\ncommitter c <c@d> 1700000000 -0500\nencoding latin1\n\nm\n"));
    for rest in [
      &b"author a <a@b> 1 +0000\n\nm\n"[..],
      b"author a <a@b> 1 +0000\ncommitter garbage\n\nm\n",
      b"author a <a@b> x +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author a <a@b> 01 +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author a <a@b> 99999999999999999999999 +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author a <a@b> 1 +00\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author a <a@b> 1 +0000\r\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author a <a@b> 1 +0000\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author R\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author a<a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author a <a<b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nm\n",
      b"author a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000",
      b"author a <a@b> 1 +0000\nParent 0123\ncommitter a <a@b> 1 +0000\n\nm\n",
    ] {
      assert!(!header_as_git_takes(rest), "{}", String::from_utf8_lossy(rest));
    }
  }

  /// Thirteenth round: what follows a commit's tree and parents in Git's commit was taken as any
  /// bytes, and a `parent` line there gave every client a parent the ref did not have.
  #[test]
  fn a_commit_whose_header_is_not_one_is_refused() {
    let with = |rest: &[u8]| Commit { parents: vec![], rest: rest.to_vec(), changes: vec![] }.encode();
    for rest in [
      &b"parent 0123456789012345678901234567890123456789\nauthor a <a> 1 +0000\ncommitter a <a> 1 +0000\n\nm\n"[..],
      b"author a <a> 1 +0000\nparent 0123456789012345678901234567890123456789\ncommitter a <a> 1 +0000\n\nm\n",
      b"author a <a> 1 +0000\ntree 0123456789012345678901234567890123456789\n\nm\n",
      b"",
      b"committer a <a> 1 +0000\n\nm\n",
      b"author a\0 <a> 1 +0000\ncommitter a <a> 1 +0000\n\nm\n",
    ] {
      assert!(Commit::decode(&with(rest)).is_err(), "{}", String::from_utf8_lossy(rest));
    }
    // A message may say anything, a `parent` line included; and Git's other headers stay.
    assert!(Commit::decode(&with(b"author a <a> 1 +0000\ncommitter a <a> 1 +0000\nencoding latin1\n\nparent of x\n")).is_ok());
  }
  use super::*;

  /// Twelfth round: a commit that set `l` as a link and `l/x` as a file was served, and a client
  /// taking the changes in order would have written through the link. Nor is a change spelled in
  /// more than one way: its kind is 0 or 1, as one commit has one id.
  #[test]
  fn a_commit_that_sets_a_file_and_something_below_it_is_refused() {
    let set = |path: &[u8], mode: u32| (path.to_vec(), Some(Entry { mode, id: [1; 32] }));
    let commit = |changes: Vec<Change>| Commit { parents: vec![], rest: TEST_REST.to_vec(), changes }.encode();
    for (first, mode) in [(&b"l"[..], 0o120000), (b"a", 0o100644), (b"s", 0o160000)] {
      let below = [first, b"/x"].concat();
      assert!(Commit::decode(&commit(vec![set(first, mode), set(&below, 0o100644)])).is_err(), "{:?}", String::from_utf8_lossy(first));
    }
    // A file put where a directory was, and the other way round, are one change each and fine.
    assert!(Commit::decode(&commit(vec![set(b"a", 0o100644), (b"a/x".to_vec(), None)])).is_ok());
    assert!(Commit::decode(&commit(vec![(b"a".to_vec(), None), set(b"a/x", 0o100644)])).is_ok());
    assert!(Commit::decode(&commit(vec![set(b"a", 0o100644), set(b"ab/x", 0o100644)])).is_ok());
    let mut bytes = commit(vec![set(b"a", 0o100644)]);
    let kind = bytes.len() - 32 - 4 - 1;
    assert_eq!(bytes[kind], 1);
    bytes[kind] = 2;
    assert!(Commit::decode(&bytes).is_err(), "a change of kind 2");
  }

  /// A commit is bytes that may have come from anyone: one that names a path no tree may have is no commit.
  #[test]
  fn a_commit_that_names_a_path_no_tree_may_have_is_refused() {
    let with = |path: &[u8], mode: u32| {
      Commit { parents: vec![], rest: TEST_REST.to_vec(), changes: vec![(path.to_vec(), Some(Entry { mode, id: [1; 32] }))] }.encode()
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
    let twice = Commit { parents: vec![], rest: TEST_REST.to_vec(), changes: vec![(b"a".to_vec(), None), (b"a".to_vec(), None)] }.encode();
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
    let first = Commit { parents: vec![], rest: TEST_REST.to_vec(), changes: vec![(b"a".to_vec(), file(1)), (b"d/b".to_vec(), file(2))] }
      .store(&mut store)
      .unwrap();
    let second = Commit { parents: vec![first], rest: TEST_REST.to_vec(), changes: vec![(b"a".to_vec(), None)] }.store(&mut store).unwrap();
    let third =
      Commit { parents: vec![second], rest: TEST_REST.to_vec(), changes: vec![(b"d/b".to_vec(), None)] }.store(&mut store).unwrap();
    // Asked for the newest first: the two before it are derived on the way.
    assert_eq!(root_of(&mut store, &third).unwrap(), None);
    let root = root_of(&mut store, &second).unwrap().unwrap();
    assert_eq!(crate::manifest::lookup(&mut store, root, b"d/b").unwrap(), file(2));
    assert_eq!(crate::manifest::lookup(&mut store, root, b"a").unwrap(), None);
    let _ = std::fs::remove_dir_all(&dir);
  }
}
