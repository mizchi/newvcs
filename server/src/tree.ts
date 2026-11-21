// A directory as nv stores it: for each name, in order, a mode, the name and the id of
// what is there (`src/manifest.rs`). The server reads one only to learn what is below it.

export const MODE_DIR = 0o40000;
/// A commit of another repository. Its id is Git's and names nothing here.
export const MODE_GITLINK = 0o160000;
export const KIND_TREE = 2;

export type TreeEntry = { mode: number; name: Uint8Array; id: Uint8Array };

export function decodeTree(bytes: Uint8Array): TreeEntry[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const entries: TreeEntry[] = [];
  for (let at = 0; at < bytes.length; ) {
    if (at + 8 > bytes.length) throw new Error("a tree object is cut short");
    const [mode, length] = [view.getUint32(at, true), view.getUint32(at + 4, true)];
    const end = at + 8 + length + 32;
    if (end > bytes.length) throw new Error("a tree object is cut short");
    // The id is a copy: ids are passed on to a repository's object, and a view would take the tree along.
    entries.push({ mode, name: bytes.subarray(at + 8, at + 8 + length), id: bytes.slice(at + 8 + length, end) });
    at = end;
  }
  return entries;
}
