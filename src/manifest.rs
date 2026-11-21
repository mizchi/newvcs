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
use std::collections::{BTreeMap, HashSet};

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

/// Whether a path may be in a tree at all. A commit is bytes that may have
/// come from anyone, and whoever takes it in writes its paths: so a path
/// stays inside the workspace by its form alone (no empty part, no `.` or
/// `..`, not from the root), is nobody's own directory (`.git`, `.nv`, in
/// any case of letters), has no NUL in it, and has no part longer than a
/// file system will take. These are Git's rules for a tree, and one more.
pub fn path_ok(path: &[u8]) -> bool {
  !path.is_empty() && path.len() <= 4096 && path.split(|&b| b == b'/').all(name_ok)
}

/// Whether one name, an entry of a directory, may be in a tree: not empty, not `.` or `..`,
/// no `/` and no NUL in it, and not a name a file system takes for `.git` or `.nv`. Those are
/// what a checkout would write into Git's directory or nv's own through, from a tree that came
/// from somebody else.
pub fn name_ok(name: &[u8]) -> bool {
  !name.is_empty() && name.len() <= 255 && name != b"." && name != b".." && !name.contains(&b'/') && !name.contains(&0) && !reserved(name)
}

/// A name that is `.git` or `.nv` to some file system: in any case; with characters HFS+ ignores
/// in it; with the dots and spaces NTFS drops from the end; as NTFS's short name; or in letters
/// that are those letters once Unicode compatibility folding is done.
fn reserved(name: &[u8]) -> bool {
  use unicode_normalization::UnicodeNormalization;
  let text = String::from_utf8_lossy(name);
  let ignorable = |c: &char| matches!(*c as u32, 0x200c..=0x200f | 0x202a..=0x202e | 0x206a..=0x206f | 0xfeff);
  let folded: String = text.chars().filter(|c| !ignorable(c)).collect::<String>().nfkc().collect::<String>().to_lowercase();
  let trimmed = folded.trim_end_matches(['.', ' ']);
  let short = |stem: &str| {
    trimmed
      .strip_prefix(stem)
      .and_then(|rest| rest.strip_prefix('~'))
      .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
  };
  trimmed == ".git" || trimmed == ".nv" || short("git") || short("nv")
}

