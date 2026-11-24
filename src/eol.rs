//! Line endings, as `.gitattributes` asks for them: `text`, `text=auto`,
//! `eol=lf`, `eol=crlf`, `-text` and `binary`. A file whose rules say CRLF
//! is written to a workspace with CRLF and read back as LF, so that what is
//! stored is what Git would store.
//!
//! The attribute files are parsed and their patterns matched by gitoxide
//! (`gix-attributes`, `gix-glob`). Which rule wins and what it does to the
//! bytes is here, after Git's `convert.c`, for a repository with
//! `core.autocrlf` unset on a system whose own line ending is LF.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bstr::ByteSlice;
use gix_attributes::StateRef;
use gix_glob::pattern::Case;
use gix_glob::wildmatch::Mode;

/// What the rules ask for one file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum How {
  /// Bytes in, bytes out.
  AsIs,
  /// Stored with LF. `auto`: only if the content looks like text.
  /// `crlf`: written to a workspace with CRLF.
  Text { auto: bool, crlf: bool },
}

#[derive(Clone, Copy)]
enum Text {
  Set,
  Unset,
  Auto,
  /// `!text`: as if no rule had spoken.
  Unspecified,
}

struct Rule {
  pattern: gix_glob::Pattern,
  text: Option<Text>,
  crlf: Option<bool>,
}

/// The rules of one attributes file that say something about line endings.
fn rules(bytes: &[u8]) -> Vec<Rule> {
  let mut out = Vec::new();
  for (kind, assignments, _) in gix_attributes::parse(bytes).flatten() {
    let gix_attributes::parse::Kind::Pattern(pattern) = kind else { continue };
    let (mut text, mut crlf) = (None, None);
    for assignment in assignments.flatten() {
      match (assignment.name.as_str(), assignment.state) {
        ("text", StateRef::Set) => text = Some(Text::Set),
        ("text", StateRef::Unset) | ("binary", StateRef::Set) => text = Some(Text::Unset),
        ("text", StateRef::Unspecified) => text = Some(Text::Unspecified),
        ("text", StateRef::Value(value)) if value.as_bstr() == "auto" => text = Some(Text::Auto),
        ("eol", StateRef::Value(value)) if value.as_bstr() == "crlf" => crlf = Some(true),
        ("eol", StateRef::Value(value)) if value.as_bstr() == "lf" => crlf = Some(false),
        _ => {}
      }
    }
    if text.is_some() || crlf.is_some() {
      out.push(Rule { pattern, text, crlf });
    }
  }
  out
}

/// The line-ending rules of a tree, by the directory whose `.gitattributes`
/// states them.
pub struct Eols {
  /// Where to read a directory's file from when it is first asked about.
  /// `None`: every file there is was handed over at the start.
  root: Option<PathBuf>,
  dirs: HashMap<Vec<u8>, Vec<Rule>>,
  /// What a file no rule speaks of is: as it is, unless Git's `core.autocrlf` says otherwise.
  otherwise: How,
}

/// What `core.autocrlf` of the Git repository in `root`, if there is one, makes of a file with no rule.
fn configured(root: &Path) -> How {
  static KNOWN: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, How>>> = std::sync::OnceLock::new();
  if !crate::colocated::as_git(root) {
    return How::AsIs;
  }
  let mut known = KNOWN.get_or_init(Default::default).lock().expect("not poisoned");
  *known.entry(root.to_path_buf()).or_insert_with(|| match crate::git::config_value(root, "core", "autocrlf").as_deref() {
    Some("true") => How::Text { auto: true, crlf: true },
    Some("input") => How::Text { auto: true, crlf: false },
    _ => How::AsIs,
  })
}

impl Eols {
  /// The rules in the files of a workspace, read as they are needed.
  pub fn of_worktree(root: &Path) -> Self {
    Eols { root: Some(root.to_path_buf()), dirs: HashMap::new(), otherwise: configured(root) }
  }

  /// The rules of the given `.gitattributes` files: `(directory, content)`.
  pub fn of_files(files: impl IntoIterator<Item = (Vec<u8>, Arc<Vec<u8>>)>) -> Self {
    let dirs = files.into_iter().map(|(dir, bytes)| (dir, rules(&bytes))).filter(|(_, rules)| !rules.is_empty()).collect();
    Eols { root: None, dirs, otherwise: How::AsIs }
  }

  /// No file will be converted, whatever its path.
  pub fn none(&self) -> bool {
    self.root.is_none() && self.dirs.is_empty()
  }

