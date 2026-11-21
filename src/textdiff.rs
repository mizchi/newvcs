//! Line differences, written the way `git diff` writes them.
//!
//! A shortest edit script by Myers' algorithm, then the one adjustment Git
//! makes that shows in everyday output: a run of added or removed lines that
//! could sit in several places is moved as far down as it goes, so that a
//! line appended after an identical one is reported as the last line and not
//! the one before. Hunks carry three lines of context, hunks whose context
//! would touch are one hunk, and each header names the nearest line above
//! that starts like a definition.
//!
//! This is not Git's diff engine. On inputs where several shortest scripts
//! exist and Git's heuristics choose among them (indentation, mostly) the
//! lines reported can differ, though never what applying them gives.

/// The lines of `bytes`, each with its line ending if it has one.
pub fn lines(bytes: &[u8]) -> Vec<&[u8]> {
  bytes.split_inclusive(|&b| b == b'\n').collect()
}

/// How many lines the two middles may have together for the search written here to be used.
const LONG: usize = 2000;

/// Which lines of `a` are removed and which of `b` are added to turn one into the other.
pub fn changed(a: &[&[u8]], b: &[&[u8]]) -> (Vec<bool>, Vec<bool>) {
  let (n, m) = (a.len(), b.len());
  let (mut removed, mut added) = (vec![false; n], vec![false; m]);
  // What both start and end with is not part of the question.
  let head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
  let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
  let (a_mid, b_mid) = (&a[head..n - tail], &b[head..m - tail]);
  // Nothing in the middle of one side: all of the other's middle is the difference. A file that
  // is new, or gone, or only grew, is settled here without asking how its lines compare.
  if a_mid.is_empty() || b_mid.is_empty() {
    removed[head..n - tail].fill(true);
    added[head..m - tail].fill(true);
    return (removed, added);
  }
  // The search below remembers every step it took, which for two long texts with little in
  // common is more memory than there is. Past a size, gitoxide's diff does it in linear space.
  if a_mid.len() + b_mid.len() > LONG {
    let mut input = imara_diff::InternedInput::default();
    input.update_before(a_mid.iter().copied());
    input.update_after(b_mid.iter().copied());
    let mut diff = imara_diff::Diff::compute(imara_diff::Algorithm::Myers, &input);
    diff.postprocess_lines(&input);
    for at in 0..a_mid.len() {
      removed[head + at] = diff.is_removed(at as u32);
    }
    for at in 0..b_mid.len() {
      added[head + at] = diff.is_added(at as u32);
    }
    return (removed, added);
  }
  let (an, bn) = (a_mid.len() as isize, b_mid.len() as isize);
  let max = (an + bn) as usize;
  // The furthest point reached on each diagonal, kept per number of edits so the path can be walked back.
  let mut v = vec![0isize; 2 * max + 2];
  let mut trace: Vec<Vec<isize>> = Vec::new();
  let at = |k: isize| (k + max as isize) as usize;
  let mut found = max == 0;
  for d in 0..=max as isize {
    if found {
      break;
    }
    trace.push(v.clone());
    let mut k = -d;
    while k <= d {
      let down = k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]);
      let mut x = if down { v[at(k + 1)] } else { v[at(k - 1)] + 1 };
      let mut y = x - k;
      while x < an && y < bn && a_mid[x as usize] == b_mid[y as usize] {
        x += 1;
        y += 1;
      }
      v[at(k)] = x;
      if x >= an && y >= bn {
        found = true;
        break;
      }
      k += 2;
    }
  }
  // Back from the end, one edit at a time.
  let (mut x, mut y) = (an, bn);
  for (d, v) in trace.iter().enumerate().rev() {
    let d = d as isize;
    let k = x - y;
    let down = k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]);
    let prev_k = if down { k + 1 } else { k - 1 };
    let prev_x = v[at(prev_k)];
    let prev_y = prev_x - prev_k;
    while x > prev_x && y > prev_y {
      x -= 1;
      y -= 1;
    }
    if d > 0 {
      if down {
        added[head + prev_y as usize] = true;
      } else {
        removed[head + prev_x as usize] = true;
      }
    }
    (x, y) = (prev_x, prev_y);
  }
  slide_down(a, &mut removed);
  slide_down(b, &mut added);
  (removed, added)
}

/// Move each run of marked lines as far down as it can go without changing
/// what the marks mean: while the line after the run equals its first line.
fn slide_down(lines: &[&[u8]], marked: &mut [bool]) {
  let mut i = 0;
  while i < lines.len() {
    if !marked[i] {
      i += 1;
      continue;
    }
    let mut start = i;
    let mut end = i;
    while end < lines.len() && marked[end] {
      end += 1;
    }
    while end < lines.len() && !marked[end] && lines[start] == lines[end] {
      marked[start] = false;
      marked[end] = true;
      start += 1;
      end += 1;
      // The run may now touch the next one and become part of it.
      while end < lines.len() && marked[end] {
        end += 1;
      }
    }
    i = end;
  }
}

const CONTEXT: usize = 3;

