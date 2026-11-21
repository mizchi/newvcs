// Where file contents go, against a running server
// (`pnpm dev`, then `NV_SERVER=http://localhost:PORT node test/tiers.ts`).
//
// Small contents of one push are one object in the bucket, however many
// there are. A large one goes to the bucket by itself, straight from the
// writer, and is checked as it streams. Which repository has what is the
// repository's to say.

import assert from "node:assert/strict";
import { blobIdOf, encode, hex, type Commit } from "../src/commit.ts";

const server = process.env.NV_SERVER ?? "http://localhost:8799";
const stamp = Date.now();
const [repo, other] = [`t/tiers-${stamp}`, `t/tiers-other-${stamp}`];
const text = (s: string) => new TextEncoder().encode(s);

function frame(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + 4 + p.length, 0));
  const view = new DataView(out.buffer);
  let at = 0;
  for (const part of parts) {
    view.setUint32(at, part.length, true);
    out.set(part, at + 4);
    at += 4 + part.length;
  }
  return out;
}

function frames(bytes: Uint8Array): Uint8Array[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out: Uint8Array[] = [];
  for (let at = 0; at < bytes.length; ) {
    const length = view.getUint32(at, true);
    out.push(bytes.subarray(at + 4, at + 4 + length));
    at += 4 + length;
  }
  return out;
}

function commit(parent: Uint8Array | null, files: [string, Uint8Array][]): Uint8Array {
  const c: Commit = {
    parents: parent ? [parent] : [],
    rest: text("author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nfiles\n"),
    changes: files.map(([path, bytes]) => ({ path: text(path), entry: { mode: 0o100644, id: blobIdOf(bytes) } })),
  };
  return encode(c);
}

type Storage = { contents: number; packs: number; direct: number; bytes: number };
const storage = async (name: string) => (await (await fetch(`${server}/${name}/storage/now`)).json()) as Storage;

async function fetched(name: string, ids: Uint8Array[]): Promise<{ direct: string[]; found: Map<string, Uint8Array> }> {
  const response = await fetch(`${server}/${name}/fetch/objects`, { method: "POST", body: ids.map((id) => hex(id) + "\n").join("") });
  assert.equal(response.status, 200);
  const [head, ...pairs] = frames(new Uint8Array(await response.arrayBuffer()));
  const found = new Map<string, Uint8Array>();
  for (let i = 0; i + 1 < pairs.length; i += 2) found.set(hex(pairs[i]!), pairs[i + 1]!);
  return { direct: (JSON.parse(new TextDecoder().decode(head)) as { direct: string[] }).direct, found };
}

// ---------------------------------------------------------------- small contents, together
const small: [string, Uint8Array][] = Array.from({ length: 200 }, (_, i) => [`src/f${i}.ts`, text(`export const v${i} = ${i};\n`.repeat(1 + (i % 7)))]);
{
  const started = performance.now();
  const response = await fetch(`${server}/${repo}/push/main`, { method: "POST", body: frame([commit(null, small), ...small.map(([, bytes]) => bytes)]) });
  assert.equal(((await response.json()) as { status: string }).status, "updated");
  const took = performance.now() - started;
  const now = await storage(repo);
  assert.deepEqual([now.contents, now.packs, now.direct], [200, 1, 0], "two hundred files are one object in the bucket");
  console.log(`push of 200 small files: 1 object written to the bucket, ${now.bytes} bytes, ${took.toFixed(0)} ms`);

  const all = await fetched(repo, small.map(([, bytes]) => blobIdOf(bytes)));
  assert.equal(all.found.size, 200);
  for (const [, bytes] of small) assert.deepEqual(all.found.get(hex(blobIdOf(bytes))), bytes);
  // One of them alone comes out of the middle of that object.
  const one = small[137]![1];
  const alone = await fetched(repo, [blobIdOf(one)]);
  assert.deepEqual([...alone.found.values()], [one]);
  const direct = await fetch(`${server}/${repo}/objects/${hex(blobIdOf(one))}`);
  assert.deepEqual(new Uint8Array(await direct.arrayBuffer()), one);
  // The same content pushed again is not placed again.
  const again = commit(null, [["copy.ts", one]]);
  await fetch(`${server}/${repo}/push/second`, { method: "POST", body: frame([again, one]) });
  assert.equal((await storage(repo)).contents, 200);
}

// ---------------------------------------------------------------- a large one, by itself
const large = new Uint8Array(8 * 1024 * 1024);
for (let i = 0; i < large.length; i += 4096) large[i] = (i / 4096) % 251;
const largeId = hex(blobIdOf(large));
{
  const started = performance.now();
  const put = await fetch(`${server}/${repo}/objects/${largeId}`, { method: "PUT", body: large });
  assert.equal(put.status, 204);
  console.log(`8 MB uploaded straight to the bucket and checked as it streamed: ${(performance.now() - started).toFixed(0)} ms`);
  const now = await storage(repo);
  assert.deepEqual([now.contents, now.packs, now.direct], [201, 1, 1]);
  // A fetch does not carry it; it says to get it directly.
  const asked = await fetched(repo, [blobIdOf(large), blobIdOf(small[0]![1])]);
  assert.deepEqual([asked.direct, asked.found.size], [[largeId], 1]);
  const got = new Uint8Array(await (await fetch(`${server}/${repo}/objects/${largeId}`)).arrayBuffer());
  assert.equal(got.length, large.length);
  assert.deepEqual(blobIdOf(got), blobIdOf(large));

  // Bytes that are not what the name says are refused, and nothing is left under the name.
  const wrong = hex(blobIdOf(text("something else")));
  const refused = await fetch(`${server}/${repo}/objects/${wrong}`, { method: "PUT", body: large.subarray(0, 100_000) });
  assert.equal(refused.status, 400);
  assert.equal((await fetch(`${server}/${repo}/objects/${wrong}`)).status, 404);
  assert.equal((await storage(repo)).direct, 1);
}

// ---------------------------------------------------------------- a repository has what it was given
{
  // Knowing an id is not having the content: another repository is not handed it.
  assert.equal((await fetch(`${server}/${other}/objects/${largeId}`)).status, 404);
  const none = await fetched(other, [blobIdOf(large), blobIdOf(small[0]![1])]);
  assert.deepEqual([none.direct, none.found.size], [[], 0]);
  // Given the bytes, it has them too, and the bucket holds them once.
  assert.equal((await fetch(`${server}/${other}/objects/${largeId}`, { method: "PUT", body: large })).status, 204);
  assert.equal((await fetch(`${server}/${other}/objects/${largeId}`)).status, 200);
}

console.log("ok");