  /// A `.gitattributes` in the workspace was written: read them again.
  pub fn forget(&mut self) {
    if self.root.is_some() {
      self.dirs.clear();
    }
  }

  fn rules_of(&mut self, dir: &[u8]) -> &[Rule] {
    if let Some(root) = &self.root
      && !self.dirs.contains_key(dir)
    {
      let file = root.join(crate::workspace::to_path(dir)).join(".gitattributes");
      let found = std::fs::read(file).map(|bytes| rules(&bytes)).unwrap_or_default();
      self.dirs.insert(dir.to_vec(), found);
    }
    self.dirs.get(dir).map_or(&[], Vec::as_slice)
  }

  pub fn how(&mut self, path: &[u8]) -> How {
    if self.none() {
      return How::AsIs;
    }
    // The file's own directory first, the root last; in a file, the last line first.
    let mut ends: Vec<usize> = path.iter().enumerate().filter(|(_, b)| **b == b'/').map(|(at, _)| at).collect();
    let (mut text, mut crlf) = (None, None);
    loop {
      let (dir, relative) = match ends.pop() {
        Some(end) => (&path[..end], &path[end + 1..]),
        None => (&path[..0], path),
      };
      let base = relative.rfind_byte(b'/').map(|at| at + 1);
      for rule in self.rules_of(dir).iter().rev() {
        let wanted = (text.is_none() && rule.text.is_some()) || (crlf.is_none() && rule.crlf.is_some());
        if wanted
          && rule.pattern.matches_repo_relative_path(relative.as_bstr(), base, Some(false), Case::Sensitive, Mode::NO_MATCH_SLASH_LITERAL)
        {
          text = text.or(rule.text);
          crlf = crlf.or(rule.crlf);
        }
      }
      if dir.is_empty() || (text.is_some() && crlf.is_some()) {
        break;
      }
    }
    match (text, crlf) {
      (Some(Text::Unset), _) => How::AsIs,
      (Some(Text::Auto), crlf) => How::Text { auto: true, crlf: crlf.unwrap_or(false) },
      (Some(Text::Set), crlf) => How::Text { auto: false, crlf: crlf.unwrap_or(false) },
      // `eol` alone makes a file text.
      (_, Some(crlf)) => How::Text { auto: false, crlf },
      _ => self.otherwise,
    }
  }
}

/// Whether `.gitattributes` gives a path Git's `union` merge (`merge=union`): where both sides
/// changed the same lines, the lines of both are kept, ours first, and the file is merged. Read
/// from the files on disk, as Git reads them in a working tree, and only for a path that is
/// being merged: not part of looking at what changed, which this would slow.
pub fn merges_by_union(root: &Path, path: &[u8]) -> bool {
  crate::union::is_union(|dir| std::fs::read(root.join(crate::workspace::to_path(dir)).join(".gitattributes")).ok(), path)
}

#[derive(Default)]
struct Stats {
  lone_lf: usize,
  lone_cr: usize,
  crlf: usize,
  nul: usize,
  printable: usize,
  unprintable: usize,
}

fn stats(bytes: &[u8]) -> Stats {
  let mut s = Stats::default();
  let mut at = 0;
  while at < bytes.len() {
    match bytes[at] {
      b'\r' if bytes.get(at + 1) == Some(&b'\n') => {
        s.crlf += 1;
        at += 1;
      }
      b'\r' => s.lone_cr += 1,
      b'\n' => s.lone_lf += 1,
      0 => {
        s.nul += 1;
        s.unprintable += 1;
      }
      // Backspace, tab, escape and form feed are things text has.
      8 | 9 | 27 | 12 => s.printable += 1,
      127 => s.unprintable += 1,
      b if b < 32 => s.unprintable += 1,
      _ => s.printable += 1,
    }
    at += 1;
  }
  // A DOS end-of-file mark does not make a file binary.
  if bytes.last() == Some(&0x1a) {
    s.unprintable -= 1;
  }
  s
}

/// Git's guess at whether content is not text.
fn looks_binary(s: &Stats) -> bool {
  s.lone_cr > 0 || s.nul > 0 || (s.printable >> 7) < s.unprintable
}

