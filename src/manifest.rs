//! A directory tree as one object per directory.
//!
//! Each directory is a sorted list of `(name, mode, id)`, where the id of a
//! subdirectory is that subdirectory's own object. A change to one file
//! rewrites the directories on its path and nothing else, and a subtree can
//! be read, checked out or verified without the rest of the tree: knowing a
//! directory's id is knowing everything under it.
//!
//! Modes are Git's, kept as Git has them, so that a Git tree can be written
//! back byte for byte.

use crate::store::{Kind, Store};
use crate::{Id, Result};
use std::collections::BTreeMap;

pub const MODE_DIR: u32 = 0o40000;
pub const MODE_LINK: u32 = 0o120000;
/// A commit of another repository: a submodule. Its id is Git's, zero-padded.
pub const MODE_GITLINK: u32 = 0o160000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
  pub mode: u32,
  pub id: Id,
}

impl Entry {
  pub fn is_dir(&self) -> bool {
    self.mode == MODE_DIR
  }
}

pub type Dir = BTreeMap<Vec<u8>, Entry>;

pub fn encode(dir: &Dir) -> Vec<u8> {
  let mut out = Vec::new();
  for (name, entry) in dir {
    out.extend_from_slice(&entry.mode.to_le_bytes());
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(&entry.id);
  }
  out
}

pub fn decode(mut bytes: &[u8]) -> Result<Dir> {
  let mut dir = Dir::new();
  while !bytes.is_empty() {
    let bad = || "a tree object is cut short".to_string();
    let mode = u32::from_le_bytes(bytes.get(..4).ok_or_else(bad)?.try_into().unwrap());
    let len = u32::from_le_bytes(bytes.get(4..8).ok_or_else(bad)?.try_into().unwrap()) as usize;
    let name = bytes.get(8..8 + len).ok_or_else(bad)?.to_vec();
    let id: Id = bytes.get(8 + len..8 + len + 32).ok_or_else(bad)?.try_into().unwrap();
    dir.insert(name, Entry { mode, id });
    bytes = &bytes[8 + len + 32..];
  }
  Ok(dir)
}

pub fn load(store: &mut Store, id: &Id) -> Result<Dir> {
  let bytes = store.get(id)?.ok_or_else(|| format!("tree {} is not in the store", crate::hex(id)))?;
  decode(&bytes)
}

/// One path set to an entry, or removed.
pub type Change = (Vec<u8>, Option<Entry>);

/// The tree `root` with `changes` applied; `None` when nothing is left.
/// Only the directories on the changed paths are read and rewritten.
pub fn apply(store: &mut Store, root: Option<Id>, changes: &[Change]) -> Result<Option<Id>> {
  let mut sorted: Vec<(&[u8], Option<Entry>)> = changes.iter().map(|(p, e)| (p.as_slice(), *e)).collect();
  sorted.sort_by(|a, b| a.0.cmp(b.0));
  apply_dir(store, root, &sorted)
}

/// The changes under one name of a directory: to the entry itself, and to paths below it.
type Group<'a> = (Option<Option<Entry>>, Vec<(&'a [u8], Option<Entry>)>);

fn apply_dir(store: &mut Store, id: Option<Id>, changes: &[(&[u8], Option<Entry>)]) -> Result<Option<Id>> {
  if changes.is_empty() {
    return Ok(id);
  }
  let mut dir = match id {
    Some(id) => load(store, &id)?,
    None => Dir::new(),
  };
  // Group by first component. Sorting put `a` before `a/…`, but `a.b` may sit between them.
  let mut groups: BTreeMap<&[u8], Group> = BTreeMap::new();
  for &(path, entry) in changes {
    match path.iter().position(|&b| b == b'/') {
      None => groups.entry(path).or_default().0 = Some(entry),
      Some(slash) => groups.entry(&path[..slash]).or_default().1.push((&path[slash + 1..], entry)),
    }
  }
  for (name, (own, nested)) in groups {
    match own {
      // The name becomes a file. Whatever else is said about paths below it
      // can only be the removal of the directory that was there.
      Some(Some(entry)) => {
        dir.insert(name.to_vec(), entry);
        continue;
      }
      // A file goes away, perhaps to make room for a directory of that name.
      Some(None) => {
        dir.remove(name);
      }
      None => {}
    }
    if nested.is_empty() {
      continue;
    }
    let below = dir.get(name).filter(|e| e.is_dir()).map(|e| e.id);
    match apply_dir(store, below, &nested)? {
      Some(id) => dir.insert(name.to_vec(), Entry { mode: MODE_DIR, id }),
      None => dir.remove(name),
    };
  }
  if dir.is_empty() {
    return Ok(None);
  }
  // The directory this one replaces differs from it in a few entries.
  store.put(Kind::Tree, &encode(&dir), id).map(Some)
}

