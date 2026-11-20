// A commit as the store has it: parents, the verbatim tail of a Git commit
// object, and the paths it changed against its first parent. The encoding
// and the id are those of `src/commit.rs` and `src/store.rs`; a test on each
// side pins the same id for the same commit.

import { blake3 } from "@noble/hashes/blake3.js";

export type Change = { path: Uint8Array; entry: { mode: number; id: Uint8Array } | null };
export type Commit = { parents: Uint8Array[]; rest: Uint8Array; changes: Change[] };

const KIND_COMMIT = 3;

export function encode(commit: Commit): Uint8Array {
  const parts: Uint8Array[] = [];
  const u32 = (n: number) => {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, n, true);
    parts.push(b);
  };
  u32(commit.parents.length);
  parts.push(...commit.parents);
  u32(commit.rest.length);
  parts.push(commit.rest);
  u32(commit.changes.length);
  for (const { path, entry } of commit.changes) {
    u32(path.length);
    parts.push(path);
    if (entry) {
      parts.push(Uint8Array.of(1));
      u32(entry.mode);
      parts.push(entry.id);
    } else {
      parts.push(Uint8Array.of(0));
    }
  }
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/// Throws on bytes that are not a commit: a client's push is not trusted.
export function decode(bytes: Uint8Array): Commit {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let at = 0;
  const take = (n: number) => {
    if (n < 0 || at + n > bytes.length) throw new Error("a commit object is cut short");
    at += n;
    return bytes.subarray(at - n, at);
  };
  const u32 = () => {
    take(4);
    return view.getUint32(at - 4, true);
  };
  const parents = Array.from({ length: u32() }, () => take(32));
  const rest = take(u32());
  const changes: Change[] = [];
  for (let n = u32(); n > 0; n--) {
    const path = take(u32());
    const entry = take(1)[0] === 0 ? null : { mode: u32(), id: take(32) };
    changes.push({ path, entry });
  }
  if (at !== bytes.length) throw new Error("bytes after the end of a commit object");
  return { parents, rest, changes };
}

const KIND_BLOB = 1;

/// An object's id: BLAKE3 over its kind and bytes.
function objectId(kind: number, bytes: Uint8Array): Uint8Array {
  const hasher = blake3.create();
  hasher.update(Uint8Array.of(kind));
  hasher.update(bytes);
  return hasher.digest();
}

export const idOf = (encoded: Uint8Array) => objectId(KIND_COMMIT, encoded);
export const blobIdOf = (bytes: Uint8Array) => objectId(KIND_BLOB, bytes);

export const hex = (bytes: Uint8Array) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

export function unhex(text: string): Uint8Array {
  if (!/^([0-9a-f]{2})*$/.test(text)) throw new Error("not a hex string");
  return Uint8Array.from(text.match(/../g) ?? [], (h) => parseInt(h, 16));
}

/// Two changes get in each other's way when they name the same path, or one
/// names a directory the other is inside of.
export function pathsConflict(a: string, b: string): boolean {
  return a === b || a.startsWith(b + "/") || b.startsWith(a + "/");
}

/// The paths of `mine` that something in `theirs` gets in the way of.
export function conflicts(mine: string[], theirs: Iterable<string>): string[] {
  const exact = new Set<string>();
  const dirs = new Set<string>();
  for (const path of theirs) {
    exact.add(path);
    for (let i = path.indexOf("/"); i >= 0; i = path.indexOf("/", i + 1)) dirs.add(path.slice(0, i));
  }
  return mine.filter((path) => {
    if (exact.has(path) || dirs.has(path)) return true;
    for (let i = path.indexOf("/"); i >= 0; i = path.indexOf("/", i + 1)) if (exact.has(path.slice(0, i))) return true;
    return false;
  });
}