/// `(removed lines, added lines)`.
pub fn counts(a: &[u8], b: &[u8]) -> (usize, usize) {
  let (removed, added) = changed(&lines(a), &lines(b));
  (removed.iter().filter(|r| **r).count(), added.iter().filter(|r| **r).count())
}

/// The hunks of a unified diff from `a` to `b`: everything after the `+++` line.
pub fn hunks(a: &[u8], b: &[u8]) -> Vec<u8> {
  let (al, bl) = (lines(a), lines(b));
  let (removed, added) = changed(&al, &bl);
  // The two files walked together: `(kind, index in a, index in b)`.
  let mut rows: Vec<(u8, usize, usize)> = Vec::new();
  let (mut i, mut j) = (0, 0);
  while i < al.len() || j < bl.len() {
    if i < al.len() && removed[i] {
      rows.push((b'-', i, j));
      i += 1;
    } else if j < bl.len() && added[j] {
      rows.push((b'+', i, j));
      j += 1;
    } else {
      rows.push((b' ', i, j));
      i += 1;
      j += 1;
    }
  }
  let mut out = Vec::new();
  let mut at = 0;
  while let Some(first) = rows[at..].iter().position(|r| r.0 != b' ').map(|p| p + at) {
    // A hunk runs until more unchanged lines follow a change than two contexts hold.
    let mut last = first;
    let mut k = first + 1;
    while k < rows.len() && k - last <= 2 * CONTEXT + 1 {
      if rows[k].0 != b' ' {
        last = k;
      }
      k += 1;
    }
    let (from, to) = (first.saturating_sub(CONTEXT), (last + 1 + CONTEXT).min(rows.len()));
    let hunk = &rows[from..to];
    let old_count = hunk.iter().filter(|r| r.0 != b'+').count();
    let new_count = hunk.iter().filter(|r| r.0 != b'-').count();
    let range = |start: usize, count: usize| match count {
      // An empty range is named by the line before it.
      0 => format!("{start},0"),
      1 => format!("{}", start + 1),
      _ => format!("{},{count}", start + 1),
    };
    out.extend_from_slice(format!("@@ -{} +{} @@", range(hunk[0].1, old_count), range(hunk[0].2, new_count)).as_bytes());
    // The nearest line above the hunk that starts like the beginning of something.
    let heading = al[..hunk[0].1].iter().rev().find(|l| l.first().is_some_and(|&c| c.is_ascii_alphabetic() || c == b'_' || c == b'$'));
    if let Some(line) = heading {
      let text = line.strip_suffix(b"\n").unwrap_or(line);
      let text = &text[..text.len().min(80)];
      out.push(b' ');
      out.extend_from_slice(text.trim_ascii_end());
    }
    out.push(b'\n');
    for &(kind, i, j) in hunk {
      let line = if kind == b'+' { bl[j] } else { al[i] };
      out.push(kind);
      out.extend_from_slice(line);
      if !line.ends_with(b"\n") {
        out.extend_from_slice(b"\n\\ No newline at end of file\n");
      }
    }
    at = to;
    if rows[to..].iter().all(|r| r.0 == b' ') {
      break;
    }
  }
  out
}

/// Git's full score for two contents being one file that changed.
pub const SAME: u32 = 60_000;
/// From this score up, Git calls a removed file and an added file a rename.
pub const RENAME: u32 = 30_000;

/// How much of `from` is still in `to`, the way Git estimates it when it
/// looks for renames: both are cut into spans (a line, or 64 bytes of one),
/// and the score is the bytes of `from`'s spans that `to` has too, over the
/// larger of the two sizes.
pub fn similarity(from: &[u8], to: &[u8]) -> u32 {
  let (larger, smaller) = (from.len().max(to.len()), from.len().min(to.len()));
  if larger == 0 {
    return SAME;
  }
  // Too different in size to reach the threshold whatever they hold.
  if (larger as u64) * ((SAME - RENAME) as u64) < ((larger - smaller) as u64) * SAME as u64 {
    return 0;
  }
  let spans = |bytes: &[u8]| {
    let text = !binary(bytes);
    let mut counts: std::collections::HashMap<Vec<u8>, usize> = std::collections::HashMap::new();
    let mut span = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
      // A carriage return before a line feed is not part of a text line.
      if text && b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
        continue;
      }
      span.push(b);
      if b == b'\n' || span.len() == 64 {
        *counts.entry(std::mem::take(&mut span)).or_default() += 1;
      }
    }
    if !span.is_empty() {
      *counts.entry(span).or_default() += 1;
    }
    counts
  };
  let (ours, theirs) = (spans(from), spans(to));
  let copied: usize = ours.iter().map(|(span, n)| span.len() * (*n).min(theirs.get(span).copied().unwrap_or(0))).sum();
  (copied as u64 * SAME as u64 / larger as u64) as u32
}

