//! nv's union merge for the Worker. The merge and the attribute rule are `src/union.rs` of nv
//! itself, included as they are: one implementation, so that what the server merges is what a
//! sync would have.
//!
//! The host writes its input into memory it asked for with `alloc`, calls, and reads the answer
//! at `answer()`, `answer_len()` bytes long, before its next call.

#[path = "../../../src/union.rs"]
#[allow(dead_code)]
mod union;

static mut ANSWER: Vec<u8> = Vec::new();

/// Memory for `len` bytes of input, to be given back with `dealloc`.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
  let mut bytes = Vec::<u8>::with_capacity(len.max(1));
  let at = bytes.as_mut_ptr();
  std::mem::forget(bytes);
  at
}

/// # Safety
/// `at` and `len` are what `alloc` gave and was given.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dealloc(at: *mut u8, len: usize) {
  drop(unsafe { Vec::from_raw_parts(at, 0, len.max(1)) });
}

fn slice<'a>(at: *const u8, len: usize) -> &'a [u8] {
  if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(at, len) } }
}

#[unsafe(no_mangle)]
pub extern "C" fn answer() -> *const u8 {
  unsafe { (*std::ptr::addr_of!(ANSWER)).as_ptr() }
}

#[unsafe(no_mangle)]
pub extern "C" fn answer_len() -> usize {
  unsafe { (*std::ptr::addr_of!(ANSWER)).len() }
}

/// The union merge of three versions of a file; the answer is the merged bytes.
#[unsafe(no_mangle)]
pub extern "C" fn merge_union(base: *const u8, base_len: usize, ours: *const u8, ours_len: usize, theirs: *const u8, theirs_len: usize) -> usize {
  let merged = union::merge_union(slice(base, base_len), slice(ours, ours_len), slice(theirs, theirs_len));
  let len = merged.len();
  unsafe { *std::ptr::addr_of_mut!(ANSWER) = merged };
  len
}

/// Whether the rules give `path` the union merge. `files` is each directory's `.gitattributes`
/// that there is, as frames: a little-endian u32 length and the directory, then one and its
/// content. 1 for union, 0 for not, 2 for files that are not frames.
#[unsafe(no_mangle)]
pub extern "C" fn is_union(path: *const u8, path_len: usize, files: *const u8, files_len: usize) -> u32 {
  let (path, mut rest) = (slice(path, path_len), slice(files, files_len));
  let mut frames = Vec::new();
  while !rest.is_empty() {
    if rest.len() < 4 {
      return 2;
    }
    let len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    if rest.len() < 4 + len {
      return 2;
    }
    frames.push(&rest[4..4 + len]);
    rest = &rest[4 + len..];
  }
  if frames.len() % 2 != 0 {
    return 2;
  }
  let pairs: Vec<(&[u8], &[u8])> = frames.chunks(2).map(|pair| (pair[0], pair[1])).collect();
  let found = |dir: &[u8]| pairs.iter().find(|(d, _)| *d == dir).map(|(_, bytes)| bytes.to_vec());
  u32::from(union::is_union(found, path))
}
