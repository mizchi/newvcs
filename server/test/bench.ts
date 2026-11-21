// How the cost of a push grows. Against a running server:
//   NV_SERVER=http://localhost:8799 node test/bench.ts
//
// - behind: the writer's base is N commits behind the tip, so the server has
//   N commits' paths to compare with.
// - paths: one commit changes N files.
// - repositories: the same writers spread over more repositories, each with
//   its own Durable Object.

import { encode, idOf, unhex, type Commit } from "../src/commit.ts";

const server = process.env.NV_SERVER ?? "http://localhost:8799";
const run = `t/bench-${Date.now()}`;
const text = (s: string) => new TextEncoder().encode(s);
/// A push is frames, each a length and bytes: the commit, then the contents it names.
function frame(bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(4 + bytes.length);
  new DataView(out.buffer).setUint32(0, bytes.length, true);
  out.set(bytes, 4);
  return out;
}
let serial = 0;

function commit(parent: string | null, paths: string[]): Uint8Array {
  const c: Commit = {
    parents: parent ? [unhex(parent)] : [],
    rest: text("author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nbench\n"),
    changes: paths.map((path) => ({ path: text(path), entry: { mode: 0o100644, id: idOf(text(`${path}#${serial++}`)) } })),
  };
  return encode(c);
}

async function push(repo: string, bytes: Uint8Array): Promise<{ status: string; commit: string }> {
  return (await (await fetch(`${server}/${repo}/push/main`, { method: "POST", body: frame(bytes) })).json()) as { status: string; commit: string };
}

const median = (xs: number[]) => xs.sort((a, b) => a - b)[Math.floor(xs.length / 2)]!;

async function timed(repo: string, bytes: Uint8Array): Promise<number> {
  const t = performance.now();
  const result = await push(repo, bytes);
  if (result.status !== "rebased" && result.status !== "updated") throw new Error(`unexpected ${result.status}`);
  return performance.now() - t;
}

// ------------------------------------------------------------------ behind
{
  const repo = `${run}-behind`;
  const root = (await push(repo, commit(null, ["README.md"]))).commit;
  let tip = root;
  let length = 1;
  const row: string[] = [];
  for (const behind of [1, 10, 100, 1000, 10000]) {
    // Grow the history to `behind` commits past the root, then push from the root.
    while (length <= behind) {
      tip = (await push(repo, commit(tip, [`history/${length % 500}/f${length}.ts`]))).commit;
      length++;
    }
    const samples: number[] = [];
    for (let i = 0; i < 9; i++) {
      samples.push(await timed(repo, commit(root, [`late/${behind}/${i}.ts`])));
      length++;
    }
    row.push(`${behind}: ${median(samples).toFixed(1)} ms`);
    tip = (await (await fetch(`${server}/${repo}/refs/main`)).json() as { tip: string }).tip;
  }
  console.log(`push from a base N commits behind the tip (median of 9)   ${row.join("   ")}`);
}

// ------------------------------------------------------------------- paths
{
  const repo = `${run}-paths`;
  let tip = (await push(repo, commit(null, ["README.md"]))).commit;
  const row: string[] = [];
  for (const paths of [1, 10, 100, 1000, 10000]) {
    const samples: number[] = [];
    let bytes = 0;
    for (let i = 0; i < 7; i++) {
      const c = commit(tip, Array.from({ length: paths }, (_, p) => `pkg/${p % 50}/src/module-${p}-${i}.ts`));
      bytes = c.length;
      const t = performance.now();
      tip = (await push(repo, c)).commit;
      samples.push(performance.now() - t);
    }
    row.push(`${paths}: ${median(samples).toFixed(1)} ms (${(bytes / 1024).toFixed(1)} KB)`);
  }
  console.log(`push of a commit that changes N paths (median of 7)       ${row.join("   ")}`);
}

// ------------------------------------------------------------ repositories
for (const [repos, writers] of [[1, 64], [8, 8], [64, 1]] as const) {
  const each = 40;
  const roots = await Promise.all(Array.from({ length: repos }, (_, r) => push(`${run}-many-${repos}-${r}`, commit(null, ["README.md"]))));
  const started = performance.now();
  await Promise.all(
    roots.flatMap((root, r) =>
      Array.from({ length: writers }, async (_, w) => {
        let base = root.commit;
        for (let i = 0; i < each; i++) base = (await push(`${run}-many-${repos}-${r}`, commit(base, [`agents/${w}/f${i % 5}.ts`]))).commit;
      }),
    ),
  );
  const seconds = (performance.now() - started) / 1000;
  console.log(`${repos} repositories x ${writers} writers x ${each} pushes: ${(repos * writers * each / seconds).toFixed(0)} pushes/s`);
}
