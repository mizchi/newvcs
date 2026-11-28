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
  // Git's commit is the tree, the parents, and then these bytes: a `parent` line among them would
  // be a parent every client's Git history has and the ref does not (thirteenth round).
  if (!restOk(rest)) throw new Error("what a commit says of who made it and why is not a commit's header and message");
  const changes: Change[] = [];
  for (let n = u32(); n > 0; n--) {
    const path = take(u32());
    // A change is spelled one way: 0 removes, 1 sets. Any other byte would give one commit many ids.
    const kind = take(1)[0];
    if (kind > 1) throw new Error("a commit has a change of a kind there is none of");
    const entry = kind === 0 ? null : { mode: u32(), id: take(32) };
    if (!pathOk(path)) throw new Error("a commit names a path no tree may have");
    if (entry && !MODES.includes(entry.mode)) throw new Error("a commit gives a path a mode no file has");
    changes.push({ path, entry });
  }
  if (at !== bytes.length) throw new Error("bytes after the end of a commit object");
  // A ref is one line of commits: a commit on more than one, or on one the ref never had, would
  // leave its history unreadable to whoever synced it.
  if (parents.length > 1) throw new Error("a commit with more than one parent is a merge, and a ref holds one line of commits");
  if (new Set(changes.map((c) => hex(c.path))).size !== changes.length) throw new Error("a commit says two things of one path");
  // Nor sets a path as something that is not a directory and sets something below it: no tree
  // holds both, and a client taking the changes in order would write through a link.
  const leaves = new Set(changes.filter((c) => c.entry && c.entry.mode !== 0o40000).map((c) => utf8.decode(c.path)));
  for (const c of changes) {
    if (!c.entry) continue;
    const parts = utf8.decode(c.path).split("/");
    for (let n = 1; n < parts.length; n++) {
      if (leaves.has(parts.slice(0, n).join("/"))) throw new Error("a commit sets a path and something below it");
    }
  }
  // Nor two paths that macOS and Windows take for one: one would be written on, or through, the other.
  // The directories on the way count as well: `Link` and `link/x` are a link and something through it.
  const spelled = new Map<string, string>();
  for (const c of changes) {
    const parts = utf8.decode(c.path).split("/");
    for (let n = 1; n <= parts.length; n++) {
      const text = parts.slice(0, n).join("/");
      const key = folded(text);
      const other = spelled.get(key);
      if (other !== undefined && other !== text) throw new Error("a commit names two paths that differ only in case or in how their characters are composed");
      spelled.set(key, text);
    }
  }
  return { parents, rest, changes };
}

/// What follows a commit's tree and parents in Git's commit (`commit::rest_ok` on the other side):
/// an author line first, and in the header, up to the first empty line, no `tree` or `parent` line
/// and no NUL. What else Git's header may hold (a committer, an encoding, a signature) is kept.
export function restOk(rest: Uint8Array): boolean {
  let end = rest.length;
  for (let i = 0; i + 1 < rest.length; i++) {
    if (rest[i] === 10 && rest[i + 1] === 10) {
      end = i;
      break;
    }
  }
  const header = rest.subarray(0, end);
  if (header.includes(0)) return false;
  const lines = new TextDecoder("latin1").decode(header).split("\n");
  return lines[0]!.startsWith("author ") && !lines.some((line) => line.startsWith("tree ") || line.startsWith("parent "));
}

export const MODE_LINK = 0o120000;

/// Whether a link's content is somewhere a file system can make a link lead (`manifest::link_ok`
/// on the other side): not nothing, no NUL, and no longer than a path may be.
export function linkOk(target: Uint8Array): boolean {
  return target.length > 0 && target.length <= MAX_PATH && !target.includes(0);
}