/// The modes an entry may have: a file (with whatever permissions Git has ever
/// written: 644, 755, and in its first weeks 664), a link, a directory, another
/// repository's commit. What kind of thing it is must be one of those.
pub fn mode_ok(mode: u32) -> bool {
  match mode & 0o170000 {
    0o100000 => mode & !0o170777 == 0,
    _ => matches!(mode, MODE_LINK | MODE_DIR | MODE_GITLINK),
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

/// A tree, from its bytes. It is checked as a commit is: a tree may have come from a server, and
/// whatever its names say is where its files are written.
pub fn decode(mut bytes: &[u8]) -> Result<Dir> {
  let mut dir = Dir::new();
  let mut last: Option<Vec<u8>> = None;
  while !bytes.is_empty() {
    let bad = || "a tree object is cut short".to_string();
    let mode = u32::from_le_bytes(bytes.get(..4).ok_or_else(bad)?.try_into().unwrap());
    let len = u32::from_le_bytes(bytes.get(4..8).ok_or_else(bad)?.try_into().unwrap()) as usize;
    let name = bytes.get(8..8usize.saturating_add(len)).ok_or_else(bad)?.to_vec();
    let id: Id = bytes.get(8 + len..8 + len + 32).ok_or_else(bad)?.try_into().unwrap();
    if !name_ok(&name) {
      return Err(format!("a tree names an entry `{}`, which no tree may have", String::from_utf8_lossy(&name)));
    }
    if !mode_ok(mode) {
      return Err(format!("a tree gives `{}` a mode no file has", String::from_utf8_lossy(&name)));
    }
    // In order and once each, as every tree is written.
    if last.as_ref().is_some_and(|last| *last >= name) {
      return Err(format!("a tree has `{}` out of order, or twice", String::from_utf8_lossy(&name)));
    }
    last = Some(name.clone());
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
    let below = match own {
      // The name becomes that directory, and the paths below it are changes to it.
      Some(Some(entry)) if entry.is_dir() && !nested.is_empty() => Some(entry.id),
      // The name becomes a file, or a directory as it is. Whatever else is said about
      // paths below a file can only be the removal of the directory that was there.
      Some(Some(entry)) => {
        dir.insert(name.to_vec(), entry);
        continue;
      }
      // What was there goes away, perhaps to make room for a directory of that name.
      Some(None) => {
        dir.remove(name);
        None
      }
      None => dir.get(name).filter(|e| e.is_dir()).map(|e| e.id),
    };
    if nested.is_empty() {
      continue;
    }
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
  diff_keeping(store, a, b, &HashSet::new())
}

/// `diff`, except that a directory of `b` whose id is in `whole` is one
/// change, however many files it has: it was put there as that directory,
/// and is still exactly it.
pub fn diff_keeping(store: &mut Store, a: Option<Id>, b: Option<Id>, whole: &HashSet<Id>) -> Result<Vec<Change>> {
  fn go(store: &mut Store, a: Option<Id>, b: Option<Id>, whole: &HashSet<Id>, at: &mut Vec<u8>, out: &mut Vec<Change>) -> Result<()> {
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
      if let Some(kept) = n.filter(|e| e.is_dir() && whole.contains(&e.id)) {
        // Whatever was there, file or directory, this takes its place.
        out.push((at.clone(), Some(kept)));
      } else {
        // A file that is gone or replaced by a directory is removed; one that appears or changed is set.
        match (o.filter(|e| !e.is_dir()), n.filter(|e| !e.is_dir())) {
          (_, Some(file)) => out.push((at.clone(), Some(file))),
          (Some(_), None) => out.push((at.clone(), None)),
          (None, None) => {}
        }
        go(store, dir_of(o), dir_of(n), whole, at, out)?;
      }
      at.truncate(len);
    }
    Ok(())
  }
  let mut out = Vec::new();
  go(store, a, b, whole, &mut Vec::new(), &mut out)?;
  Ok(out)
}

/// `changes`, made on `root`, with every change to a whole directory opened
/// into the files it sets and removes. For whoever has to touch the files.
pub fn expand(store: &mut Store, root: Option<Id>, changes: &[Change]) -> Result<Vec<Change>> {
  let mut out = Vec::with_capacity(changes.len());
  for (path, entry) in changes {
    let was = match root {
      Some(root) => lookup(store, root, path)?,
      None => None,
    };
    let (was_dir, now_dir) = (was.filter(Entry::is_dir).map(|e| e.id), entry.filter(Entry::is_dir).map(|e| e.id));
    if was_dir.is_none() && now_dir.is_none() {
      out.push((path.clone(), *entry));
      continue;
    }
    // A file on either side is a change of its own; the directories are opened against each other.
    match (was.filter(|e| !e.is_dir()), entry.filter(|e| !e.is_dir())) {
      (_, Some(file)) => out.push((path.clone(), Some(file))),
      (Some(_), None) => out.push((path.clone(), None)),
      (None, None) => {}
    }
    for (below, entry) in diff(store, was_dir, now_dir)? {
      out.push(([path.as_slice(), b"/", below.as_slice()].concat(), entry));
    }
  }
  Ok(out)
}

/// The ids of the directories that `changes` set whole.
pub fn whole_dirs<'a>(changes: impl IntoIterator<Item = &'a Change>) -> impl Iterator<Item = Id> {
  changes.into_iter().filter_map(|(_, entry)| entry.filter(Entry::is_dir).map(|e| e.id))
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
    Store::create(&dir).unwrap()
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

  /// A tree to stand in for a directory of another repository.
  fn library(s: &mut Store) -> Id {
    apply(s, None, &[change("a", file(1)), change("deep/b", file(2)), change("deep/c", file(3))]).unwrap().unwrap()
  }

  #[test]
  fn a_change_can_set_a_whole_directory() {
    let mut s = store("dir-change");
    let lib = library(&mut s);
    let dir = Some(Entry { mode: MODE_DIR, id: lib });
    let root = apply(&mut s, None, &[change("src/main", file(9)), change("vendor/lib", file(8))]).unwrap();
    // Where a file was, where nothing was, and with changes below it in the same list.
    let linked =
      apply(&mut s, root, &[change("vendor/lib", dir), change("other", dir), change("other/deep/b", None), change("other/new", file(7))])
        .unwrap()
        .unwrap();
    assert_eq!(lookup(&mut s, linked, b"vendor/lib").unwrap(), dir);
    assert_eq!(
      paths(&mut s, linked, &[]),
      ["other/a", "other/deep/c", "other/new", "src/main", "vendor/lib/a", "vendor/lib/deep/b", "vendor/lib/deep/c"]
    );
    // And a directory goes away in one change.
    let gone = apply(&mut s, Some(linked), &[change("vendor/lib", None)]).unwrap().unwrap();
    assert_eq!(paths(&mut s, gone, &[]), ["other/a", "other/deep/c", "other/new", "src/main"]);
  }

  #[test]
  fn a_directory_named_whole_stays_one_change_in_a_diff_and_opens_into_its_files() {
    let mut s = store("dir-diff");
    let lib = library(&mut s);
    let dir = Some(Entry { mode: MODE_DIR, id: lib });
    let before = apply(&mut s, None, &[change("src/main", file(9)), change("vendor/lib", file(8))]).unwrap();
    let after = apply(&mut s, before, &[change("vendor/lib", dir), change("src/main", file(10))]).unwrap();
    let kept: std::collections::HashSet<Id> = [lib].into();
    let whole = diff_keeping(&mut s, before, after, &kept).unwrap();
    assert_eq!(whole, [change("src/main", file(10)), change("vendor/lib", dir)]);
    assert_eq!(apply(&mut s, before, &whole).unwrap(), after);
    // Nobody named it whole: every file, as before.
    assert_eq!(diff(&mut s, before, after).unwrap().len(), 5);
    // Opened against what was there: the file it replaces goes, its files arrive.
    let files = expand(&mut s, before, &whole).unwrap();
    assert_eq!(files, diff(&mut s, before, after).unwrap());
    // Changed since: no longer that directory, so its changes are listed.
    let touched = apply(&mut s, after, &[change("vendor/lib/a", file(11))]).unwrap();
    assert_eq!(diff_keeping(&mut s, after, touched, &kept).unwrap(), [change("vendor/lib/a", file(11))]);
    // Another directory taking its place is opened against the one that was there.
    let other = apply(&mut s, Some(lib), &[change("a", file(12))]).unwrap().unwrap();
    let replaced = apply(&mut s, after, &[change("vendor/lib", Some(Entry { mode: MODE_DIR, id: other }))]).unwrap();
    let step = [change("vendor/lib", Some(Entry { mode: MODE_DIR, id: other }))];
    assert_eq!(expand(&mut s, after, &step).unwrap(), [change("vendor/lib/a", file(12))]);
    assert_eq!(apply(&mut s, after, &step).unwrap(), replaced);
  }

  #[test]
  fn a_tree_whose_names_lead_out_is_refused() {
    let tree = |name: &[u8], mode: u32| {
      let mut dir = Dir::new();
      dir.insert(name.to_vec(), Entry { mode, id: [7; 32] });
      encode(&dir)
    };
    for name in [
      &b"../../outside/pwned"[..],
      b"..",
      b".",
      b"",
      b"a/b",
      b".git",
      b".GIT",
      b".Git.",
      b".git ",
      ".g\u{200c}it".as_bytes(),
      "\u{feff}.git".as_bytes(),
      ".\u{ff47}it".as_bytes(),
      b"GIT~1",
      b".nv",
      b".NV",
      b"nv~1",
      b"a\0b",
    ] {
      assert!(decode(&tree(name, 0o100644)).is_err(), "{:?} was taken as a name", String::from_utf8_lossy(name));
    }
    assert!(decode(&tree(b"ok.txt", 0o104755)).is_err(), "a set-uid mode");
    assert!(decode(&tree(b"ok.txt", 0)).is_err(), "mode 0");
    for name in [&b".gitignore"[..], b".github", b"git", b".gitx", b"x.git", b".nvmrc", "caf\u{e9}".as_bytes()] {
      assert!(decode(&tree(name, 0o100644)).is_ok(), "{:?} was refused", String::from_utf8_lossy(name));
    }
    // Twice, and out of order.
    let mut twice = tree(b"same", 0o100644);
    twice.extend(tree(b"same", MODE_DIR));
    assert!(decode(&twice).is_err());
    let mut backwards = tree(b"b", 0o100644);
    backwards.extend(tree(b"a", 0o100644));
    assert!(decode(&backwards).is_err());
  }
}
