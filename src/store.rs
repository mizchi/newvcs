//! Objects by content hash, in one append-only segment.
//!
//! Three kinds of object share the segment: blobs (file contents), trees
//! (one directory of a manifest) and commits. A record is a fixed header and
//! the object's bytes, zstd-compressed.
//!
//! An object may be stored as a delta against another: zstd with the base as
//! a raw dictionary. Blobs take the version of the file they replace as base,
//! trees the directory they replace: a directory is mostly ids, which do not
//! compress, but a new version differs from the old in one of them. Measured on real histories, a delta against the
//! previous version of the same file is around 1% of the file where zstd
//! alone is around 20%, and cutting chains at a few dozen links costs almost
//! nothing. `MAX_CHAIN` is that cut.
//!
//! Next to the segment sit files of derived facts, all recomputable from the
//! objects: where each object is (`objects.idx`), Git's id for an object, and
//! the root tree of a commit.
//!
//! Opening the store must be cheap, because every command does it. So the
//! segment is not read at open. `objects.idx` is the objects sorted by id
//! behind a fan-out table, mapped into memory and searched in place; only
//! the records appended since it was last written are scanned. Git's ids are
//! read when something first asks for one.

use crate::{Id, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

pub const MAX_CHAIN: u16 = 32;
const LEVEL: i32 = 3;
/// kind, flags, id, raw length, stored length; a delta's base id follows.
const HEADER: usize = 1 + 1 + 32 + 4 + 4;
const BASE: usize = 32;
const F_DELTA: u8 = 1;
const F_RAW: u8 = 2;
const FLUSH_AT: usize = 4 << 20;
const CACHE_BYTES: usize = 256 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
  Blob = 1,
  Tree = 2,
  Commit = 3,
  /// An annotated tag: the id of what it names, then Git's tag object from its second line on.
  Tag = 4,
}

#[derive(Clone, Copy)]
struct Slot {
  /// Where the stored bytes start in the segment.
  offset: u64,
  stored: u32,
  raw: u32,
  flags: u8,
  base: Id,
  /// Links between this object and a full copy.
  depth: u16,
}

pub type GitOid = [u8; 20];

/// `objects.idx`: a header, a fan-out table by the first two bytes of an id,
/// and one fixed-width record per object, sorted by id.
struct Sorted {
  map: memmap2::Mmap,
  count: usize,
  /// Bytes of the segment the records account for.
  covered: u64,
}

const IDX_MAGIC: &[u8; 8] = b"nvidx001";
const IDX_HEADER: usize = 8 + 8 + 8 + 16 * 8;
const IDX_FANOUT: usize = (65536 + 1) * 4;
/// id, offset, stored, raw, flags, a spare byte, depth, base.
const IDX_RECORD: usize = 32 + 8 + 4 + 4 + 1 + 1 + 2 + 32;

impl Sorted {
  fn open(path: &Path, segment_len: u64) -> Option<(Sorted, Stats)> {
    let file = File::open(path).ok()?;
    // SAFETY: the file is replaced by rename, never written in place.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    if map.len() < IDX_HEADER + IDX_FANOUT || &map[..8] != IDX_MAGIC {
      return None;
    }
    let word = |i: usize| u64::from_le_bytes(map[8 + i * 8..16 + i * 8].try_into().unwrap());
    let (covered, count) = (word(0), word(1) as usize);
    // An index for more of the segment than there is belongs to another segment.
    if covered > segment_len || map.len() != IDX_HEADER + IDX_FANOUT + count * IDX_RECORD {
      return None;
    }
    let mut stats =
      Stats { objects: word(2) as usize, raw_bytes: word(3), stored_bytes: word(4), deltas: word(5) as usize, ..Stats::default() };
    for (i, cell) in stats.by_class.iter_mut().flatten().enumerate() {
      *cell = word(6 + i);
    }
    Some((Sorted { map, count, covered }, stats))
  }

  fn record(&self, i: usize) -> &[u8] {
    let at = IDX_HEADER + IDX_FANOUT + i * IDX_RECORD;
    &self.map[at..at + IDX_RECORD]
  }

  fn slot_of(record: &[u8]) -> Slot {
    Slot {
      offset: u64::from_le_bytes(record[32..40].try_into().unwrap()),
      stored: u32::from_le_bytes(record[40..44].try_into().unwrap()),
      raw: u32::from_le_bytes(record[44..48].try_into().unwrap()),
      flags: record[48],
      depth: u16::from_le_bytes(record[50..52].try_into().unwrap()),
      base: record[52..84].try_into().unwrap(),
    }
  }

  fn get(&self, id: &Id) -> Option<Slot> {
    let prefix = (id[0] as usize) << 8 | id[1] as usize;
    let fan = |p: usize| u32::from_le_bytes(self.map[IDX_HEADER + p * 4..IDX_HEADER + p * 4 + 4].try_into().unwrap()) as usize;
    // Ids are hashes: a two-byte prefix leaves a handful of records to look through.
    (fan(prefix)..fan(prefix + 1).min(self.count)).map(|i| self.record(i)).find(|r| r[..32] == id[..]).map(Sorted::slot_of)
  }
}

