//! What a workspace knows of its files: for each path, the entry of the
//! commit it was last recorded as, and the size and time the file had then.
//!
//! The index is read by every command and changed by most, so neither may
//! cost in proportion to the workspace. It is two files:
//!
//! - `index`: every entry, sorted by path, behind a table of offsets. It is
//!   mapped into memory and searched in place; opening it reads nothing.
//! - `index.log`: the entries set or removed since `index` was written,
//!   appended a record at a time.
//!
//! A command opens the map, replays the log (a handful of records) and, if
//! it changes anything, appends to the log. Recording one edited file in a
//! workspace of a hundred thousand costs one record. When the log has grown
//! to a fraction of the index the two are merged into a new `index`.

use crate::manifest::Entry;
use crate::{Id, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// What the index knows of one file. A file whose `size`, `mtime` and
/// `ctime` are still these is taken to still have the entry's id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Indexed {
  pub entry: Entry,
  pub size: u64,
  /// Nanoseconds since the epoch; 0 for "do not trust, look at the file".
  pub mtime: u128,
  /// When the file's bytes or its attributes last changed, by the file
  /// system's own account, which nobody can set: nanoseconds since the
  /// epoch. A file can be given back its size and its `mtime` after an
  /// edit; not this. 0 for "not known yet": it is noted the next time the
  /// file is looked at and found unchanged otherwise.
  pub ctime: u64,
}

/// The index file's first bytes: with `ctime` in each entry, and as it was before.
const MAGIC: &[u8; 8] = b"nvindex2";
const MAGIC_BEFORE: &[u8; 8] = b"nvindex1";
const HEADER: usize = 16;
/// mode, id, size, mtime, ctime.
const FIXED: usize = 4 + 32 + 8 + 16 + 8;
/// An entry as it was written before `ctime` was kept.
const FIXED_BEFORE: usize = 4 + 32 + 8 + 16;
/// A record of the log: a path removed, set as before `ctime` was kept, or set.
const REMOVED: u8 = 0;
const SET_BEFORE: u8 = 1;
const SET: u8 = 2;
/// The log is merged into the index once it holds this many entries, or a
/// quarter as many as the index if that is more.
const MERGE_AT: usize = 256;

fn put_fixed(out: &mut Vec<u8>, i: &Indexed) {
  out.extend_from_slice(&i.entry.mode.to_le_bytes());
  out.extend_from_slice(&i.entry.id);
  out.extend_from_slice(&i.size.to_le_bytes());
  out.extend_from_slice(&i.mtime.to_le_bytes());
  out.extend_from_slice(&i.ctime.to_le_bytes());
}

fn fixed(f: &[u8]) -> Indexed {
  let id: Id = f[4..36].try_into().unwrap();
  Indexed {
    entry: Entry { mode: u32::from_le_bytes(f[..4].try_into().unwrap()), id },
    size: u64::from_le_bytes(f[36..44].try_into().unwrap()),
    mtime: u128::from_le_bytes(f[44..60].try_into().unwrap()),
    // An entry written before `ctime` was kept has none.
    ctime: f.get(60..68).map_or(0, |bytes| u64::from_le_bytes(bytes.try_into().unwrap())),
  }
}

pub struct Index {
  dir: PathBuf,
  base: Option<memmap2::Mmap>,
  count: usize,
  /// How long an entry of `base` is: it may have been written before `ctime` was kept.
  fixed: usize,
  /// Entries set (`Some`) or removed (`None`) since the base was written.
  overlay: BTreeMap<Vec<u8>, Option<Indexed>>,
  /// Log records made by this process and not yet appended.
  pending: Vec<u8>,
}

/// Which entries a walk of the disk came across.
pub struct Seen {
  base: Vec<bool>,
  other: BTreeSet<Vec<u8>>,
}

impl Seen {
  /// Take in what another walk of the same index came across.
  pub fn merge(&mut self, other: Seen) {
    for (mine, theirs) in self.base.iter_mut().zip(other.base) {
      *mine |= theirs;
    }
    self.other.extend(other.other);
  }
}

