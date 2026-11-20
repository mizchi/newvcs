// Against a running server (`pnpm dev`, then `NV_SERVER=http://localhost:PORT node test/e2e.ts`).
//
// First the three outcomes of a push, one by one. Then many writers at once,
// none of whom ever looks at what the others did: each keeps committing on
// top of its own last commit, as an agent in its own workspace would.

import assert from "node:assert/strict";
import { decode, encode, hex, idOf, unhex, type Commit } from "../src/commit.ts";

const server = process.env.NV_SERVER ?? "http://localhost:8799";
const repo = `e2e-${Date.now()}`;
const text = (s: string) => new TextEncoder().encode(s);
/// A push is frames, each a length and bytes: the commit, then the contents it names.
function frame(bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(4 + bytes.length);
  new DataView(out.buffer).setUint32(0, bytes.length, true);
  out.set(bytes, 4);
  return out;
}

let serial = 0;
function commit(parent: string | null, paths: string[], message: string): Uint8Array {
  const c: Commit = {
    parents: parent ? [unhex(parent)] : [],
    rest: text(`author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\n${message}\n`),
    // The content does not matter to the server; a fresh id per change stands in for it.
    changes: paths.map((path) => ({ path: text(path), entry: { mode: 0o100644, id: idOf(text(`${path}#${serial++}`)) } })),
  };
  return encode(c);
}

type Pushed = { status: string; commit: string; seq?: number; behind?: number; tip?: string; paths?: string[] };
async function push(name: string, ref: string, bytes: Uint8Array): Promise<Pushed> {
  const response = await fetch(`${server}/${name}/push/${ref}`, { method: "POST", body: frame(bytes) });
  return (await response.json()) as Pushed;
}
const get = async (path: string) => (await fetch(`${server}/${path}`)).json();

// ---------------------------------------------------------------- outcomes
{
  const root = await push(repo, "main", commit(null, ["README.md"], "root"));
  assert.equal(root.status, "updated");

  // Two writers start from the root. The first to arrive moves the ref.
  const b = await push(repo, "main", commit(root.commit, ["src/b.ts"], "b"));
  assert.deepEqual([b.status, b.seq], ["updated", 2]);
  // The second touched other paths: its changes are put on the new tip, and it is told the new id.
  const c = await push(repo, "main", commit(root.commit, ["src/c.ts", "docs/c.md"], "c"));
  assert.deepEqual([c.status, c.seq, c.behind], ["rebased", 3, 1]);
  const moved = decode(new Uint8Array(await (await fetch(`${server}/${repo}/commits/${c.commit}`)).arrayBuffer()));
  assert.equal(hex(moved.parents[0]!), b.commit);
  assert.deepEqual(moved.changes.map((ch) => new TextDecoder().decode(ch.path)), ["src/c.ts", "docs/c.md"]);

  // A third, also from the root, touched a path that has changed since: kept aside, ref unmoved.
  const d = await push(repo, "main", commit(root.commit, ["src/b.ts", "src/d.ts"], "d"));
  assert.deepEqual([d.status, d.paths, d.tip], ["conflict", ["src/b.ts"], c.commit]);
  // So is one that wants a file where there is now a directory's content, or the reverse.
  const e = await push(repo, "main", commit(root.commit, ["docs"], "e"));
  assert.deepEqual([e.status, e.paths], ["conflict", ["docs"]]);
  assert.deepEqual(await get(`${repo}/refs/main`), { tip: c.commit, seq: 3 });
  assert.deepEqual(((await get(`${repo}/divergent/main`)) as { id: string }[]).map((x) => x.id), [d.commit, e.commit]);

  // A commit on something the ref never was is refused.
  const stray = await push(repo, "main", commit("ab".repeat(32), ["x"], "stray"));
  assert.equal(stray.status, "unknown-base");
  // Refs are independent.
  assert.equal((await push(repo, "other", commit(null, ["src/b.ts"], "other"))).status, "updated");
  console.log("outcomes: updated, rebased, conflict, unknown-base all as specified");
}

// -------------------------------------------------------------- many writers
async function storm(writers: number, each: number, sharedEvery: number) {
  const name = `${repo}-storm-${writers}-${sharedEvery}`;
  const root = await push(name, "main", commit(null, ["README.md"], "root"));
  const tally: Record<string, number> = {};
  const latencies: number[] = [];
  const started = performance.now();
  await Promise.all(
    Array.from({ length: writers }, async (_, w) => {
      // Never fetches: its base is the last commit of its own the server accepted.
      let base = root.commit;
      for (let i = 0; i < each; i++) {
        const shared = sharedEvery > 0 && i % sharedEvery === sharedEvery - 1;
        const paths = shared ? ["shared/config.json"] : [`agents/${w}/file-${i % 7}.ts`];
        const t = performance.now();
        const result = await push(name, "main", commit(base, paths, `w${w} #${i}`));
        latencies.push(performance.now() - t);
        tally[result.status] = (tally[result.status] ?? 0) + 1;
        if (result.status === "updated" || result.status === "rebased") base = result.commit;
      }
    }),
  );
  const seconds = (performance.now() - started) / 1000;

  // The ref's history is one line: each commit's parent is the commit before it.
  const head = (await get(`${name}/refs/main`)) as { tip: string; seq: number };
  const log = (await get(`${name}/log/main?after=0`)) as { seq: number; id: string }[];
  const accepted = (tally.updated ?? 0) + (tally.rebased ?? 0);
  assert.equal(log.length, accepted + 1);
  assert.deepEqual(log.map((l) => l.seq), Array.from({ length: log.length }, (_, i) => i + 1));
  assert.equal(log.at(-1)!.id, head.tip);
  for (const i of [1, Math.floor(log.length / 2), log.length - 1]) {
    const bytes = new Uint8Array(await (await fetch(`${server}/${name}/commits/${log[i]!.id}`)).arrayBuffer());
    assert.equal(hex(idOf(bytes)), log[i]!.id);
    assert.equal(hex(decode(bytes).parents[0]!), log[i - 1]!.id);
  }
  // Nothing was dropped: every push is in the history or kept aside.
  const aside = ((await get(`${name}/divergent/main`)) as unknown[]).length;
  assert.equal(aside, tally.conflict ?? 0);
  assert.equal(accepted + aside, writers * each);

  latencies.sort((a, b) => a - b);
  const pct = (p: number) => latencies[Math.floor((latencies.length - 1) * p)]!.toFixed(1);
  console.log(
    `${writers} writers x ${each} pushes${sharedEvery ? `, every ${sharedEvery}th on a shared file` : ", disjoint paths"}: ` +
      `${(writers * each / seconds).toFixed(0)} pushes/s, p50 ${pct(0.5)} ms, p99 ${pct(0.99)} ms; ` +
      Object.entries(tally).map(([k, v]) => `${v} ${k}`).join(", "),
  );
}

await storm(1, 400, 0);
await storm(16, 100, 0);
await storm(64, 50, 0);
await storm(16, 100, 10);
console.log("history linear, ids verified, no push lost");