/// A file, one that can be run, a link, a directory, another repository's commit.
export const MODES = [0o100644, 0o100755, 0o100664, 0o120000, 0o40000, 0o160000];
const utf8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/// Whether a path may be in a tree (`manifest::path_ok` on the other side), and is text:
/// every client that syncs writes the paths a commit names, and the server keeps them as text.
export function pathOk(path: Uint8Array): boolean {
  if (path.length === 0 || path.length > MAX_PATH || path.includes(0)) return false;
  let text: string;
  try {
    text = utf8.decode(path);
  } catch {
    return false;
  }
  return text.split("/").every((part) => partOk(part) && windowsOk(part));
}

/// What Windows makes of a name (`manifest::windows_ok` on the other side): a backslash is a
/// separator there, and a name ends at a `:` (what follows names a stream of the file). Each part,
/// up to its `:`, may be neither `.git` nor `.nv`, nor a parent or the directory itself; with a
/// backslash, no part is empty.
function windowsOk(name: string): boolean {
  const parted = name.includes("\\");
  return name.split("\\").every((part) => {
    const stem = part.split(":")[0]!;
    const spaced = stem.replace(/ +$/, "");
    return !(parted && part === "") && spaced !== "." && spaced !== ".." && (stem === "" || partOk(stem) || stem === "." || stem === "..");
  });
}

/// One name, an entry of a directory (`manifest::name_ok` on the other side): not empty, not `.`
/// or `..`, and not a name some file system takes for `.git` or `.nv` — in any case, with the
/// characters HFS+ ignores, with the dots and spaces NTFS drops, as NTFS's short name, or in
/// letters that are those once Unicode compatibility folding is done.
function partOk(part: string): boolean {
  if (part === "" || part === "." || part === ".." || part.includes("/") || part.includes("\0") || new TextEncoder().encode(part).length > 255) return false;
  const folded = part.replace(/[\u200c-\u200f\u202a-\u202e\u206a-\u206f\ufeff]/g, "").normalize("NFKC").toLowerCase().replace(/[. ]+$/, "");
  return folded !== ".git" && folded !== ".nv" && !/^(git|nv)~\d+$/.test(folded);
}

/// Whether a name may be an entry of a tree, and is text.
export function nameOk(name: Uint8Array): boolean {
  if (name.length === 0 || name.includes(0)) return false;
  try {
    const text = utf8.decode(name);
    return partOk(text) && windowsOk(text);
  } catch {
    return false;
  }
}

/// The longest path a tree may have (`manifest::MAX_PATH`): what macOS takes in one path.
export const MAX_PATH = 1024;

/// A name as a file system that does not tell case or composed characters apart sees it
/// (`manifest::fold_name` on the other side): composed, and folded as Unicode folds case in full,
/// as APFS does (`ß` is `ss`, `ſ` is `s`, `ﬁ` is `fi`). Lower, upper, then lower again brings
/// together what lower case alone leaves apart.
export const folded = (text: string) =>
  text
    .split("/")
    // To NTFS a name ends at a `:` and without the dots and spaces at its end: `a`, `a.`, `a ` and
    // `a::$DATA` are one file (`manifest::fold_name` on the other side).
    .map((name) => name.split(":")[0]!.replace(/[. ]+$/, "") || name)
    .join("/")
    .normalize("NFC")
    .toLowerCase()
    .toUpperCase()
    .toLowerCase()
    .normalize("NFC");

/// A ref's name: parts of letters, digits and `-_.`, with `/` between them.
export const refOk = (name: string) => name.length <= 255 && name.split("/").every((part) => /^[A-Za-z0-9_.-]+$/.test(part) && part !== "." && part !== "..");

const KIND_BLOB = 1;

/// An object's id: BLAKE3 over its kind and bytes.
function objectId(kind: number, bytes: Uint8Array): Uint8Array {
  const hasher = blake3.create();
  hasher.update(Uint8Array.of(kind));
  hasher.update(bytes);
  return hasher.digest();
}

export const idOf = (encoded: Uint8Array) => objectId(KIND_COMMIT, encoded);

/// A blob's id, worked out as its bytes go by: for content too large to hold.
export function blobHasher() {
  const hasher = blake3.create();
  hasher.update(Uint8Array.of(KIND_BLOB));
  return hasher;
}

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