pub struct Store {
  pub dir: PathBuf,
  segment: File,
  /// Bytes of the segment on disk, and the records not yet written after them.
  flushed: u64,
  pending: Vec<u8>,
  sorted: Option<Sorted>,
  /// Objects appended since `sorted` was written.
  index: HashMap<Id, Slot>,
  /// Recently read or written blobs, raw: the next delta is usually against one of them.
  cache: HashMap<Id, std::sync::Arc<Vec<u8>>>,
  cache_bytes: usize,
  git_log: File,
  /// Git's id of an object, and for blobs and commits the way back. Read on first use.
  git: Option<GitIds>,
  roots_log: File,
  /// The root tree of a commit; `None` for a commit with no files.
  pub roots: HashMap<Id, Option<Id>>,
  /// Bytes of the segment after its last whole record.
  unread: u64,
  pub stats: Stats,
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
  pub objects: usize,
  pub raw_bytes: u64,
  pub stored_bytes: u64,
  pub deltas: usize,
  /// `[count, raw bytes, stored bytes]` for full blobs, delta blobs, trees, commits.
  pub by_class: [[u64; 3]; 4],
}

impl Stats {
  fn add(&mut self, kind: u8, flags: u8, raw: u32, stored: usize) {
    self.objects += 1;
    self.raw_bytes += raw as u64;
    self.stored_bytes += stored as u64;
    self.deltas += (flags & F_DELTA != 0) as usize;
    let class = match kind {
      k if k == Kind::Tree as u8 => 2,
      k if k == Kind::Commit as u8 => 3,
      _ => (flags & F_DELTA != 0) as usize,
    };
    self.by_class[class][0] += 1;
    self.by_class[class][1] += raw as u64;
    self.by_class[class][2] += stored as u64;
  }
}

/// Git's ids for objects, both ways. Most are in `git.idx`, sorted and
/// mapped, so that asking for one does not mean reading them all; the ones
/// recorded since it was written are in the two maps.
#[derive(Default)]
struct GitIds {
  sorted: Option<(memmap2::Mmap, usize)>,
  of: HashMap<Id, GitOid>,
  back: HashMap<GitOid, Id>,
  /// Whether this process recorded any: only then is `git.idx` worth rewriting.
  added: bool,
}

const GIT_RECORD: usize = 53;
const GIT_PAIR: usize = 52;
const GIT_IDX_MAGIC: &[u8; 8] = b"nvgitid1";
/// With this many ids outside `git.idx`, a store that recorded some rewrites it on closing.
const GIT_CHECKPOINT_AT: usize = 1024;

impl GitIds {
  /// The `at`th pair of the table sorted by nv id (`by_git` false) or by Git id.
  fn pair(&self, by_git: bool, at: usize) -> &[u8] {
    let (map, count) = self.sorted.as_ref().expect("asked only when there is one");
    let start = 16 + if by_git { count * GIT_PAIR } else { 0 } + at * GIT_PAIR;
    &map[start..start + GIT_PAIR]
  }

  fn find(&self, by_git: bool, key: &[u8]) -> Option<&[u8]> {
    let (_, count) = self.sorted.as_ref()?;
    let (mut lo, mut hi) = (0, *count);
    while lo < hi {
      let mid = (lo + hi) / 2;
      match self.pair(by_git, mid)[..key.len()].cmp(key) {
        std::cmp::Ordering::Less => lo = mid + 1,
        std::cmp::Ordering::Greater => hi = mid,
        std::cmp::Ordering::Equal => return Some(&self.pair(by_git, mid)[key.len()..]),
      }
    }
    None
  }