/// The changes that turn tree `a` into tree `b`. Subtrees with the same id
/// are the same subtree and are not opened.
pub fn diff(store: &mut Store, a: Option<Id>, b: Option<Id>) -> Result<Vec<Change>> {
  fn go(store: &mut Store, a: Option<Id>, b: Option<Id>, at: &mut Vec<u8>, out: &mut Vec<Change>) -> Result<()> {
    if a == b {
      return Ok(());
    }
    let mut load_or_empty = |id: Option<Id>| match id {
      Some(id) => load(store, &id),
      None => Ok(Dir::new()),
    };
    let (old, new) = (load_or_empty(a)?, load_or_empty(b)?);
    let names: std::collections::BTreeSet<&Vec<u8>> = old.keys().chain(new.keys()).collect();
    for name in names {
      let (o, n) = (old.get(name).copied(), new.get(name).copied());
      if o == n {
        continue;
      }
      let len = at.len();
      if len > 0 {
        at.push(b'/');
      }
      at.extend_from_slice(name);
      let dir_of = |e: Option<Entry>| e.filter(Entry::is_dir).map(|e| e.id);
      // A file that is gone or replaced by a directory is removed; one that appears or changed is set.
      match (o.filter(|e| !e.is_dir()), n.filter(|e| !e.is_dir())) {
        (_, Some(file)) => out.push((at.clone(), Some(file))),
        (Some(_), None) => out.push((at.clone(), None)),
        (None, None) => {}
      }
      go(store, dir_of(o), dir_of(n), at, out)?;
      at.truncate(len);
    }
    Ok(())
  }
  let mut out = Vec::new();
  go(store, a, b, &mut Vec::new(), &mut out)?;
  Ok(out)
}

/// The entry at `path`, if the tree has one.
pub fn lookup(store: &mut Store, root: Id, path: &[u8]) -> Result<Option<Entry>> {
  let mut entry = Entry { mode: MODE_DIR, id: root };
  for name in path.split(|&b| b == b'/').filter(|n| !n.is_empty()) {
    if !entry.is_dir() {
      return Ok(None);
    }
    match load(store, &entry.id)?.get(name) {
      Some(e) => entry = *e,
      None => return Ok(None),
    }
  }
  Ok(Some(entry))
}

/// Does a view made of `prefixes` include `path`, or something below it?
/// An empty list is a view of everything.
pub fn in_view(prefixes: &[Vec<u8>], path: &[u8], is_dir: bool) -> bool {
  let under =
    |path: &[u8], prefix: &[u8]| path == prefix || (path.len() > prefix.len() && path.starts_with(prefix) && path[prefix.len()] == b'/');
  prefixes.is_empty() || prefixes.iter().any(|p| under(path, p) || (is_dir && under(p, path)))
}

