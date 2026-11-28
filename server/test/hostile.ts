// What a writer who means harm can send, against a running server
// (`pnpm dev`, then `NV_SERVER=http://localhost:PORT node test/hostile.ts`).
//
// A commit is bytes from a client, and every client that syncs writes the
// paths it names. So a commit that names a path no tree may have, or
// contents the repository was never given, is refused here, before it is
// anyone's history; and an upload that is not what it says it is cannot
// touch what is already stored.

import assert from "node:assert/strict";
import { blake3 } from "@noble/hashes/blake3.js";
import { blobHasher, blobIdOf, encode, hex, idOf, type Commit } from "../src/commit.ts";

const server = process.env.NV_SERVER ?? "http://localhost:8799";
const stamp = Date.now();
const repo = `t/hostile-${stamp}`;
const text = (s: string) => new TextEncoder().encode(s);
const u32 = (n: number) => new Uint8Array(new Uint32Array([n]).buffer);
const frame = (...parts: Uint8Array[]) => {
  const all = parts.flatMap((p) => [u32(p.length), p]);
  const out = new Uint8Array(all.reduce((n, p) => n + p.length, 0));
  all.reduce((at, p) => (out.set(p, at), at + p.length), 0);
  return out;
};
const rest = text("author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nm\n");
const commit = (parent: string | null, changes: Commit["changes"]) => encode({ parents: parent ? [Uint8Array.from(parent.match(/../g)!.map((b) => parseInt(b, 16)))] : [], rest, changes });
const push = (ref: string, ...parts: Uint8Array[]) => fetch(`${server}/${repo}/push/${ref}`, { method: "POST", body: frame(...parts) });
const tip = async () => ((await (await fetch(`${server}/${repo}/refs/main`)).json()) as { tip: string; seq: number });

const good = text("good\n");
const started = (await (await push("main", commit(null, [{ path: text("a.txt"), entry: { mode: 0o100644, id: blobIdOf(good) } }]), good)).json()) as { status: string };
assert.equal(started.status, "updated");
const before = await tip();

// ---------------------------------------------------------------- paths no tree may have
const bytes = (...b: number[]) => Uint8Array.from(b);
const paths: [string, Uint8Array][] = [
  ["empty", text("")],
  ["a directory, not a file", text("d/")],
  ["above the workspace", text("../escape.txt")],
  ["above, by way of a directory", text("src/../../escape.txt")],
  ["a dot", text("./a.txt")],
  ["two slashes", text("a//b")],
  ["from the root", text("/etc/hosts")],
  ["Git's own directory", text(".git/hooks/pre-commit")],
  ["Git's own directory, in capitals", text("sub/.GIT/config")],
  ["nv's own directory", text(".nv/workspace")],
  ["a NUL", bytes(0x61, 0, 0x62)],
  ["not text", bytes(0x61, 0xff, 0x62)],
  ["a name of 300 bytes", text("x".repeat(300))],
  ["a path of 5,000 bytes", text(Array.from({ length: 100 }, () => "x".repeat(49)).join("/"))],
];
for (const [what, path] of paths) {
  const refused = await push("main", commit(before.tip, [{ path, entry: { mode: 0o100644, id: blobIdOf(good) } }]), good);
  assert.equal(refused.status, 400, `a path that is ${what} was taken`);
}
// A mode no file has, and one path named twice.
assert.equal((await push("main", commit(before.tip, [{ path: text("m.txt"), entry: { mode: 0, id: blobIdOf(good) } }]), good)).status, 400);
assert.equal((await push("main", commit(before.tip, [{ path: text("m.txt"), entry: { mode: 0o100777, id: blobIdOf(good) } }]), good)).status, 400);
const twice = [{ path: text("t.txt"), entry: { mode: 0o100644, id: blobIdOf(good) } }, { path: text("t.txt"), entry: null }];
assert.equal((await push("main", commit(before.tip, twice), good)).status, 400);
// A ref's name is a name.
for (const ref of ["odd ref", "x\u2603", "semi;colon"]) {
  const refused = await fetch(`${server}/${repo}/push/${encodeURIComponent(ref)}`, { method: "POST", body: frame(commit(null, [{ path: text("a.txt"), entry: { mode: 0o100644, id: blobIdOf(good) } }]), good) });
  assert.equal(refused.status, 400, `a ref named ${JSON.stringify(ref)} was taken`);
}