impl Index {
  /// Open the index kept in a workspace's metadata directory. A missing or
  /// damaged file is an empty index: everything is then looked at afresh.
  pub fn open(dir: &Path) -> Index {
    let mut index = Index { dir: dir.to_path_buf(), base: None, count: 0, fixed: FIXED, overlay: BTreeMap::new(), pending: Vec::new() };
    // SAFETY: the file is replaced by rename, never written in place.
    if let Ok(file) = std::fs::File::open(dir.join("index"))
      && let Ok(map) = unsafe { memmap2::Mmap::map(&file) }
      && map.len() >= HEADER
      && (&map[..8] == MAGIC || &map[..8] == MAGIC_BEFORE)
    {
      index.fixed = if &map[..8] == MAGIC { FIXED } else { FIXED_BEFORE };
      let count = u64::from_le_bytes(map[8..16].try_into().unwrap()) as usize;
      let table_end = HEADER.saturating_add(count.saturating_add(1).saturating_mul(8));
      let last = |m: &[u8]| u64::from_le_bytes(m[table_end - 8..table_end].try_into().unwrap()) as usize;
      if table_end <= map.len() && table_end + last(&map) == map.len() {
        // Every record where the table says it is, and long enough to be one: a table that says
        // otherwise is damage, and a damaged index is an empty one.
        let at = |k: usize| u64::from_le_bytes(map[HEADER + k * 8..HEADER + k * 8 + 8].try_into().unwrap());
        let fixed = index.fixed as u64;
        if at(0) == 0 && (0..count).all(|k| at(k + 1) >= at(k) + fixed) {
          (index.count, index.base) = (count, Some(map));
        }
      }
    }
    // A record cut short by a crash ends the replay; what came before it stands.
    let log = std::fs::read(dir.join("index.log")).unwrap_or_default();
    let mut rest = log.as_slice();
    while rest.len() >= 5 {
      let len = u32::from_le_bytes(rest[1..5].try_into().unwrap()) as usize;
      let body = match rest[0] {
        SET => FIXED,
        SET_BEFORE => FIXED_BEFORE,
        _ => 0,
      };
      let Some(record) = rest.get(5..5 + len + body) else { break };
      let value = (rest[0] != REMOVED).then(|| fixed(&record[len..]));
      index.overlay.insert(record[..len].to_vec(), value);
      rest = &rest[5 + len + body..];
    }
    index
  }

  /// Write a fresh index holding exactly `entries`, and no log.
  pub fn create(dir: &Path, mut entries: Vec<(Vec<u8>, Indexed)>) -> Result<()> {
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let records: usize = entries.iter().map(|(p, _)| p.len() + FIXED).sum();
    let mut out = Vec::with_capacity(HEADER + (entries.len() + 1) * 8 + records);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    let mut at = 0u64;
    for (path, _) in &entries {
      out.extend_from_slice(&at.to_le_bytes());
      at += (path.len() + FIXED) as u64;
    }
    out.extend_from_slice(&at.to_le_bytes());
    for (path, indexed) in &entries {
      out.extend_from_slice(path);
      put_fixed(&mut out, indexed);
    }
    let fail = |e: std::io::Error| format!("cannot write the workspace index: {e}");
    std::fs::create_dir_all(dir).map_err(fail)?;
    let temp = dir.join(format!("index.tmp{}", std::process::id()));
    std::fs::write(&temp, out).and_then(|_| std::fs::rename(&temp, dir.join("index"))).map_err(fail)?;
    match std::fs::remove_file(dir.join("index.log")) {
      Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(fail(e)),
      _ => Ok(()),
    }
  }

  fn base_at(&self, i: usize) -> (&[u8], Indexed) {
    let map = self.base.as_ref().expect("an index with entries has a base");
    let offset = |k: usize| u64::from_le_bytes(map[HEADER + k * 8..HEADER + k * 8 + 8].try_into().unwrap()) as usize;
    let records = HEADER + (self.count + 1) * 8;
    let record = &map[records + offset(i)..records + offset(i + 1)];
    let (path, f) = record.split_at(record.len() - self.fixed);
    (path, fixed(f))
  }

  fn base_find(&self, path: &[u8]) -> Option<usize> {
    let (mut lo, mut hi) = (0, self.count);
    while lo < hi {
      let mid = (lo + hi) / 2;
      match self.base_at(mid).0.cmp(path) {
        std::cmp::Ordering::Less => lo = mid + 1,
        std::cmp::Ordering::Greater => hi = mid,
        std::cmp::Ordering::Equal => return Some(mid),
      }
    }
    None
  }

  pub fn get(&self, path: &[u8]) -> Option<Indexed> {
    match self.overlay.get(path) {
      Some(value) => *value,
      None => self.base_find(path).map(|i| self.base_at(i).1),
    }
  }

  /// Whether the index holds anything below this directory.
  pub fn has_under(&self, dir: &[u8]) -> bool {
    let below = |path: &[u8]| path.len() > dir.len() && path.starts_with(dir) && path[dir.len()] == b'/';
    if self.overlay.iter().any(|(path, value)| value.is_some() && below(path)) {
      return true;
    }
    // The first entry at or after `dir/` is under it, or nothing is.
    let prefix = [dir, b"/"].concat();
    let (mut lo, mut hi) = (0, self.count);
    while lo < hi {
      let mid = (lo + hi) / 2;
      if self.base_at(mid).0 < prefix.as_slice() { lo = mid + 1 } else { hi = mid }
    }
    (lo..self.count).map(|i| self.base_at(i).0).take_while(|path| below(path)).any(|path| !self.overlay.contains_key(path))
  }

