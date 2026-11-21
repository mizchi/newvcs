// Time a push and a fetch of many small files, and the upload of a large one.
// Works against the server before and after packs: only requests are timed.
import { blobHasher, blobIdOf, encode, hex, type Commit } from "../src/commit.ts";

const server = process.env.NV_SERVER ?? "http://localhost:8799";
const text = (s: string) => new TextEncoder().encode(s);
const frame = (parts: Uint8Array[]) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + 4 + p.length, 0));
  const view = new DataView(out.buffer);
  let at = 0;
  for (const part of parts) {
    view.setUint32(at, part.length, true);
    out.set(part, at + 4);
    at += 4 + part.length;
  }
  return out;
};
const best = async (times: number, run: (i: number) => Promise<void>) => {
  let least = Infinity;
  for (let i = 0; i < times; i++) {
    const started = performance.now();
    await run(i);
    least = Math.min(least, performance.now() - started);
  }
  return least.toFixed(0);
};
for (const count of [200, 2000]) {
  const sets = Array.from({ length: 5 }, (_, round) =>
    Array.from({ length: count }, (_, i) => [`src/f${i}.ts`, text(`// ${round} ${Date.now()}\nexport const v${i} = ${i};\n`.repeat(40))] as [string, Uint8Array]),
  );
  const commit = (files: [string, Uint8Array][]) => {
    const c: Commit = { parents: [], rest: text("author t <t@e> 1 +0000\ncommitter t <t@e> 1 +0000\n\nm\n"), changes: files.map(([path, bytes]) => ({ path: text(path), entry: { mode: 0o100644, id: blobIdOf(bytes) } })) };
    return encode(c);
  };
  const repo = `t/bench-${count}-${Date.now()}`;
  const push = await best(5, async (i) => {
    const files = sets[i]!;
    const r = await fetch(`${server}/${repo}/push/r${i}`, { method: "POST", body: frame([commit(files), ...files.map(([, b]) => b)]) });
    if (!r.ok) throw new Error(await r.text());
  });
  const fetchAll = await best(5, async (i) => {
    const r = await fetch(`${server}/${repo}/fetch/objects`, { method: "POST", body: sets[i]!.map(([, b]) => hex(blobIdOf(b)) + "\n").join("") });
    const bytes = new Uint8Array(await r.arrayBuffer());
    if (bytes.length < count * 40) throw new Error("short answer");
  });
  console.log(`${count} small files (${(sets[0]!.reduce((n, [, b]) => n + b.length, 0) / 1024).toFixed(0)} KB): push ${push} ms, fetch ${fetchAll} ms`);
}
for (const megabytes of [8, 256]) {
  const large = new Uint8Array(megabytes << 20);
  for (let i = 0; i < large.length; i += 4096) large[i] = (i / 4096 + megabytes) % 251;
  const hasher = blobHasher();
  hasher.update(large);
  const id = hex(hasher.digest());
  const started = performance.now();
  const r = await fetch(`${server}/t/bench-large-${Date.now()}/objects/${id}`, { method: "PUT", body: large });
  console.log(`${megabytes} MB by itself: ${r.status}, ${(performance.now() - started).toFixed(0)} ms`);
}