// ---------------------------------------------------------------- contents never given
const never = text("nobody sent these bytes\n");
const naming = await push("main", commit(before.tip, [{ path: text("new.txt"), entry: { mode: 0o100644, id: blobIdOf(never) } }]));
assert.equal(naming.status, 400, "a commit that names contents the repository does not have was taken");
// Nothing of all that moved the ref, and a writer who means well is not in anyone's way.
assert.deepEqual(await tip(), before);
const fine = text("fine\n");
const after = (await (await push("main", commit(before.tip, [{ path: text("b.txt"), entry: { mode: 0o100755, id: blobIdOf(fine) } }]), fine)).json()) as { status: string };
assert.equal(after.status, "updated");

// ---------------------------------------------------------------- an upload that is not what it says
const large = new Uint8Array(1_500_000);
for (let i = 0; i < large.length; i += 997) large[i] = (i / 997) % 251;
const hasher = blobHasher();
hasher.update(large);
const id = hex(hasher.digest());
assert.equal((await fetch(`${server}/${repo}/objects/${id}`, { method: "PUT", body: large })).status, 204);
// From another repository, under the same name, bytes that are not those bytes.
const other = `t/hostile-other-${stamp}`;
const wrong = await fetch(`${server}/${other}/objects/${id}`, { method: "PUT", body: text("garbage") });
assert.equal(wrong.status, 400);
const still = await fetch(`${server}/${repo}/objects/${id}`);
assert.equal(still.status, 200, "what was stored is still there");
assert.deepEqual(new Uint8Array(await still.arrayBuffer()), large);
// And the other repository was not given it by trying.
assert.equal((await fetch(`${server}/${other}/objects/${id}`)).status, 404);
// The right bytes, sent again by someone else, are taken, and stored once.
assert.equal((await fetch(`${server}/${other}/objects/${id}`, { method: "PUT", body: large })).status, 204);
assert.deepEqual(new Uint8Array(await (await fetch(`${server}/${other}/objects/${id}`)).arrayBuffer()), large);
// ---------------------------------------------------------------- directories set whole
// A tree, as nv writes one: mode, name length, name, id; names in order.
const tree = (...entries: [number, Uint8Array, Uint8Array][]) =>
  Uint8Array.from(entries.flatMap(([mode, name, id]) => [...u32(mode), ...u32(name.length), ...name, ...id]));
const treeId = (bytes: Uint8Array) => {
  const hasher = blake3.create();
  hasher.update(Uint8Array.of(2));
  hasher.update(bytes);
  return hasher.digest();
};
const adopt = (...trees: Uint8Array[]) => fetch(`${server}/${repo}/adopt/trees`, { method: "POST", body: frame(...trees) });
const payload = text("planted\n");
// Names that a checkout would write outside the workspace, or into Git's directory or nv's own.
const names: [string, Uint8Array][] = [
  ["above the workspace", text("../../outside/pwned")],
  ["Git's hooks, from above", text("../.git/hooks/pre-commit")],
  ["a path, not a name", text("a/b")],
  ["two dots", text("..")],
  ["empty", text("")],
  ["Git's directory", text(".git")],
  ["Git's directory, with a dot NTFS drops", text(".git.")],
  ["Git's directory, with a character HFS+ ignores", text(".g\u200cit")],
  ["Git's directory, as NTFS's short name", text("GIT~1")],
  ["Git's directory, in full-width letters", text(".\uff47it")],
  ["nv's directory", text(".NV")],
];
for (const [what, name] of names) {
  const bad = tree([0o100755, name, blobIdOf(payload)]);
  assert.equal((await adopt(bad)).status, 400, `a tree naming an entry that is ${what} was taken`);
  const refused = await push("main", commit(before.tip, [{ path: text("d"), entry: { mode: 0o40000, id: treeId(bad) } }]), payload);
  assert.equal(refused.status, 400, `a commit setting a directory named ${what} was taken`);
}
// Two entries of one name, entries out of order, and modes no file has.
assert.equal((await adopt(tree([0o100644, text("same"), blobIdOf(payload)], [0o40000, text("same"), blobIdOf(payload)]))).status, 400);
assert.equal((await adopt(tree([0o100644, text("b"), blobIdOf(payload)], [0o100644, text("a"), blobIdOf(payload)]))).status, 400);
assert.equal((await adopt(tree([0, text("m"), blobIdOf(payload)]))).status, 400);
assert.equal((await adopt(tree([0o104755, text("m"), blobIdOf(payload)]))).status, 400);
assert.deepEqual(await tip(), (await tip()));
// A directory's id given to a file, and a file's to a directory: refused, and the ref can still be synced.
const ok = tree([0o100644, text("in.txt"), blobIdOf(good)]);
assert.equal((await adopt(ok)).status, 200);
const now = await tip();
assert.equal((await push("main", commit(now.tip, [{ path: text("f.txt"), entry: { mode: 0o100644, id: treeId(ok) } }]))).status, 400);
assert.equal((await push("main", commit(now.tip, [{ path: text("g"), entry: { mode: 0o40000, id: blobIdOf(payload) } }]), payload)).status, 400);
assert.deepEqual(await tip(), now);
const honest = (await (await push("main", commit(now.tip, [{ path: text("d"), entry: { mode: 0o40000, id: treeId(ok) } }]))).json()) as { status: string };
assert.equal(honest.status, "updated");
const fetched = await fetch(`${server}/${repo}/fetch/trees`, { method: "POST", body: hex(treeId(ok)) + "\n" });
assert.equal(fetched.status, 200);
assert.equal((await fetch(`${server}/${repo}/fetch/trees`, { method: "POST", body: hex(blobIdOf(payload)) + "\n" })).status, 404, "a file is not handed out as a directory");

