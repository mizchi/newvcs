// A directory handed to a repository whole, against a running server
// (`pnpm dev`, then `NV_SERVER=http://localhost:PORT node test/links.ts`).
//
// The directory arrives as tree objects. The files below it are not sent if
// the repository it came from has them: they change hands as rows.

import assert from "node:assert/strict";
import { blake3 } from "@noble/hashes/blake3.js";
import { blobIdOf, encode, hex, type Commit } from "../src/commit.ts";

const server = process.env.NV_SERVER ?? "http://localhost:8799";
const stamp = Date.now();
const [lib, app, stranger] = [`t/links-lib-${stamp}`, `t/links-app-${stamp}`, `t/links-stranger-${stamp}`];
const text = (s: string) => new TextEncoder().encode(s);
const u32 = (n: number) => new Uint8Array(new Uint32Array([n]).buffer);
const joined = (parts: Uint8Array[]) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  parts.reduce((at, p) => (out.set(p, at), at + p.length), 0);
  return out;
};
const frame = (parts: Uint8Array[]) => joined(parts.flatMap((p) => [u32(p.length), p]));
const frames = (bytes: Uint8Array) => {
  const out: Uint8Array[] = [];
  for (let at = 0; at < bytes.length; ) {
    const length = new DataView(bytes.buffer, bytes.byteOffset + at, 4).getUint32(0, true);
    out.push(bytes.subarray(at + 4, at + 4 + length));
    at += 4 + length;
  }
  return out;
};
/// A tree object as `src/manifest.rs` writes it, names in order.
const tree = (entries: [string, number, Uint8Array][]) => joined([...entries].sort(([a], [b]) => (a < b ? -1 : 1)).flatMap(([name, mode, id]) => [u32(mode), u32(text(name).length), text(name), id]));
const treeId = (bytes: Uint8Array) => blake3(joined([Uint8Array.of(2), bytes]));
const commit = (changes: Commit["changes"]) => encode({ parents: [], rest: text("author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nm\n"), changes });
const post = (path: string, body: Uint8Array | string) => fetch(`${server}/${path}`, { method: "POST", body });

// The library has three files. A fourth is in the directory and nowhere on the server.
const files: [string, Uint8Array][] = [1, 2, 3].map((i) => [`f${i}.rs`, text(`pub fn f${i}() {}\n`)]);
const unsent = text("only the writer has this\n");
const pushed = await post(`${lib}/push/main`, frame([commit(files.map(([path, bytes]) => ({ path: text(`src/${path}`), entry: { mode: 0o100644, id: blobIdOf(bytes) } }))), ...files.map(([, b]) => b)]));
assert.equal(((await pushed.json()) as { status: string }).status, "updated");

const deep = tree([["f3.rs", 0o100644, blobIdOf(files[2]![1])], ["local.rs", 0o100644, blobIdOf(unsent)]]);
const top = tree([["f1.rs", 0o100644, blobIdOf(files[0]![1])], ["f2.rs", 0o100755, blobIdOf(files[1]![1])], ["deep", 0o40000, treeId(deep)]]);
const linking = commit([{ path: text("vendor/lib"), entry: { mode: 0o40000, id: treeId(top) } }]);

{
  // A commit cannot set a directory the repository was never handed.
  const refused = await post(`${app}/push/main`, frame([linking]));
  assert.equal(refused.status, 400);
  // Nor can a directory be handed over with a hole in it.
  assert.equal((await post(`${app}/adopt/trees`, frame([top]))).status, 400);
}
{
  // Handed over naming where it came from: three files change hands, one is asked for.
  const answer = (await (await post(`${app}/adopt/trees?from=/${lib}`, frame([top, deep]))).json()) as { adopted: number; missing: string[] };
  assert.deepEqual(answer, { adopted: 3, missing: [hex(blobIdOf(unsent))] });
  const result = await post(`${app}/push/main`, frame([linking, unsent]));
  assert.equal(((await result.json()) as { status: string }).status, "updated");
  // Everything below the directory can now be read from this repository: the trees in one answer...
  const trees = frames(new Uint8Array(await (await post(`${app}/fetch/trees`, hex(treeId(top)) + "\n")).arrayBuffer()));
  assert.deepEqual(trees.map((t) => hex(treeId(t))).sort(), [hex(treeId(top)), hex(treeId(deep))].sort());
  // ...and the files, none of which this repository was ever sent but one.
  for (const bytes of [...files.map(([, b]) => b), unsent]) {
    const got = await fetch(`${server}/${app}/objects/${hex(blobIdOf(bytes))}`);
    assert.deepEqual(new Uint8Array(await got.arrayBuffer()), bytes);
  }
  // Handing the same directory over again moves nothing.
  const again = (await (await post(`${app}/adopt/trees?from=/${lib}`, frame([top, deep]))).json()) as { adopted: number; missing: string[] };
  assert.deepEqual(again, { adopted: 0, missing: [] });
}
{
  // Named from nowhere, or from a repository that has none of it, every file is still to be sent,
  // and none can be read meanwhile.
  for (const from of ["", `?from=/t/links-nobody-${stamp}`, "?from=not-a-name"]) {
    const answer = (await (await post(`${stranger}/adopt/trees${from}`, frame([top, deep]))).json()) as { adopted: number; missing: string[] };
    assert.equal(answer.adopted, 0);
    assert.equal(answer.missing.length, 4);
  }
  assert.equal((await fetch(`${server}/${stranger}/objects/${hex(blobIdOf(files[0]![1]))}`)).status, 404);
  assert.equal((await post(`${stranger}/fetch/trees`, hex(treeId(tree([]))) + "\n")).status, 404);
}
console.log("ok");
