//! Git's three-way merge of a text file, and the `.gitattributes` rule that asks for `union`,
//! as functions of bytes alone. This file is compiled into nv and, as it is, into the server's
//! WebAssembly (`server/merger`), so that a list the server merges is byte for byte the list
//! `nv sync` would have made. It may use nothing of nv's.

use bstr::ByteSlice;
use gix_attributes::StateRef;
use gix_glob::pattern::Case;
use gix_glob::wildmatch::Mode;

/// Git's three-way merge of a text file, by gitoxide. `false` when the two sides changed the
/// same lines: the text then holds both, between Git's markers. With `union` (`merge=union`),
/// it holds both without markers, ours first, and is merged.
pub fn merge_text(base: &[u8], ours: &[u8], theirs: &[u8], our_name: &str, their_name: &str, union: bool) -> (Vec<u8>, bool) {
  use gix_merge::blob::{Resolution, builtin_driver::text};
  let mut merged = Vec::new();
  let mut input = imara_diff::InternedInput::default();
  let labels = text::Labels { ancestor: None, current: Some(our_name.into()), other: Some(their_name.into()) };
  let mut options = text::Options::default();
  if union {
    options.conflict = text::Conflict::ResolveWithUnion;
  }
  let how = text(&mut merged, &mut input, labels, ours, base, theirs, options);
  (merged, how != Resolution::Conflict)
}

/// The union merge of a file, as a sync makes it: the names on markers do not matter, since
/// there are none.
pub fn merge_union(base: &[u8], ours: &[u8], theirs: &[u8]) -> Vec<u8> {
  merge_text(base, ours, theirs, "yours", "theirs", true).0
}

/// What `.gitattributes` says of how a path is merged: Git's `merge` attribute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merge {
  /// Unspecified, `merge` set or `merge=text`: the three-way text merge.
  Text,
  /// `merge=union`: both sides' lines, without markers.
  Union,
  /// `-merge`, `merge=binary` or `binary`: not merged.
  Binary,
  /// `merge=<name>`: the driver the configuration names `merge.<name>.driver`.
  Driver(String),
}

/// Whether `.gitattributes` gives a path Git's `union` merge. `attributes(dir)` is the content of
/// the `.gitattributes` in that directory (`b""` for the top), if there is one. The file's own
/// directory first, the top last; in a file, the last line first; the first rule that matches
/// and says anything of `merge` decides, and `binary` says `-merge`.
pub fn is_union(attributes: impl FnMut(&[u8]) -> Option<Vec<u8>>, path: &[u8]) -> bool {
  merge_of(attributes, path) == Merge::Union
}

/// The `merge` attribute of a path, as `is_union` reads it.
pub fn merge_of(mut attributes: impl FnMut(&[u8]) -> Option<Vec<u8>>, path: &[u8]) -> Merge {
  let mut ends: Vec<usize> = path.iter().enumerate().filter(|(_, b)| **b == b'/').map(|(at, _)| at).collect();
  loop {
    let (dir, relative) = match ends.pop() {
      Some(end) => (&path[..end], &path[end + 1..]),
      None => (&path[..0], path),
    };
    let bytes = attributes(dir).unwrap_or_default();
    let mut said = None;
    for (kind, assignments, _) in gix_attributes::parse(&bytes).flatten() {
      let gix_attributes::parse::Kind::Pattern(pattern) = kind else { continue };
      let mut merge = None;
      for assignment in assignments.flatten() {
        match (assignment.name.as_str(), assignment.state) {
          ("merge", StateRef::Value(value)) => {
            merge = Some(match value.as_bstr().to_str_lossy().as_ref() {
              "union" => Merge::Union,
              "text" => Merge::Text,
              "binary" => Merge::Binary,
              name => Merge::Driver(name.to_string()),
            })
          }
          ("merge", StateRef::Set) => merge = Some(Merge::Text),
          ("merge", StateRef::Unset) | ("binary", StateRef::Set) => merge = Some(Merge::Binary),
          ("merge", _) => merge = Some(Merge::Text),
          _ => {}
        }
      }
      let Some(merge) = merge else { continue };
      let base = relative.rfind_byte(b'/').map(|at| at + 1);
      if pattern.matches_repo_relative_path(relative.as_bstr(), base, Some(false), Case::Sensitive, Mode::NO_MATCH_SLASH_LITERAL) {
        said = Some(merge);
      }
    }
    if let Some(merge) = said {
      return merge;
    }
    if dir.is_empty() {
      return Merge::Text;
    }
  }
}

/// The directories whose `.gitattributes` can speak of a path, the top first: `b""`, then each
/// directory on the way down. What a push names for the server to read.
pub fn attribute_dirs(path: &[u8]) -> Vec<&[u8]> {
  let mut dirs = vec![&path[..0]];
  dirs.extend(path.iter().enumerate().filter(|(_, b)| **b == b'/').map(|(at, _)| &path[..at]));
  dirs
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_nearest_rule_that_speaks_of_merge_decides() {
    let files = |dir: &[u8]| match dir {
      b"" => Some(b"*.txt merge=union\nlist merge=union\n".to_vec()),
      b"sub" => Some(b"a.txt -merge\nb.txt text\n".to_vec()),
      _ => None,
    };
    assert!(is_union(files, b"list"));
    assert!(is_union(files, b"x.txt"));
    assert!(!is_union(files, b"sub/a.txt"), "the nearer file says otherwise");
    assert!(is_union(files, b"sub/b.txt"), "a rule about something else does not decide");
    assert!(!is_union(files, b"plain"));
    assert_eq!(attribute_dirs(b"a/b/c"), vec![&b""[..], b"a", b"a/b"]);
  }

  #[test]
  fn a_union_keeps_both_sides_ours_first() {
    assert_eq!(merge_union(b"start\n", b"start\nbob\n", b"start\nann\n"), b"start\nbob\nann\n");
  }
}
