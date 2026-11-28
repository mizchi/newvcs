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

/// What a line sets an attribute to: set, `-`, `!`, or a value.
#[derive(Clone)]
enum State {
  Set,
  Unset,
  Unspecified,
  Value(String),
}

impl State {
  fn of(state: &StateRef<'_>) -> State {
    match state {
      StateRef::Set => State::Set,
      StateRef::Unset => State::Unset,
      StateRef::Unspecified => State::Unspecified,
      StateRef::Value(value) => State::Value(value.as_bstr().to_str_lossy().into_owned()),
    }
  }
}

/// What `merge` is, where filling this attribute fills it: Git fills an attribute once, from the
/// line of highest priority that names it, and a macro filled as set fills its own attributes
/// that are not filled yet, in its turn (second pass of the thirteenth round: `*.txt m` then
/// `f.txt -m` left `m` unset, and `merge` unspecified, where nv took the earlier line's union).
fn fill(
  name: &str,
  state: &State,
  macros: &[(String, Vec<(String, State)>)],
  filled: &mut std::collections::HashSet<String>,
  depth: usize,
) -> Option<Merge> {
  if !filled.insert(name.to_string()) {
    return None;
  }
  if name == "merge" {
    return Some(match state {
      State::Value(value) => match value.as_str() {
        "union" => Merge::Union,
        "text" => Merge::Text,
        "binary" => Merge::Binary,
        name => Merge::Driver(name.to_string()),
      },
      State::Unset => Merge::Binary,
      State::Set | State::Unspecified => Merge::Text,
    });
  }
  if !matches!(state, State::Set) || depth >= 8 {
    return None;
  }
  // `binary` is Git's own macro: `-diff -merge -text`.
  let builtin = [("diff".to_string(), State::Unset), ("merge".to_string(), State::Unset), ("text".to_string(), State::Unset)];
  let said: &[(String, State)] = match macros.iter().rev().find(|(m, _)| m == name) {
    Some((_, said)) => said,
    None if name == "binary" => &builtin,
    None => return None,
  };
  said.iter().rev().find_map(|(n, s)| fill(n, s, macros, filled, depth + 1))
}

/// The macros the top `.gitattributes` defines (`[attr]<name> <attributes>`): Git takes them from
/// there and from no other file of the tree.
fn macros_of(top: &[u8]) -> Vec<(String, Vec<(String, State)>)> {
  let mut out = Vec::new();
  for (kind, assignments, _) in gix_attributes::parse(top).flatten() {
    let gix_attributes::parse::Kind::Macro(name) = kind else { continue };
    let said = assignments.flatten().map(|a| (a.name.as_str().to_string(), State::of(&a.state))).collect();
    out.push((name.as_str().to_string(), said));
  }
  out
}

/// The `merge` attribute of a path, as `is_union` reads it: the lines that match it from the
/// nearest file to the top, each file's from its last line up, each line's from its last
/// attribute back, as Git fills them; `Text` where none says anything of `merge`.
pub fn merge_of(mut attributes: impl FnMut(&[u8]) -> Option<Vec<u8>>, path: &[u8]) -> Merge {
  let mut ends: Vec<usize> = path.iter().enumerate().filter(|(_, b)| **b == b'/').map(|(at, _)| at).collect();
  let macros = macros_of(&attributes(b"").unwrap_or_default());
  let mut filled = std::collections::HashSet::new();
  loop {
    let (dir, relative) = match ends.pop() {
      Some(end) => (&path[..end], &path[end + 1..]),
      None => (&path[..0], path),
    };
    let bytes = attributes(dir).unwrap_or_default();
    let lines: Vec<_> = gix_attributes::parse(&bytes).flatten().collect();
    for (kind, assignments, _) in lines.into_iter().rev() {
      let gix_attributes::parse::Kind::Pattern(pattern) = kind else { continue };
      let base = relative.rfind_byte(b'/').map(|at| at + 1);
      if !pattern.matches_repo_relative_path(relative.as_bstr(), base, Some(false), Case::Sensitive, Mode::NO_MATCH_SLASH_LITERAL) {
        continue;
      }
      let said: Vec<(String, State)> = assignments.flatten().map(|a| (a.name.as_str().to_string(), State::of(&a.state))).collect();
      if let Some(merge) = said.iter().rev().find_map(|(n, s)| fill(n, s, &macros, &mut filled, 0)) {
        return merge;
      }
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

  /// As `git check-attr merge` answers for the same files (thirteenth round: macros were not
  /// read, and the server merged by union a path Git gives a driver).
  #[test]
  fn a_macro_set_says_what_its_attributes_say() {
    let files = |dir: &[u8]| {
      match dir {
      b"" => Some(
        b"[attr]m merge=x\n[attr]n -merge\n[attr]both m n\n*.txt merge=union\nf.txt m\ng.txt n\nh.txt both\ni.txt m merge=union\nj.txt merge=y m\nk.txt -m\n".to_vec(),
      ),
      b"sub" => Some(b"[attr]m merge=z\nl.txt m\n".to_vec()),
      _ => None,
    }
    };
    let of = |path: &[u8]| merge_of(files, path);
    assert_eq!(of(b"f.txt"), Merge::Driver("x".into()));
    assert_eq!(of(b"g.txt"), Merge::Binary);
    assert_eq!(of(b"h.txt"), Merge::Binary);
    assert_eq!(of(b"i.txt"), Merge::Union);
    assert_eq!(of(b"j.txt"), Merge::Driver("x".into()));
    assert_eq!(of(b"k.txt"), Merge::Union, "a macro unset is not expanded");
    assert_eq!(of(b"sub/l.txt"), Merge::Driver("x".into()), "a macro is defined at the top only");
    // A later line that unsets the macro, makes it unspecified or gives it a value leaves it
    // unexpanded (second pass), here and through another macro.
    for top in [
      &b"[attr]m merge=union\n*.txt m\nf.txt -m\n"[..],
      b"[attr]m merge=union\nf.txt m\nf.txt !m\n",
      b"[attr]m merge=union\nf.txt m\n*.txt m=foo\n",
      b"[attr]m merge=x\n[attr]n m\nf.txt n\nf.txt -n\n",
    ] {
      assert_eq!(merge_of(|dir: &[u8]| dir.is_empty().then(|| top.to_vec()), b"f.txt"), Merge::Text, "{}", String::from_utf8_lossy(top));
    }
    let top = b"*.txt merge=union\nf.txt binary\nf.txt !binary\n".to_vec();
    assert_eq!(merge_of(|dir: &[u8]| dir.is_empty().then(|| top.clone()), b"f.txt"), Merge::Union);
  }

  #[test]
  fn a_union_keeps_both_sides_ours_first() {
    assert_eq!(merge_union(b"start\n", b"start\nbob\n", b"start\nann\n"), b"start\nbob\nann\n");
  }
}