  fn git_of(&self, id: &Id) -> Option<GitOid> {
    self.of.get(id).copied().or_else(|| self.find(false, id).map(|oid| oid.try_into().expect("twenty bytes")))
  }

  fn of_git(&self, oid: &GitOid) -> Option<Id> {
    self.back.get(oid).copied().or_else(|| self.find(true, oid).map(|id| id.try_into().expect("thirty-two bytes")))
  }

  fn all(&self) -> Vec<(Id, GitOid)> {
    let count = self.sorted.as_ref().map_or(0, |(_, count)| *count);
    let sorted = (0..count).map(|at| self.pair(false, at)).map(|p| (p[..32].try_into().unwrap(), p[32..].try_into().unwrap()));
    sorted.chain(self.of.iter().map(|(id, oid)| (*id, *oid))).collect()
  }
}

pub fn object_id(kind: Kind, bytes: &[u8]) -> Id {
  let mut hasher = blake3::Hasher::new();
  hasher.update(&[kind as u8]);
  hasher.update(bytes);
  *hasher.finalize().as_bytes()
}

fn open_log(path: &Path) -> Result<(File, Vec<u8>)> {
  let mut file =
    File::options().read(true).append(true).create(true).open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
  let mut bytes = Vec::new();
  file.read_to_end(&mut bytes).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
  Ok((file, bytes))
}

impl Store {
  pub fn open(dir: &Path) -> Result<Store> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join("objects.seg");
    let segment =
      File::options().read(true).append(true).create(true).open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let len = segment.metadata().map_err(|e| e.to_string())?.len();
    let (sorted, mut stats) = match Sorted::open(&dir.join("objects.idx"), len) {
      Some((sorted, stats)) => (Some(sorted), stats),
      None => (None, Stats::default()),
    };
    // Only what the index does not account for is read: the records since it was written.
    let covered = sorted.as_ref().map_or(0, |s| s.covered);
    let mut bytes = vec![0u8; (len - covered) as usize];
    segment.read_exact_at(&mut bytes, covered).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut index: HashMap<Id, Slot> = HashMap::new();
    let mut at = 0usize;
    // A record cut short by a crash is ignored, and overwritten by the next append.
    while at + HEADER <= bytes.len() {
      let h = &bytes[at..at + HEADER];
      let (raw, stored) = (u32::from_le_bytes(h[34..38].try_into().unwrap()), u32::from_le_bytes(h[38..42].try_into().unwrap()));
      let flags = h[1];
      let head = HEADER + if flags & F_DELTA != 0 { BASE } else { 0 };
      if at + head + stored as usize > bytes.len() {
        break;
      }
      let id: Id = h[2..34].try_into().unwrap();
      let base: Id = if flags & F_DELTA != 0 { bytes[at + HEADER..at + head].try_into().unwrap() } else { [0; 32] };
      let base_depth = || index.get(&base).copied().or_else(|| sorted.as_ref().and_then(|s| s.get(&base))).map_or(0, |b| b.depth);
      let depth = if flags & F_DELTA != 0 { base_depth() + 1 } else { 0 };
      index.insert(id, Slot { offset: covered + (at + head) as u64, stored, raw, flags, base, depth });
      stats.add(h[0], flags, raw, head + stored as usize);
      at += head + stored as usize;
    }
    // Bytes after the last whole record are left where they are. They may be
    // a record cut short by a crash, or the sign of damage further up; only a
    // command that has to write cuts them off, and `check` reports them.
    let unread = (bytes.len() - at) as u64;
    let flushed = covered + at as u64;
    drop(bytes);

