// A directory as nv stores it: for each name, in order, a mode, the name and the id of
// what is there (`src/manifest.rs`). The server reads one only to learn what is below it.

import { MODES, nameOk } from "./commit.ts";

export const MODE_DIR = 0o40000;
/// A commit of another repository. Its id is Git's and names nothing here.
export const MODE_GITLINK = 0o160000;
export const KIND_TREE = 2;

export type TreeEntry = { mode: number; name: Uint8Array; id: Uint8Array };

/// Throws on bytes that are not a tree a client could have written: every client that syncs a
/// directory set whole writes its files where its names say.
export function decodeTree(bytes: Uint8Array): TreeEntry[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const entries: TreeEntry[] = [];
  for (let at = 0; at < bytes.length; ) {
    if (at + 8 > bytes.length) throw new Error("a tree object is cut short");
    const [mode, length] = [view.getUint32(at, true), view.getUint32(at + 4, true)];
    const end = at + 8 + length + 32;
    if (end > bytes.length) throw new Error("a tree object is cut short");
    const name = bytes.subarray(at + 8, at + 8 + length);
    if (!nameOk(name)) throw new Error("a tree names an entry no tree may have");
    if (!MODES.includes(mode)) throw new Error("a tree gives an entry a mode no file has");
    // In order and once each, as every tree is written.
    const last = entries[entries.length - 1];
    if (last && compare(last.name, name) >= 0) throw new Error("a tree has an entry out of order, or twice");
    // The id is a copy: ids are passed on to a repository's object, and a view would take the tree along.
    entries.push({ mode, name, id: bytes.slice(at + 8 + length, end) });
    at = end;
  }
  return entries;
}

function compare(a: Uint8Array, b: Uint8Array): number {
  for (let i = 0; i < Math.min(a.length, b.length); i++) if (a[i] !== b[i]) return a[i]! - b[i]!;
  return a.length - b.length;
}