// ---------------------------------------------------------------- history a ref cannot hold
const head = await tip();
const merge = encode({ parents: [Uint8Array.from(head.tip.match(/../g)!.map((b) => parseInt(b, 16))), new Uint8Array(32).fill(0xab)], rest, changes: [] });
assert.equal((await push("main", merge)).status, 400, "a merge with a parent nobody has was taken");
assert.deepEqual(await tip(), head);

// ---------------------------------------------------------------- addresses
assert.equal((await fetch(`${server}/t%zz/x/refs/main`)).status, 400, "a broken escape");
assert.equal((await fetch(`${server}/t%2Fa/b/refs/main`)).status, 400, "a slash in an owner");
for (const bad of ["abc", "-5", "1e300", "1.5"]) assert.equal((await fetch(`${server}/${repo}/since/main?after=${bad}`)).status, 400, `after=${bad}`);
assert.equal((await fetch(`${server}/${repo}/since/main?after=0`)).status, 200);
// ---------------------------------------------------------------- names a file system takes for one
const twins: [string, string][] = [
  ["README", "readme"],
  ["caf\u00e9.txt", "cafe\u0301.txt"],
];
for (const [one, other] of twins) {
  const a = text("a\n"), b = text("b\n");
  const both = commit((await tip()).tip, [
    { path: text(one), entry: { mode: 0o100644, id: blobIdOf(a) } },
    { path: text(other), entry: { mode: 0o100644, id: blobIdOf(b) } },
  ].sort((x, y) => (hex(x.path) < hex(y.path) ? -1 : 1)));
  assert.equal((await push("main", both, a, b)).status, 400, `${one} and ${other} in one commit were taken`);
  const pair = [[0o100644, text(one), blobIdOf(a)], [0o100644, text(other), blobIdOf(b)]].sort((x, y) => (hex(x[1] as Uint8Array) < hex(y[1] as Uint8Array) ? -1 : 1)) as [number, Uint8Array, Uint8Array][];
  assert.equal((await adopt(tree(...pair))).status, 400, `a tree with ${one} and ${other} was taken`);
}
// A link and a directory of one name, as a file system that folds case sees them.
const linkAndDir = commit((await tip()).tip, [
  { path: text("Link"), entry: { mode: 0o120000, id: blobIdOf(text("../outside")) } },
  { path: text("link/pwned"), entry: { mode: 0o100644, id: blobIdOf(payload) } },
]);
assert.equal((await push("main", linkAndDir, text("../outside"), payload)).status, 400);

// ---------------------------------------------------------------- paths longer than a file system takes
const long = text("d/".repeat(600) + "f");
assert.equal((await push("main", commit((await tip()).tip, [{ path: long, entry: { mode: 0o100644, id: blobIdOf(payload) } }]), payload)).status, 400);
// A chain of directories, each one name deep, handed over at once: too deep, refused whole.
{
  let below = tree([0o100644, text("f"), blobIdOf(payload)]);
  const chain = [below];
  for (let i = 0; i < 600; i++) {
    below = tree([0o40000, text("d"), treeId(below)]);
    chain.push(below);
  }
  assert.equal((await adopt(...chain.reverse())).status, 400, "a tree deeper than a path may be was taken");
}