    let git_path = dir.join("git.map");
    let git_log = File::options()
      .read(true)
      .append(true)
      .create(true)
      .open(&git_path)
      .map_err(|e| format!("cannot open {}: {e}", git_path.display()))?;
    let (roots_log, bytes) = open_log(&dir.join("roots.map"))?;
    let mut roots = HashMap::new();
    for record in bytes.as_chunks::<65>().0 {
      let (commit, root): (Id, Id) = (record[..32].try_into().unwrap(), record[33..65].try_into().unwrap());
      roots.insert(commit, (record[32] != 0).then_some(root));
    }
    Ok(Store {
      dir: dir.to_path_buf(),
      segment,
      flushed,
      pending: Vec::new(),
      sorted,
      index,
      cache: HashMap::new(),
      cache_bytes: 0,
      git_log,
      git: None,
      roots_log,
      roots,
      unread,
      stats,
    })
  }

  fn slot(&self, id: &Id) -> Option<Slot> {
    self.index.get(id).copied().or_else(|| self.sorted.as_ref().and_then(|s| s.get(id)))
  }

  pub fn contains(&self, id: &Id) -> bool {
    self.slot(id).is_some()
  }

  /// Write `objects.idx` anew to account for everything in the segment, so
  /// that the next open has nothing to scan.
  pub fn checkpoint(&mut self) -> Result<()> {
    self.flush()?;
    self.checkpoint_git()?;
    if self.index.is_empty() && self.sorted.is_some() {
      return Ok(());
    }
    let mut all: Vec<(Id, Slot)> = self.index.iter().map(|(id, slot)| (*id, *slot)).collect();
    if let Some(sorted) = &self.sorted {
      all.extend((0..sorted.count).map(|i| sorted.record(i)).map(|r| (r[..32].try_into().unwrap(), Sorted::slot_of(r))));
    }
    all.sort_unstable_by_key(|a| a.0);
    let mut out = Vec::with_capacity(IDX_HEADER + IDX_FANOUT + all.len() * IDX_RECORD);
    out.extend_from_slice(IDX_MAGIC);
    let st = &self.stats;
    let words = [self.flushed, all.len() as u64, st.objects as u64, st.raw_bytes, st.stored_bytes, st.deltas as u64];
    for word in words.into_iter().chain(st.by_class.into_iter().flatten()) {
      out.extend_from_slice(&word.to_le_bytes());
    }
    out.resize(IDX_HEADER, 0);
    // For each two-byte prefix, the position of the first record at or after it.
    let mut next = 0usize;
    for prefix in 0..=65536usize {
      while next < all.len() && ((all[next].0[0] as usize) << 8 | all[next].0[1] as usize) < prefix {
        next += 1;
      }
      out.extend_from_slice(&(next as u32).to_le_bytes());
    }
    for (id, slot) in &all {
      out.extend_from_slice(id);
      out.extend_from_slice(&slot.offset.to_le_bytes());
      out.extend_from_slice(&slot.stored.to_le_bytes());
      out.extend_from_slice(&slot.raw.to_le_bytes());
      out.extend_from_slice(&[slot.flags, 0]);
      out.extend_from_slice(&slot.depth.to_le_bytes());
      out.extend_from_slice(&slot.base);
    }
    let path = self.dir.join("objects.idx");
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temp, &out).and_then(|_| std::fs::rename(&temp, &path)).map_err(|e| format!("cannot write the object index: {e}"))?;
    drop(out);
    let (sorted, _) = Sorted::open(&path, self.flushed).ok_or("the object index just written does not open")?;
    self.sorted = Some(sorted);
    self.index.clear();
    Ok(())
  }

  fn git_ids(&mut self) -> &mut GitIds {
    let (dir, log) = (&self.dir, &self.git_log);
    self.git.get_or_insert_with(|| {
      let records = log.metadata().map_or(0, |m| m.len() as usize / GIT_RECORD);
      let mut ids = GitIds::default();
      // The sorted table says how many records of the log it holds; the rest are read.
      let mut covered = 0;
      if let Ok(file) = File::open(dir.join("git.idx"))
        && let Ok(map) = unsafe { memmap2::Mmap::map(&file) }
        && map.len() >= 16
        && &map[..8] == GIT_IDX_MAGIC
      {
        let in_log = u32::from_le_bytes(map[8..12].try_into().unwrap()) as usize;
        let count = u32::from_le_bytes(map[12..16].try_into().unwrap()) as usize;
        if in_log <= records && map.len() == 16 + count * GIT_PAIR * 2 {
          covered = in_log;
          ids.sorted = Some((map, count));
        }
      }
      let mut tail = vec![0u8; (records - covered) * GIT_RECORD];
      if log.read_exact_at(&mut tail, (covered * GIT_RECORD) as u64).is_ok() {
        for record in tail.as_chunks::<GIT_RECORD>().0 {
          let (id, oid): (Id, GitOid) = (record[1..33].try_into().unwrap(), record[33..53].try_into().unwrap());
          ids.of.insert(id, oid);
          ids.back.insert(oid, id);
        }
      }
      ids
    })
  }

  /// Write every Git id into the sorted table, so that the next process reads none of the log.
  fn checkpoint_git(&mut self) -> Result<()> {
    let records = self.git_log.metadata().map_or(0, |m| m.len() as usize / GIT_RECORD);
    let ids = self.git_ids();
    if ids.of.is_empty() && ids.sorted.is_some() {
      return Ok(());
    }
    let mut all = ids.all();
    all.sort_unstable();
    all.dedup_by_key(|pair| pair.0);
    let mut out = Vec::with_capacity(16 + all.len() * GIT_PAIR * 2);
    out.extend_from_slice(GIT_IDX_MAGIC);
    out.extend_from_slice(&(records as u32).to_le_bytes());
    out.extend_from_slice(&(all.len() as u32).to_le_bytes());
    for (id, oid) in &all {
      out.extend_from_slice(id);
      out.extend_from_slice(oid);
    }
    all.sort_unstable_by_key(|pair| pair.1);
    for (id, oid) in &all {
      out.extend_from_slice(oid);
      out.extend_from_slice(id);
    }
    let path = self.dir.join("git.idx");
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temp, &out).and_then(|_| std::fs::rename(&temp, &path)).map_err(|e| format!("cannot write the Git id index: {e}"))?;
    // Read again from the table just written.
    self.git = None;
    Ok(())
  }

  /// Git's id of an object, if it has been given one.
  pub fn git_of(&mut self, id: &Id) -> Option<GitOid> {
    self.git_ids().git_of(id)
  }

  /// What kind of object this is, read from its record.
  pub fn kind_of(&mut self, id: &Id) -> Result<Option<Kind>> {
    let Some(slot) = self.slot(id) else { return Ok(None) };
    let head = (HEADER + if slot.flags & F_DELTA != 0 { BASE } else { 0 }) as u64;
    let at = slot.offset - head;
    let mut byte = [0u8; 1];
    if at >= self.flushed {
      byte[0] = self.pending[(at - self.flushed) as usize];
    } else {
      self.segment.read_exact_at(&mut byte, at).map_err(|e| format!("cannot read the segment: {e}"))?;
    }
    Ok(match byte[0] {
      1 => Some(Kind::Blob),
      2 => Some(Kind::Tree),
      3 => Some(Kind::Commit),
      4 => Some(Kind::Tag),
      _ => None,
    })
  }

  /// The object Git knows by this id.
  pub fn of_git(&mut self, oid: &GitOid) -> Option<Id> {
    self.git_ids().of_git(oid)
  }

  /// Every object that has a Git id.
  pub fn with_git_ids(&mut self) -> Vec<(Id, GitOid)> {
    self.git_ids().all()
  }

  pub fn flush(&mut self) -> Result<()> {
    if !self.pending.is_empty() {
      // New records go after the last whole one, so what follows it has to go first.
      if std::mem::take(&mut self.unread) > 0 {
        self.segment.set_len(self.flushed).map_err(|e| format!("cannot cut the segment's torn end: {e}"))?;
      }
      self.segment.write_all(&self.pending).map_err(|e| format!("cannot write the segment: {e}"))?;
      self.flushed += self.pending.len() as u64;
      self.pending.clear();
    }
    Ok(())
  }

  fn remember(&mut self, id: Id, bytes: std::sync::Arc<Vec<u8>>) {
    if self.cache_bytes > CACHE_BYTES {
      self.cache.clear();
      self.cache_bytes = 0;
    }
    self.cache_bytes += bytes.len();
    self.cache.insert(id, bytes);
  }

  /// Store an object and return its id. `base` names an object this one is
  /// probably similar to: the version of the file or directory it replaces.
  pub fn put(&mut self, kind: Kind, bytes: &[u8], base: Option<Id>) -> Result<Id> {
    let id = object_id(kind, bytes);
    if self.contains(&id) {
      return Ok(id);
    }
    let mut flags = 0u8;
    let mut base_id = [0u8; 32];
    let mut depth = 0;
    let mut stored: Option<Vec<u8>> = None;
    if let Some(base) = base
      && let Some(slot) = self.slot(&base)
      && slot.depth < MAX_CHAIN
      && let Some(base_bytes) = self.get(&base)?
    {
      let delta = compress_against(&base_bytes, bytes)?;
      // A delta that saves little is not worth a link in the chain.
      if delta.len() * 2 < bytes.len() {
        (flags, base_id, depth) = (F_DELTA, base, slot.depth + 1);
        stored = Some(delta);
      }
    }
    let stored = match stored {
      Some(s) => s,
      None => {
        let z = zstd::bulk::compress(bytes, LEVEL).map_err(|e| e.to_string())?;
        if z.len() < bytes.len() {
          z
        } else {
          flags = F_RAW;
          bytes.to_vec()
        }
      }
    };
    let head = HEADER + if flags & F_DELTA != 0 { BASE } else { 0 };
    let offset = self.flushed + (self.pending.len() + head) as u64;
    self.pending.push(kind as u8);
    self.pending.push(flags);
    self.pending.extend_from_slice(&id);
    self.pending.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    self.pending.extend_from_slice(&(stored.len() as u32).to_le_bytes());
    if flags & F_DELTA != 0 {
      self.pending.extend_from_slice(&base_id);
    }
    self.pending.extend_from_slice(&stored);
    self.index.insert(id, Slot { offset, stored: stored.len() as u32, raw: bytes.len() as u32, flags, base: base_id, depth });
    self.stats.add(kind as u8, flags, bytes.len() as u32, head + stored.len());
    if kind == Kind::Blob {
      self.remember(id, std::sync::Arc::new(bytes.to_vec()));
    }
    if self.pending.len() >= FLUSH_AT {
      self.flush()?;
    }
    Ok(id)
  }

  fn stored_bytes(&self, slot: &Slot) -> Result<Vec<u8>> {
    let mut out = vec![0u8; slot.stored as usize];
    if slot.offset >= self.flushed {
      let at = (slot.offset - self.flushed) as usize;
      out.copy_from_slice(&self.pending[at..at + slot.stored as usize]);
    } else {
      self.segment.read_exact_at(&mut out, slot.offset).map_err(|e| format!("cannot read the segment: {e}"))?;
    }
    Ok(out)
  }

  /// The bytes of an object, or `None` if the store does not hold it.
  pub fn get(&mut self, id: &Id) -> Result<Option<std::sync::Arc<Vec<u8>>>> {
    if let Some(hit) = self.cache.get(id) {
      return Ok(Some(hit.clone()));
    }
    let Some(slot) = self.slot(id) else { return Ok(None) };
    let stored = self.stored_bytes(&slot)?;
    let bytes = if slot.flags & F_RAW != 0 {
      stored
    } else if slot.flags & F_DELTA != 0 {
      let base = self.get(&slot.base)?.ok_or_else(|| format!("the base of {} is missing", crate::hex(id)))?;
      let mut d = zstd::bulk::Decompressor::with_dictionary(&base).map_err(|e| e.to_string())?;
      d.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(31)).map_err(|e| e.to_string())?;
      d.decompress(&stored, slot.raw as usize).map_err(|e| format!("cannot apply a delta: {e}"))?
    } else {
      zstd::bulk::decompress(&stored, slot.raw as usize).map_err(|e| format!("cannot decompress an object: {e}"))?
    };
    let bytes = std::sync::Arc::new(bytes);
    if slot.raw as usize <= 8 << 20 {
      self.remember(*id, bytes.clone());
    }
    Ok(Some(bytes))
  }

  /// Record Git's id for an object.
  pub fn map_git(&mut self, kind: Kind, id: Id, oid: GitOid) -> Result<()> {
    let ids = self.git_ids();
    if ids.git_of(&id).is_some() {
      return Ok(());
    }
    ids.of.insert(id, oid);
    ids.back.insert(oid, id);
    ids.added = true;
    let mut record = Vec::with_capacity(53);
    record.push(kind as u8);
    record.extend_from_slice(&id);
    record.extend_from_slice(&oid);
    self.git_log.write_all(&record).map_err(|e| format!("cannot write the git map: {e}"))
  }

  pub fn set_root(&mut self, commit: Id, root: Option<Id>) -> Result<()> {
    if self.roots.insert(commit, root).is_some() {
      return Ok(());
    }
    let mut record = Vec::with_capacity(65);
    record.extend_from_slice(&commit);
    record.push(root.is_some() as u8);
    record.extend_from_slice(&root.unwrap_or([0; 32]));
    self.roots_log.write_all(&record).map_err(|e| format!("cannot write the roots map: {e}"))
  }

  /// A named pointer to a commit. Moving it is the one write that has to be
  /// agreed on: it succeeds only if the pointer still holds `expected`.
  pub fn compare_and_set_ref(&self, name: &str, expected: Option<Id>, new: Id) -> Result<bool> {
    let path = self.dir.join("refs").join(name);
    if self.read_ref(name) != expected {
      return Ok(false);
    }
    std::fs::create_dir_all(path.parent().expect("under refs")).map_err(|e| e.to_string())?;
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temp, crate::hex(&new)).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    Ok(true)
  }

  /// Point a ref at a commit whatever it pointed at: for a copy of someone else's ref.
  pub fn set_ref(&self, name: &str, id: Id) -> Result<()> {
    let path = self.dir.join("refs").join(name);
    std::fs::create_dir_all(path.parent().expect("under refs")).map_err(|e| e.to_string())?;
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temp, crate::hex(&id)).and_then(|_| std::fs::rename(&temp, &path)).map_err(|e| e.to_string())
  }

  /// Remove a ref. `false` if there was none.
  pub fn delete_ref(&self, name: &str) -> bool {
    std::fs::remove_file(self.dir.join("refs").join(name)).is_ok()
  }

  /// The refs under a prefix (`heads`, `tags`, `remotes/origin`), by name.
  pub fn refs_under(&self, prefix: &str) -> Vec<(String, Id)> {
    let mut found: Vec<(String, Id)> = std::fs::read_dir(self.dir.join("refs").join(prefix))
      .into_iter()
      .flatten()
      .flatten()
      .filter_map(|e| e.file_name().into_string().ok())
      .filter(|name| !name.contains(".tmp"))
      .filter_map(|name| Some((name.clone(), self.read_ref(&format!("{prefix}/{name}"))?)))
      .collect();
    found.sort();
    found
  }

  /// Every object's id.
  pub fn ids(&self) -> Vec<Id> {
    let mut ids: Vec<Id> = self.index.keys().copied().collect();
    if let Some(sorted) = &self.sorted {
      ids.extend((0..sorted.count).map(|i| Id::try_from(&sorted.record(i)[..32]).unwrap()));
    }
    ids
  }

  /// Read every object and hash it: `(objects read, what is wrong)`. An
  /// object is wrong if it cannot be read or is not what its name says.
  pub fn check(&mut self) -> Result<(usize, Vec<String>)> {
    let ids = self.ids();
    let mut wrong = Vec::new();
    if self.unread > 0 {
      wrong.push(format!(
        "{} bytes after the last whole record of the segment: a record cut short, or damage that hides the records after it",
        self.unread
      ));
    }
    for id in &ids {
      let kind = self.kind_of(id)?;
      match (kind, self.get(id)) {
        (Some(kind), Ok(Some(bytes))) if object_id(kind, &bytes) == *id => {}
        (_, Err(e)) => wrong.push(format!("{}: {e}", crate::hex(id))),
        _ => wrong.push(format!("{}: is not the object its name says", crate::hex(id))),
      }
      // Reading everything must not mean holding everything.
      self.cache.clear();
      self.cache_bytes = 0;
    }
    Ok((ids.len(), wrong))
  }

  pub fn read_ref(&self, name: &str) -> Option<Id> {
    let text = std::fs::read_to_string(self.dir.join("refs").join(name)).ok()?;
    crate::unhex(text.trim())?.try_into().ok()
  }

  /// A commit by Git id, by this store's id, or by ref name.
  pub fn resolve(&mut self, rev: &str) -> Option<Id> {
    if let Some(bytes) = crate::unhex(rev) {
      if let Ok(oid) = GitOid::try_from(bytes.as_slice()) {
        return self.of_git(&oid);
      }
      if let Ok(id) = Id::try_from(bytes.as_slice()) {
        return self.contains(&id).then_some(id);
      }
    }
    self.read_ref(rev)
  }
}