  pub fn insert(&mut self, path: Vec<u8>, indexed: Indexed) {
    self.pending.push(SET);
    self.pending.extend_from_slice(&(path.len() as u32).to_le_bytes());
    self.pending.extend_from_slice(&path);
    put_fixed(&mut self.pending, &indexed);
    self.overlay.insert(path, Some(indexed));
  }

  pub fn remove(&mut self, path: &[u8]) {
    self.pending.push(REMOVED);
    self.pending.extend_from_slice(&(path.len() as u32).to_le_bytes());
    self.pending.extend_from_slice(path);
    self.overlay.insert(path.to_vec(), None);
  }

  pub fn seen(&self) -> Seen {
    Seen { base: vec![false; self.count], other: BTreeSet::new() }
  }

  /// `get`, noting in `seen` that the path was come across.
  pub fn get_noting(&self, seen: &mut Seen, path: &[u8]) -> Option<Indexed> {
    if let Some(value) = self.overlay.get(path) {
      seen.other.insert(path.to_vec());
      return *value;
    }
    let i = self.base_find(path)?;
    seen.base[i] = true;
    Some(self.base_at(i).1)
  }

  /// The entries a walk did not come across: files that are gone.
  pub fn unseen(&self, seen: &Seen) -> Vec<(Vec<u8>, Indexed)> {
    let mut out = Vec::new();
    for i in (0..self.count).filter(|&i| !seen.base[i]) {
      let (path, indexed) = self.base_at(i);
      if !self.overlay.contains_key(path) {
        out.push((path.to_vec(), indexed));
      }
    }
    out.extend(self.overlay.iter().filter(|(path, _)| !seen.other.contains(*path)).filter_map(|(path, v)| Some((path.clone(), (*v)?))));
    out
  }

  /// Every entry, in path order. The base is in path order (`create` sorts it, and lookups
  /// search it so), and so is the overlay: the two are merged as they are read.
  pub fn entries(&self) -> Vec<(Vec<u8>, Indexed)> {
    let mut all = Vec::with_capacity(self.count + self.overlay.len());
    let mut overlay = self.overlay.iter().peekable();
    for i in 0..self.count {
      let (path, indexed) = self.base_at(i);
      // One entry for a path, should the base ever hold one twice.
      if all.last().is_some_and(|(last, _): &(Vec<u8>, Indexed)| last.as_slice() == path) {
        continue;
      }
      // What the overlay has before this path, and what it says of this one.
      let mut replaced = false;
      while let Some((other, value)) = overlay.peek() {
        match other.as_slice().cmp(path) {
          std::cmp::Ordering::Greater => break,
          order => {
            replaced |= order == std::cmp::Ordering::Equal;
            if let Some(value) = value {
              all.push(((*other).clone(), *value));
            }
            overlay.next();
          }
        }
      }
      if !replaced {
        all.push((path.to_vec(), indexed));
      }
    }
    all.extend(overlay.filter_map(|(path, value)| Some((path.clone(), (*value)?))));
    all
  }