/// The bytes to write into a workspace, if they are not the stored ones.
pub fn to_worktree(how: How, bytes: &[u8]) -> Option<Vec<u8>> {
  let How::Text { auto, crlf: true } = how else { return None };
  // Nothing to do without a line feed; the scan is only worth it when there is one.
  bytes.find_byte(b'\n')?;
  let s = stats(bytes);
  if s.lone_lf == 0 || (auto && (s.lone_cr > 0 || s.crlf > 0 || looks_binary(&s))) {
    return None;
  }
  let mut out = Vec::with_capacity(bytes.len() + s.lone_lf);
  let mut before = 0u8;
  for &byte in bytes {
    if byte == b'\n' && before != b'\r' {
      out.push(b'\r');
    }
    out.push(byte);
    before = byte;
  }
  Some(out)
}

/// The bytes to store for a file of a workspace, if they are not the file's own.
pub fn to_store(how: How, bytes: &[u8]) -> Option<Vec<u8>> {
  let How::Text { auto, .. } = how else { return None };
  bytes.find(b"\r\n")?;
  if auto && looks_binary(&stats(bytes)) {
    return None;
  }
  let mut out = Vec::with_capacity(bytes.len());
  for (at, &byte) in bytes.iter().enumerate() {
    if byte != b'\r' || bytes.get(at + 1) != Some(&b'\n') {
      out.push(byte);
    }
  }
  Some(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn eols(files: &[(&str, &str)]) -> Eols {
    Eols::of_files(files.iter().map(|(dir, text)| (dir.as_bytes().to_vec(), Arc::new(text.as_bytes().to_vec()))))
  }

  #[test]
  fn the_nearest_file_and_the_last_line_decide_each_attribute() {
    let mut e = eols(&[
      ("", "* text=auto\n*.bat text eol=crlf\n*.png binary\nfixtures/** eol=lf\n"),
      ("fixtures/crlf", "*.js eol=crlf\nraw.js -text\n"),
    ]);
    assert_eq!(e.how(b"src/main.rs"), How::Text { auto: true, crlf: false });
    assert_eq!(e.how(b"run.bat"), How::Text { auto: false, crlf: true });
    assert_eq!(e.how(b"deep/down/run.bat"), How::Text { auto: false, crlf: true });
    assert_eq!(e.how(b"logo.png"), How::AsIs);
    assert_eq!(e.how(b"fixtures/a.js"), How::Text { auto: true, crlf: false });
    // `eol` from the nearer file, `text=auto` still from the root's.
    assert_eq!(e.how(b"fixtures/crlf/a.js"), How::Text { auto: true, crlf: true });
    assert_eq!(e.how(b"fixtures/crlf/raw.js"), How::AsIs);
    assert!(eols(&[("", "*.rs diff=rust\n")]).none());
    assert_eq!(eols(&[("", "*.txt eol=crlf\n")]).how(b"a.txt"), How::Text { auto: false, crlf: true });
  }

  #[test]
  fn lines_end_as_asked_on_the_way_out_and_with_lf_on_the_way_in() {
    let (crlf, auto) = (How::Text { auto: false, crlf: true }, How::Text { auto: true, crlf: true });
    assert_eq!(to_worktree(crlf, b"a\nb\n"), Some(b"a\r\nb\r\n".to_vec()));
    // A line that has its CR keeps the one it has.
    assert_eq!(to_worktree(crlf, b"a\r\nb\n"), Some(b"a\r\nb\r\n".to_vec()));
    assert_eq!(to_worktree(crlf, b"no newline"), None);
    assert_eq!(to_worktree(How::Text { auto: false, crlf: false }, b"a\n"), None);
    assert_eq!(to_worktree(How::AsIs, b"a\n"), None);
    // Guessing, a file with CRs of its own or with a NUL is left alone.
    assert_eq!(to_worktree(auto, b"a\nb\n"), Some(b"a\r\nb\r\n".to_vec()));
    assert_eq!(to_worktree(auto, b"a\r\nb\n"), None);
    assert_eq!(to_worktree(auto, b"a\n\0b\n"), None);

    assert_eq!(to_store(crlf, b"a\r\nb\r\n"), Some(b"a\nb\n".to_vec()));
    assert_eq!(to_store(How::Text { auto: false, crlf: false }, b"a\r\nb\n"), Some(b"a\nb\n".to_vec()));
    assert_eq!(to_store(crlf, b"a\nb\n"), None);
    assert_eq!(to_store(crlf, b"a\rb\r\n"), Some(b"a\rb\n".to_vec()));
    assert_eq!(to_store(auto, b"a\r\n\0"), None);
    assert_eq!(to_store(How::AsIs, b"a\r\n"), None);
  }
}