/// With this many objects outside the sorted index, closing the store rewrites it.
const CHECKPOINT_AT: usize = 512;

impl Drop for Store {
  fn drop(&mut self) {
    let _ = self.flush();
    if self.index.len() >= CHECKPOINT_AT {
      let _ = self.checkpoint();
    }
    if self.git.as_ref().is_some_and(|ids| ids.added && ids.of.len() >= GIT_CHECKPOINT_AT) {
      let _ = self.checkpoint_git();
    }
  }
}

/// `new` compressed with `base` as a raw dictionary: what `zstd --patch-from` does.
fn compress_against(base: &[u8], new: &[u8]) -> Result<Vec<u8>> {
  let mut c = zstd::bulk::Compressor::with_dictionary(LEVEL, base).map_err(|e| e.to_string())?;
  // A window wide enough to reach all of the base.
  let window = (base.len() + new.len()).max(1 << 10).next_power_of_two().trailing_zeros().clamp(10, 31);
  c.set_parameter(zstd::zstd_safe::CParameter::WindowLog(window)).map_err(|e| e.to_string())?;
  c.compress(new).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("newvcs-store-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
  }

  #[test]
  fn objects_come_back_after_reopening_and_through_delta_chains() {
    let dir = temp("chain");
    let mut versions: Vec<Vec<u8>> = Vec::new();
    let mut text: Vec<u8> = (0..400).flat_map(|i| format!("line {i} of a file that changes a little each time\n").into_bytes()).collect();
    for i in 0..(MAX_CHAIN as usize * 2 + 5) {
      text.extend_from_slice(format!("added in version {i}\n").as_bytes());
      versions.push(text.clone());
    }
    let mut ids = Vec::new();
    {
      let mut store = Store::open(&dir).unwrap();
      let mut previous = None;
      for v in &versions {
        let id = store.put(Kind::Blob, v, previous).unwrap();
        previous = Some(id);
        ids.push(id);
      }
      // Deltas are small, and the chain is cut: some versions are full copies again.
      assert!(store.stats.stored_bytes * 10 < store.stats.raw_bytes, "{} of {}", store.stats.stored_bytes, store.stats.raw_bytes);
      assert!(store.stats.deltas < versions.len() - 2 && store.stats.deltas > versions.len() / 2);
      assert!(store.index.values().all(|s| s.depth <= MAX_CHAIN));
    }
    let mut store = Store::open(&dir).unwrap();
    for (id, v) in ids.iter().zip(&versions) {
      assert_eq!(store.get(id).unwrap().unwrap().as_slice(), v.as_slice());
    }
    // The same through the sorted index, with newer objects after it and deltas across the two.
    let before = store.stats;
    store.checkpoint().unwrap();
    assert!(store.index.is_empty());
    let late = store.put(Kind::Blob, &[versions.last().unwrap().as_slice(), b"one more line\n"].concat(), ids.last().copied()).unwrap();
    drop(store);
    let mut store = Store::open(&dir).unwrap();
    assert_eq!(store.index.len(), 1, "only the object written after the checkpoint is scanned for");
    assert_eq!((store.stats.objects, store.stats.raw_bytes > before.raw_bytes), (before.objects + 1, true));
    for (id, v) in ids.iter().zip(&versions) {
      assert_eq!(store.get(id).unwrap().unwrap().as_slice(), v.as_slice());
    }
    assert!(store.get(&late).unwrap().unwrap().ends_with(b"one more line\n"));
    assert!(store.slot(&late).unwrap().depth <= MAX_CHAIN);
    // An index that claims more segment than there is, is not believed.
    drop(store);
    let segment = dir.join("objects.seg");
    let cut = std::fs::metadata(&segment).unwrap().len() / 2;
    std::fs::File::options().write(true).open(&segment).unwrap().set_len(cut).unwrap();
    let mut store = Store::open(&dir).unwrap();
    assert!(store.sorted.is_none() && store.stats.objects < before.objects);
    assert_eq!(store.get(&ids[0]).unwrap().unwrap().as_slice(), versions[0].as_slice());
    assert!(store.get(&[7; 32]).unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn the_same_bytes_as_another_kind_are_another_object() {
    assert_ne!(object_id(Kind::Blob, b"x"), object_id(Kind::Tree, b"x"));
  }

  #[test]
  fn a_ref_moves_only_from_the_value_the_writer_saw() {
    let dir = temp("refs");
    let store = Store::open(&dir).unwrap();
    assert!(store.compare_and_set_ref("main", None, [1; 32]).unwrap());
    assert!(!store.compare_and_set_ref("main", None, [2; 32]).unwrap());
    assert!(!store.compare_and_set_ref("main", Some([9; 32]), [2; 32]).unwrap());
    assert!(store.compare_and_set_ref("main", Some([1; 32]), [2; 32]).unwrap());
    assert_eq!(store.read_ref("main"), Some([2; 32]));
    let _ = std::fs::remove_dir_all(&dir);
  }
}