  /// Make what this process changed durable: appended to the log, or, once
  /// the log is large beside the index, merged into a new index.
  pub fn save(&mut self) -> Result<()> {
    if self.pending.is_empty() {
      return Ok(());
    }
    if self.overlay.len() >= MERGE_AT.max(self.count / 4) || self.fixed != FIXED {
      let entries = self.entries();
      // The mapping of the old file goes before the file does.
      (self.base, self.count) = (None, 0);
      Index::create(&self.dir, entries)?;
      *self = Index::open(&self.dir);
      return Ok(());
    }
    let fail = |e: std::io::Error| format!("cannot write the workspace index: {e}");
    let mut log = std::fs::File::options().append(true).create(true).open(self.dir.join("index.log")).map_err(fail)?;
    log.write_all(&self.pending).map_err(fail)?;
    self.pending.clear();
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("newvcs-index-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
  }

  fn file(n: u8) -> Indexed {
    Indexed { entry: Entry { mode: 0o100644, id: [n; 32] }, size: n as u64, mtime: 1_000 + n as u128, ctime: 2_000 + n as u64 }
  }

  fn names(entries: &[(Vec<u8>, Indexed)]) -> Vec<String> {
    entries.iter().map(|(p, _)| String::from_utf8_lossy(p).into_owned()).collect()
  }

  #[test]
  fn entries_are_the_base_with_the_overlay_over_it_in_path_order() {
    // Against a map of the same entries, over many shapes of base and overlay: changes before,
    // between, at and after the base's paths, removals of paths it has and does not have.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |n: u64| {
      seed ^= seed << 13;
      seed ^= seed >> 7;
      seed ^= seed << 17;
      seed % n
    };
    for round in 0..200 {
      let d = dir(&format!("merge-{round}"));
      let path = |n: u64| format!("p{:02}", n).into_bytes();
      let mut model: BTreeMap<Vec<u8>, Indexed> = BTreeMap::new();
      for _ in 0..next(12) {
        model.insert(path(next(30)), file(next(200) as u8));
      }
      Index::create(&d, model.iter().map(|(p, i)| (p.clone(), *i)).collect()).unwrap();
      let mut index = Index::open(&d);
      for _ in 0..next(12) {
        let p = path(next(30));
        if next(3) == 0 {
          index.remove(&p);
          model.remove(&p);
        } else {
          let i = file(next(200) as u8);
          index.insert(p.clone(), i);
          model.insert(p, i);
        }
      }
      let expected: Vec<(Vec<u8>, Indexed)> = model.into_iter().collect();
      assert_eq!(names(&index.entries()), names(&expected), "round {round}");
      assert!(index.entries().iter().zip(&expected).all(|(a, b)| a.1 == b.1), "round {round}");
      let _ = std::fs::remove_dir_all(&d);
    }
  }

  #[test]
  fn changes_are_appended_and_read_back_over_the_base() {
    let d = dir("log");
    Index::create(&d, vec![(b"b/two".to_vec(), file(2)), (b"a".to_vec(), file(1)), (b"b/three".to_vec(), file(3))]).unwrap();
    let mut index = Index::open(&d);
    assert_eq!(names(&index.entries()), ["a", "b/three", "b/two"]);
    assert_eq!(index.get(b"b/two"), Some(file(2)));
    assert_eq!(index.get(b"b"), None);

    index.insert(b"b/two".to_vec(), file(9));
    index.insert(b"c".to_vec(), file(4));
    index.remove(b"a");
    index.save().unwrap();
    let base = std::fs::metadata(d.join("index")).unwrap().modified().unwrap();
    // Three records in the log; the index itself was not rewritten.
    assert!(std::fs::metadata(d.join("index.log")).unwrap().len() < 200);
    let index = Index::open(&d);
    assert_eq!(std::fs::metadata(d.join("index")).unwrap().modified().unwrap(), base);
    assert_eq!(names(&index.entries()), ["b/three", "b/two", "c"]);
    assert_eq!((index.get(b"b/two"), index.get(b"a"), index.get(b"c")), (Some(file(9)), None, Some(file(4))));

    // A walk that comes across two of the three finds the third missing.
    let mut seen = index.seen();
    assert_eq!(index.get_noting(&mut seen, b"b/three"), Some(file(3)));
    assert_eq!(index.get_noting(&mut seen, b"c"), Some(file(4)));
    assert_eq!(index.get_noting(&mut seen, b"new"), None);
    assert_eq!(names(&index.unseen(&seen)), ["b/two"]);
    let _ = std::fs::remove_dir_all(&d);
  }

  #[test]
  fn a_long_log_is_merged_and_a_torn_one_is_read_as_far_as_it_goes() {
    let d = dir("merge");
    Index::create(&d, (0..10u8).map(|i| (vec![b'f', b'0' + i], file(i))).collect()).unwrap();
    let mut index = Index::open(&d);
    for i in 0..MERGE_AT {
      index.insert(format!("new/{i:04}").into_bytes(), file(7));
    }
    index.remove(b"f3");
    index.save().unwrap();
    assert!(!d.join("index.log").exists(), "merged into the index");
    let mut index = Index::open(&d);
    assert_eq!(index.entries().len(), 10 - 1 + MERGE_AT);
    assert_eq!((index.get(b"f3"), index.get(b"f4"), index.get(b"new/0100")), (None, Some(file(4)), Some(file(7))));

    index.insert(b"x".to_vec(), file(1));
    index.insert(b"y".to_vec(), file(2));
    index.save().unwrap();
    let log = std::fs::read(d.join("index.log")).unwrap();
    std::fs::write(d.join("index.log"), &log[..log.len() - 10]).unwrap();
    let index = Index::open(&d);
    assert_eq!((index.get(b"x"), index.get(b"y")), (Some(file(1)), None));
    // A file that is not an index is no index.
    std::fs::write(d.join("index"), b"nvindex1 and then nonsense").unwrap();
    assert_eq!(Index::open(&d).get(b"f4"), None);
    let _ = std::fs::remove_dir_all(&d);
  }
}