/// Content Git would not show a text diff for.
pub fn binary(bytes: &[u8]) -> bool {
  bytes[..bytes.len().min(8000)].contains(&0)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap()
  }

  /// Applying what `changed` says to `a` gives `b`.
  fn applies(a: &str, b: &str) {
    let (al, bl) = (lines(a.as_bytes()), lines(b.as_bytes()));
    let (removed, added) = changed(&al, &bl);
    let kept: Vec<&[u8]> = al.iter().zip(&removed).filter(|(_, r)| !**r).map(|(l, _)| *l).collect();
    let same: Vec<&[u8]> = bl.iter().zip(&added).filter(|(_, r)| !**r).map(|(l, _)| *l).collect();
    assert_eq!(kept, same, "{a:?} -> {b:?}");
  }

  #[test]
  fn what_is_marked_turns_one_text_into_the_other() {
    let cases = ["", "a\n", "a\nb\nc\n", "a\nx\nc\n", "x\na\nb\nc\ny\n", "c\nb\na\n", "a\na\na\n", "a\nb\na\nb\na\n", "no newline"];
    for a in cases {
      for b in cases {
        applies(a, b);
      }
    }
    // And no more is marked than has to be.
    assert_eq!(counts(b"a\nb\nc\nd\n", b"a\nc\nd\ne\n"), (1, 1));
    assert_eq!(counts(b"same\n", b"same\n"), (0, 0));
  }

  #[test]
  fn a_change_in_the_middle_is_one_hunk_with_context_and_a_heading() {
    let a =
      "import os\n\ndef first(x):\n    return x + 1\n\ndef second(x):\n    y = x * 2\n    return y\n\ndef third(x):\n    return x - 1\n";
    let b = a.replace("x * 2", "x * 3");
    assert_eq!(
      text(hunks(a.as_bytes(), b.as_bytes())),
      "@@ -4,7 +4,7 @@ def first(x):\n     return x + 1\n \n def second(x):\n-    y = x * 2\n+    y = x * 3\n     return y\n \n def third(x):\n"
    );
  }

  #[test]
  fn appended_lines_new_files_and_missing_newlines() {
    assert_eq!(text(hunks(b"one\ntwo\nthree\n", b"one\ntwo\nthree\nfour\n")), "@@ -1,3 +1,4 @@\n one\n two\n three\n+four\n");
    assert_eq!(text(hunks(b"", b"new\n")), "@@ -0,0 +1 @@\n+new\n");
    assert_eq!(text(hunks(b"gone\nboth\n", b"")), "@@ -1,2 +0,0 @@\n-gone\n-both\n");
    assert_eq!(
      text(hunks(b"end", b"end, changed")),
      "@@ -1 +1 @@\n-end\n\\ No newline at end of file\n+end, changed\n\\ No newline at end of file\n"
    );
    // A line added after an identical one is reported as the later of the two.
    assert_eq!(text(hunks(b"a\nx\nb\n", b"a\nx\nx\nb\n")), "@@ -1,3 +1,4 @@\n a\n x\n+x\n b\n");
  }

  #[test]
  fn similarity_is_the_share_of_the_old_content_still_there() {
    assert_eq!(similarity(b"same\n", b"same\n"), SAME);
    // One line of two survives: half of the larger size.
    assert_eq!(similarity(b"fn a() {}\n", b"fn a() {}\nfn b() {}\n"), SAME / 2);
    assert_eq!(similarity(b"one\ntwo\n", b"three\nfour\n"), 0);
    // A tenth the size cannot be a rename, without looking inside.
    assert_eq!(similarity(b"x\n", &b"x\n".repeat(10)), 0);
    assert!(similarity(b"a\r\nb\r\n", b"a\nb\n") >= RENAME);
  }

  #[test]
  fn changes_far_apart_are_separate_hunks_and_near_ones_are_one() {
    let a: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    let far = a.replace("line 3\n", "LINE 3\n").replace("line 25\n", "LINE 25\n");
    assert_eq!(text(hunks(a.as_bytes(), far.as_bytes())).matches("@@ -").count(), 2);
    let near = a.replace("line 3\n", "LINE 3\n").replace("line 10\n", "LINE 10\n");
    assert_eq!(text(hunks(a.as_bytes(), near.as_bytes())).matches("@@ -").count(), 1);
    let apart = a.replace("line 3\n", "LINE 3\n").replace("line 11\n", "LINE 11\n");
    assert_eq!(text(hunks(a.as_bytes(), apart.as_bytes())).matches("@@ -").count(), 2);
  }

  #[test]
  fn a_long_text_is_compared_without_remembering_every_step() {
    // New, and gone: every line, and no search.
    let long: Vec<u8> = (0..200_000).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    assert_eq!(counts(b"", &long), (0, 200_000));
    assert_eq!(counts(&long, b""), (200_000, 0));
    // Two long texts that share every other line.
    let other: Vec<u8> =
      (0..60_000).flat_map(|i| if i % 2 == 0 { format!("line {i}\n") } else { format!("other {i}\n") }.into_bytes()).collect();
    let first: Vec<u8> = (0..60_000).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    assert_eq!(counts(&first, &other), (30_000, 30_000));
  }
}
