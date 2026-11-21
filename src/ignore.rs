//! Which files a workspace does not see: Git's `.gitignore` rules, matched by
//! gitoxide's `gix-ignore`. The rules are Git's and nothing here adds to them.

use std::path::{Path, PathBuf};

use bstr::ByteSlice;
use gix_glob::pattern::Case;
use gix_ignore::search::Ignore;

use crate::workspace::to_path;

/// The ignore files in force at one place of a walk: where a Git repository
/// shares the directory, the files Git reads that are not in the tree; the
/// workspace's own `.nv/exclude`; then the `.gitignore` of each directory
/// from the root down.
#[derive(Clone)]
pub struct Ignores {
  root: PathBuf,
  search: gix_ignore::Search,
}

impl Ignores {
  pub fn new(root: &Path) -> Self {
    let mut search = gix_ignore::Search::default();
    // Beside a Git repository, what Git leaves out is left out here: the user's file for
    // every repository, then this clone's own. A `.gitignore` in the tree outranks both.
    if crate::colocated::shared(root) {
      let files = crate::git::global_excludes(root).into_iter().chain([root.join(".git").join("info").join("exclude")]);
      for file in files {
        if let Ok(bytes) = std::fs::read(&file) {
          let _ = search.add_patterns_buffer(&bytes, file, None, Ignore::default());
        }
      }
    }
    if let Ok(bytes) = std::fs::read(root.join(crate::workspace::META).join("exclude")) {
      let _ = search.add_patterns_buffer(&bytes, root.join(".nv/exclude"), None, Ignore::default());
    }
    Ignores { root: root.to_path_buf(), search }
  }

  /// Take in the `.gitignore` of a directory being entered. Whether there
  /// was one, in which case `leave` is owed.
  pub fn enter(&mut self, dir: &[u8]) -> bool {
    let file = self.root.join(to_path(dir)).join(".gitignore");
    let Ok(bytes) = std::fs::read(&file) else { return false };
    self.search.add_patterns_buffer(&bytes, file, Some(&self.root), Ignore::default()).is_ok()
  }

  pub fn leave(&mut self) {
    self.search.patterns.pop();
  }

  /// Whether the rules taken in so far ignore this path. The last rule that
  /// matches decides, and a `!` rule decides against.
  pub fn ignored(&self, path: &[u8], is_dir: bool) -> bool {
    self
      .search
      .pattern_matching_relative_path(path.as_bstr(), Some(is_dir), Case::Sensitive)
      .is_some_and(|found| !found.pattern.is_negative())
  }
}

/// `is_ignored` for many paths: a directory found to be ignored is remembered,
/// so that the thousand files a build writes into one cost one question.
pub struct Checker {
  root: PathBuf,
  ignored_dirs: std::collections::HashSet<Vec<u8>>,
}

impl Checker {
  pub fn new(root: &Path) -> Self {
    Checker { root: root.to_path_buf(), ignored_dirs: Default::default() }
  }

  pub fn ignored(&mut self, path: &[u8]) -> bool {
    let parent = path.iter().rposition(|&b| b == b'/').map_or(&path[..0], |at| &path[..at]);
    if self
      .ignored_dirs
      .iter()
      .any(|dir| parent == dir.as_slice() || (parent.len() > dir.len() && parent.starts_with(dir) && parent[dir.len()] == b'/'))
    {
      return true;
    }
    if !is_ignored(&self.root, path) {
      return false;
    }
    // Ignored through its directory, or only by its own name?
    if !parent.is_empty() && is_ignored(&self.root, parent) {
      self.ignored_dirs.insert(parent.to_vec());
    }
    true
  }
}

/// Whether a path of the workspace is ignored: by a rule for it, or by being
/// inside a directory that is.
pub fn is_ignored(root: &Path, path: &[u8]) -> bool {
  let mut ignores = Ignores::new(root);
  ignores.enter(b"");
  let is_dir = root.join(to_path(path)).is_dir();
  for (at, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
    if ignores.ignored(&path[..at], true) {
      return true;
    }
    ignores.enter(&path[..at]);
  }
  ignores.ignored(path, is_dir)
}