// ---------------------------------------------------------------- a directory's id, taken over as a file
{
  const source = `t/hostile-source-${stamp}`;
  const inner = tree([0o100644, text("x"), blobIdOf(good)]);
  const handed = await fetch(`${server}/${source}/adopt/trees`, { method: "POST", body: frame(inner) });
  assert.equal(handed.status, 200);
  const disguised = tree([0o100644, text("f"), treeId(inner)]);
  const taken = (await (await fetch(`${server}/${repo}/adopt/trees?from=/${source}`, { method: "POST", body: frame(disguised) })).json()) as { adopted: number };
  assert.equal(taken.adopted, 0, "a directory of another repository was taken over as a file's content");
  const now2 = await tip();
  assert.equal((await push("main", commit(now2.tip, [{ path: text("f"), entry: { mode: 0o100644, id: treeId(inner) } }]))).status, 400);
}

// ---------------------------------------------------------------- what an eleventh reviewer got past
// Pairs APFS folds that lower case alone does not: refused in one commit as `Link` and `link/x` are.
for (const [link, dir] of [["ss", "\u00df"], ["s", "\u017f"], ["fi", "\ufb01"], ["\u03c3", "\u03c2"]] as const) {
  const target = text("../outside");
  const changes = [
    { path: text(link), entry: { mode: 0o120000, id: blobIdOf(target) } },
    { path: text(`${dir}/pwned`), entry: { mode: 0o100644, id: blobIdOf(payload) } },
  ].sort((x, y) => (hex(x.path) < hex(y.path) ? -1 : 1));
  assert.equal((await push("main", commit((await tip()).tip, changes), target, payload)).status, 400, `${link} and ${dir}/pwned were taken`);
}
// Twins spread over two commits of one push, or over two pushes, against what the ref has.
{
  const target = text("../outside");
  const first = commit((await tip()).tip, [{ path: text("Twin"), entry: { mode: 0o120000, id: blobIdOf(target) } }]);
  const chained = encode({ parents: [idOf(first)], rest, changes: [{ path: text("twin/pwned"), entry: { mode: 0o100644, id: blobIdOf(payload) } }] });
  const two = await fetch(`${server}/${repo}/push/main?commits=2`, { method: "POST", body: frame(first, chained, target, payload) });
  assert.equal(two.status, 400, "twins in two commits of one push were taken");
  const landed = (await (await push("main", first, target)).json()) as { status: string };
  assert.equal(landed.status, "updated");
  const later = commit((await tip()).tip, [{ path: text("twin/pwned"), entry: { mode: 0o100644, id: blobIdOf(payload) } }]);
  assert.equal((await push("main", later, payload)).status, 400, "a path whose twin the ref has was taken");
  // Once the twin is gone, the name is free.
  const gone = (await (await push("main", commit((await tip()).tip, [{ path: text("Twin"), entry: null }]))).json()) as { status: string };
  assert.equal(gone.status, "updated");
  const free = (await (await push("main", commit((await tip()).tip, [{ path: text("twin/fine"), entry: { mode: 0o100644, id: blobIdOf(payload) } }]), payload)).json()) as { status: string };
  assert.equal(free.status, "updated");
  // A directory set whole whose entry has a twin in the same commit.
  const inner = tree([0o100644, text("x"), blobIdOf(good)]);
  assert.equal((await adopt(inner)).status, 200);
  const both = commit((await tip()).tip, [
    { path: text("w"), entry: { mode: 0o40000, id: treeId(inner) } },
    { path: text("w/X"), entry: { mode: 0o100644, id: blobIdOf(payload) } },
  ]);
  assert.equal((await push("main", both, payload)).status, 400, "a directory's entry and its twin in one commit were taken");
}
// A directory whose files the repository was never sent, and a file entry holding a directory's id.
{
  const unsent = text(`never sent ${stamp}\n`);
  const hollow = tree([0o100644, text("x"), blobIdOf(unsent)]);
  const answer = (await (await adopt(hollow)).json()) as { missing: string[] };
  assert.equal(answer.missing.length, 1);
  assert.equal((await push("main", commit((await tip()).tip, [{ path: text("hollow"), entry: { mode: 0o40000, id: treeId(hollow) } }]))).status, 400, "a directory with a file nobody sent was taken");
  // Sent with the push, it is whole.
  const filled = (await (await push("main", commit((await tip()).tip, [{ path: text("hollow"), entry: { mode: 0o40000, id: treeId(hollow) } }]), unsent)).json()) as { status: string };
  assert.equal(filled.status, "updated");
  const inner = tree([0o100644, text("y"), blobIdOf(good)]);
  assert.equal((await adopt(inner)).status, 200);
  const disguised = tree([0o100644, text("f"), treeId(inner)]);
  await adopt(disguised);
  assert.equal((await push("main", commit((await tip()).tip, [{ path: text("disguised"), entry: { mode: 0o40000, id: treeId(disguised) } }]))).status, 400, "a file entry with a directory's id was taken");
}
// Too long once the place a directory is set at counts, and too deep built over several adopts.
{
  const leaf = tree([0o100644, text("f".repeat(200)), blobIdOf(good)]);
  let below = leaf;
  const chain = [below];
  for (let i = 0; i < 2; i++) {
    below = tree([0o40000, text("d".repeat(100)), treeId(below)]);
    chain.push(below);
  }
  assert.equal((await adopt(...chain.reverse())).status, 200);
  const deep = text(Array.from({ length: 8 }, () => "p".repeat(99)).join("/"));
  assert.equal((await push("main", commit((await tip()).tip, [{ path: deep, entry: { mode: 0o40000, id: treeId(below) } }]))).status, 400, "a directory set where its paths grow too long was taken");
  let top = below;
  let refused = false;
  for (let round = 0; round < 5 && !refused; round++) {
    const more = [];
    for (let i = 0; i < 2; i++) {
      top = tree([0o40000, text("e".repeat(100)), treeId(top)]);
      more.push(top);
    }
    refused = (await adopt(...more.reverse())).status === 400;
  }
  assert.ok(refused, "a tree built too deep over several adopts was taken");
}