/// Every file of the tree that the view includes, as `(path, entry)`.
/// Directories outside the view are never read.
pub fn walk(store: &mut Store, root: Id, prefixes: &[Vec<u8>], visit: &mut dyn FnMut(&[u8], Entry) -> Result<()>) -> Result<()> {
  fn go(
    store: &mut Store,
    id: Id,
    at: &mut Vec<u8>,
    prefixes: &[Vec<u8>],
    visit: &mut dyn FnMut(&[u8], Entry) -> Result<()>,
  ) -> Result<()> {
    for (name, entry) in load(store, &id)? {
      let len = at.len();
      if len > 0 {
        at.push(b'/');
      }
      at.extend_from_slice(&name);
      if in_view(prefixes, at, entry.is_dir()) {
        if entry.is_dir() {
          go(store, entry.id, at, prefixes, visit)?;
        } else {
          visit(at, entry)?;
        }
      }
      at.truncate(len);
    }
    Ok(())
  }
  go(store, root, &mut Vec::new(), prefixes, visit)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn store(name: &str) -> Store {
    let dir = std::env::temp_dir().join(format!("newvcs-manifest-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Store::open(&dir).unwrap()
  }

  fn file(n: u8) -> Option<Entry> {
    Some(Entry { mode: 0o100644, id: [n; 32] })
  }

  fn paths(store: &mut Store, root: Id, prefixes: &[&str]) -> Vec<String> {
    let prefixes: Vec<Vec<u8>> = prefixes.iter().map(|p| p.as_bytes().to_vec()).collect();
    let mut out = Vec::new();
    walk(store, root, &prefixes, &mut |path, _| {
      out.push(String::from_utf8(path.to_vec()).unwrap());
      Ok(())
    })
    .unwrap();
    out
  }

  fn change(path: &str, entry: Option<Entry>) -> Change {
    (path.as_bytes().to_vec(), entry)
  }

  #[test]
  fn changes_build_remove_and_reshape_a_tree() {
    let mut s = store("apply");
    let root = apply(&mut s, None, &[change("a/b/c.txt", file(1)), change("a/d.txt", file(2)), change("e.txt", file(3))]).unwrap().unwrap();
    assert_eq!(paths(&mut s, root, &[]), ["a/b/c.txt", "a/d.txt", "e.txt"]);
    assert_eq!(lookup(&mut s, root, b"a/b/c.txt").unwrap(), file(1));
    assert_eq!(lookup(&mut s, root, b"a/b/missing").unwrap(), None);

    // Removing the last file of a directory removes the directory.
    let root2 = apply(&mut s, Some(root), &[change("a/b/c.txt", None)]).unwrap().unwrap();
    assert_eq!(paths(&mut s, root2, &[]), ["a/d.txt", "e.txt"]);
    // A file becomes a directory in one step, and the neighbour `e.txt.bak` sorts between.
    let root3 =
      apply(&mut s, Some(root2), &[change("e.txt", None), change("e.txt/inner", file(4)), change("e.txt.bak", file(5))]).unwrap().unwrap();
    assert_eq!(paths(&mut s, root3, &[]), ["a/d.txt", "e.txt/inner", "e.txt.bak"]);
    // And a directory becomes a file: the removals below it must not take the new file with them.
    let root4 = apply(&mut s, Some(root3), &[change("e.txt", file(6)), change("e.txt/inner", None)]).unwrap().unwrap();
    assert_eq!(paths(&mut s, root4, &[]), ["a/d.txt", "e.txt", "e.txt.bak"]);
    assert_eq!(lookup(&mut s, root4, b"e.txt").unwrap(), file(6));
    assert_eq!(
      apply(&mut s, Some(root3), &[change("a/d.txt", None), change("e.txt/inner", None), change("e.txt.bak", None)]).unwrap(),
      None
    );
  }

  #[test]
  fn the_same_tree_has_the_same_id_however_it_was_built() {
    let mut s = store("ids");
    let once = apply(&mut s, None, &[change("x/1", file(1)), change("x/2", file(2)), change("y", file(3))]).unwrap();
    let first = apply(&mut s, None, &[change("y", file(3)), change("x/2", file(9))]).unwrap();
    let later = apply(&mut s, first, &[change("x/2", file(2)), change("x/1", file(1))]).unwrap();
    assert_eq!(once, later);
    // And an untouched subtree keeps its id when a sibling changes.
    let changed = apply(&mut s, once, &[change("y", file(7))]).unwrap().unwrap();
    assert_eq!(lookup(&mut s, changed, b"x").unwrap(), lookup(&mut s, once.unwrap(), b"x").unwrap());
  }

  #[test]
  fn a_diff_applied_to_the_old_tree_gives_the_new_one() {
    let mut s = store("diff");
    let a = apply(
      &mut s,
      None,
      &[
        change("keep/x", file(1)),
        change("d/one", file(2)),
        change("d/two", file(3)),
        change("was-file", file(4)),
        change("was-dir/in", file(5)),
      ],
    )
    .unwrap();
    let b = apply(
      &mut s,
      a,
      &[
        change("d/two", file(9)),
        change("d/three", file(8)),
        change("was-file", None),
        change("was-file/now", file(7)),
        change("was-dir", file(6)),
        change("was-dir/in", None),
      ],
    )
    .unwrap();
    let forward = diff(&mut s, a, b).unwrap();
    let names: Vec<String> =
      forward.iter().map(|(p, e)| format!("{}{}", if e.is_some() { '+' } else { '-' }, String::from_utf8_lossy(p))).collect();
    assert_eq!(names, ["+d/three", "+d/two", "+was-dir", "-was-dir/in", "-was-file", "+was-file/now"]);
    assert_eq!(apply(&mut s, a, &forward).unwrap(), b);
    let back = diff(&mut s, b, a).unwrap();
    assert_eq!(apply(&mut s, b, &back).unwrap(), a);
    assert_eq!(diff(&mut s, a, a).unwrap(), Vec::new());
    let from_nothing = diff(&mut s, None, b).unwrap();
    assert_eq!(apply(&mut s, None, &from_nothing).unwrap(), b);
  }

  #[test]
  fn a_view_walks_only_its_prefixes() {
    let mut s = store("view");
    let root = apply(
      &mut s,
      None,
      &[change("docs/a.md", file(1)), change("pkg/core/x.ts", file(2)), change("pkg/core2/y.ts", file(3)), change("pkg/ui/z.ts", file(4))],
    )
    .unwrap()
    .unwrap();
    assert_eq!(paths(&mut s, root, &["pkg/core"]), ["pkg/core/x.ts"]);
    assert_eq!(paths(&mut s, root, &["pkg/ui", "docs"]), ["docs/a.md", "pkg/ui/z.ts"]);
    assert_eq!(paths(&mut s, root, &["pkg/core/x.ts"]), ["pkg/core/x.ts"]);
    assert_eq!(paths(&mut s, root, &["nowhere"]), Vec::<String>::new());
  }
}