// ---------------------------------------------------------------- what a push states of its union paths
// The server merges a list `.gitattributes` gives `merge=union` from what the pusher says the
// list was at its base. Where the ref's own history says otherwise, the pusher is not believed.
{
  const file = (s: string) => ({ bytes: text(s), id: blobIdOf(text(s)) });
  const rules = file("list merge=union\n");
  const start = file("start\n");
  const plain = file("plain\n");
  const entry = (f: { id: Uint8Array }) => ({ mode: 0o100644, id: f.id });
  const stated = (union: [string, { id: Uint8Array } | null][], attributes: [string, { id: Uint8Array } | null][]) =>
    text(JSON.stringify({ union: union.map(([path, f]) => ({ path, base: f && { mode: 0o100644, id: hex(f.id) } })), attributes: attributes.map(([path, f]) => ({ path, entry: f && { mode: 0o100644, id: hex(f.id) } })) }));
  const pushStated = (statement: Uint8Array, c: Uint8Array, ...contents: Uint8Array[]) =>
    fetch(`${server}/${repo}/push/union?commits=1&stated=1`, { method: "POST", body: frame(c, statement, ...contents) });
  const begun = (await (await push("union", commit(null, [
    { path: text(".gitattributes"), entry: entry(rules) },
    { path: text("list"), entry: entry(start) },
    { path: text("plain"), entry: entry(plain) },
  ]), rules.bytes, start.bytes, plain.bytes)).json()) as { commit: string };
  const unionTip = async () => ((await (await fetch(`${server}/${repo}/refs/union`)).json()) as { tip: string }).tip;
  const a = file("start\na\n");
  assert.equal((await push("union", commit(begun.commit, [{ path: text("list"), entry: entry(a) }]), a.bytes)).status, 200);
  const m = file("start\nm\n");
  const mine = commit(begun.commit, [{ path: text("list"), entry: entry(m) }]);
  // The list at the base is not what the ref's history says it was.
  let said = await pushStated(stated([["list", a]], [[".gitattributes", rules]]), mine, m.bytes, a.bytes, rules.bytes);
  assert.equal(said.status, 409, "a push that lied about what a path was at its base was merged");
  // Nor are the rules.
  const other = file("* merge=union\n");
  said = await pushStated(stated([["list", start]], [[".gitattributes", other]]), mine, m.bytes, start.bytes, other.bytes);
  assert.equal(said.status, 409, "a push that lied about the rules at its base was merged");
  // A path the rules do not make union is not merged, whatever the push says of it.
  const b = file("plain\nb\n");
  assert.equal((await push("union", commit(await unionTip(), [{ path: text("plain"), entry: entry(b) }]), b.bytes)).status, 200);
  const p2 = file("plain\np\n");
  said = await pushStated(stated([["plain", plain]], [[".gitattributes", rules]]), commit(begun.commit, [{ path: text("plain"), entry: entry(p2) }]), p2.bytes, plain.bytes, rules.bytes);
  assert.equal(said.status, 409, "a path the rules do not merge by union was merged");
  // A statement that is not one: refused as a request, before anything.
  for (const bad of [text("not json"), text("{}"), text('{"union":[{"path":"list","base":{"mode":1,"id":"zz"}}],"attributes":[]}')]) {
    assert.equal((await pushStated(bad, mine, m.bytes)).status, 400, `a statement ${new TextDecoder().decode(bad)} was taken`);
  }
  // Said truly, the list is merged here, and the push lands on the tip with both entries.
  said = await pushStated(stated([["list", start]], [[".gitattributes", rules]]), mine, m.bytes, start.bytes, rules.bytes);
  const landed = (await said.json()) as { status: string; merged?: string[] };
  assert.equal(said.status, 200, JSON.stringify(landed));
  assert.deepEqual(landed.merged, ["list"]);
  // Sent again, it is told what it was told: merged, and what (thirteenth round).
  said = await pushStated(stated([["list", start]], [[".gitattributes", rules]]), mine, m.bytes, start.bytes, rules.bytes);
  const again = (await said.json()) as { status: string; merged?: string[]; commit?: string };
  assert.deepEqual([again.merged, again.commit], [["list"], (landed as { commit?: string }).commit]);
}

// ---------------------------------------------------------------- the thirteenth round
{
  // What follows a commit's tree and parents in Git's commit is its author, committer and message:
  // a `parent` or `tree` line there would give every client a Git history it cannot walk.
  const tipNow = await tip();
  const withRest = (said: Uint8Array) =>
    encode({ parents: [Uint8Array.from(tipNow.tip.match(/../g)!.map((b) => parseInt(b, 16)))], rest: said, changes: [{ path: text("r.txt"), entry: { mode: 0o100644, id: blobIdOf(good) } }] });
  for (const [what, said] of [
    ["a parent line", text(`parent ${"ab".repeat(20)}\nauthor t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nm\n`)],
    ["a parent line after the author", text(`author t <t@example.com> 1 +0000\nparent ${"ab".repeat(20)}\ncommitter t <t@example.com> 1 +0000\n\nm\n`)],
    ["a tree line", text(`author t <t@example.com> 1 +0000\ntree ${"ab".repeat(20)}\ncommitter t <t@example.com> 1 +0000\n\nm\n`)],
    ["nothing", text("")],
    ["no author", text("committer t <t@example.com> 1 +0000\n\nm\n")],
    ["a NUL in the header", text("author t <t@example.com> 1 +0000\ncommitter t\0 <t@example.com> 1 +0000\n\nm\n")],
    // And what Git's fsck refuses (second pass).
    ["no committer", text("author t <t@example.com> 1 +0000\n\nm\n")],
    ["a committer that is not one", text("author t <t@example.com> 1 +0000\ncommitter garbage\n\nm\n")],
    ["a date that is not one", text("author t <t@example.com> x +0000\ncommitter t <t@example.com> 1 +0000\n\nm\n")],
    ["a time zone that is not one", text("author t <t@example.com> 1 +00\ncommitter t <t@example.com> 1 +0000\n\nm\n")],
    ["two authors", text("author t <t@example.com> 1 +0000\nauthor t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nm\n")],
    ["no email", text("author R\ncommitter t <t@example.com> 1 +0000\n\nm\n")],
    ["no end", text("author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000")],
    // Past what a time_t holds, which Git's fsck calls an overflow (third pass).
    ["a date past a time_t", text("author t <t@example.com> 9223372036854775808 +0000\ncommitter t <t@example.com> 1 +0000\n\nm\n")],
  ] as [string, Uint8Array][]) {
    assert.equal((await push("main", withRest(said), good)).status, 400, `a commit whose header has ${what} was taken`);
  }
  assert.deepEqual(await tip(), tipNow);

  // A link is written where the link's content says it leads: nothing a file system cannot write.
  for (const [what, target] of [["5,000 bytes", text("x".repeat(5000))], ["a NUL", bytes(0x61, 0, 0x62)], ["nothing", text("")]] as [string, Uint8Array][]) {
    const refused = await push("main", commit(tipNow.tip, [{ path: text("l"), entry: { mode: 0o120000, id: blobIdOf(target) } }]), target);
    assert.equal(refused.status, 400, `a link to ${what} was taken`);
  }
  assert.deepEqual(await tip(), tipNow);

  // An empty file is a content like any other, asked for alone.
  const empty = text("");
  const landed = await push("main", commit(tipNow.tip, [{ path: text("empty.txt"), entry: { mode: 0o100644, id: blobIdOf(empty) } }]), empty);
  assert.equal(landed.status, 200, await landed.text());
  const handed = await fetch(`${server}/${repo}/fetch/objects`, { method: "POST", body: hex(blobIdOf(empty)) + "\n" });
  assert.equal(handed.status, 200, await handed.clone().text());
  assert.equal((await fetch(`${server}/${repo}/objects/${hex(blobIdOf(empty))}`)).status, 200);

  // A link below a directory is held to what a link may be when the directory is set, however its
  // content came: by itself, in a push, or from another repository (third and fourth pass). And a
  // directory holding such a link handed over does not make that content any less a file's.
  for (const [what, target] of [["5,000 bytes", text("y".repeat(5000))], ["a NUL", bytes(0x63, 0, 0x64)], ["nothing", text("")]] as [string, Uint8Array][]) {
    for (const how of ["put", "push", "adopted"]) {
      const here = `t/hostile-link-${how}-${what.length}-${stamp}`;
      const to = (path: string, init?: RequestInit) => fetch(`${server}/${here}/${path}`, init);
      const first = await to("push/main", { method: "POST", body: frame(commit(null, [{ path: text("a.txt"), entry: { mode: 0o100644, id: blobIdOf(good) } }]), good) });
      assert.equal(first.status, 200, await first.text());
      const start = ((await (await to("refs/main")).json()) as { tip: string }).tip;
      const holding = tree([0o120000, text("l"), blobIdOf(target)]);
      assert.equal((await to("adopt/trees", { method: "POST", body: frame(holding) })).status, 200);
      // The same bytes as a file of its own, as an honest writer has them.
      const file = commit(start, [{ path: text("f.bin"), entry: { mode: 0o100644, id: blobIdOf(target) } }]);
      if (how === "put") {
        assert.equal((await to(`objects/${hex(blobIdOf(target))}`, { method: "PUT", body: target })).status, 204, `${what} by itself`);
        assert.equal((await to("push/main", { method: "POST", body: frame(file) })).status, 200, `a file of ${what}, after a link to it was handed over`);
      } else if (how === "push") {
        assert.equal((await to("push/main", { method: "POST", body: frame(file, target) })).status, 200, `a file of ${what}, after a link to it was handed over`);
      } else {
        const source = `t/hostile-link-source-${what.length}-${stamp}`;
        assert.equal((await fetch(`${server}/${source}/objects/${hex(blobIdOf(target))}`, { method: "PUT", body: target })).status, 204);
        const asFile = tree([0o100644, text("x"), blobIdOf(target)]);
        assert.equal((await fetch(`${server}/${source}/adopt/trees`, { method: "POST", body: frame(asFile) })).status, 200);
        assert.equal((await to(`adopt/trees?from=/${source}`, { method: "POST", body: frame(asFile) })).status, 200);
      }
      const now = ((await (await to("refs/main")).json()) as { tip: string }).tip;
      const set = await to("push/main", { method: "POST", body: frame(commit(now, [{ path: text("v"), entry: { mode: 0o40000, id: treeId(holding) } }])) });
      assert.notEqual(set.status, 200, `a directory with a link to ${what}, its content come by ${how}, was set`);
      assert.equal(((await (await to("refs/main")).json()) as { tip: string }).tip, now);
    }
  }

  // A position is a number written as one.
  for (const bad of ["0x1", "1e0", "+1", "%201", "01", "", "-0"]) assert.equal((await fetch(`${server}/${repo}/since/main?after=${bad}`)).status, 400, `after=${bad}`);
  assert.equal((await fetch(`${server}/${repo}/since/main?after=-1`)).status, 200);
}
console.log("ok");
