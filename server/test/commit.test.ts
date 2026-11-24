import assert from "node:assert/strict";
import { test } from "node:test";
import { conflicts, decode, encode, folded, hex, idOf, nameOk, pathOk, pathsConflict, type Commit } from "../src/commit.ts";

const text = (s: string) => new TextEncoder().encode(s);

// The commit of `the_id_of_a_commit_is_pinned_for_the_server` in src/commit.rs.
const pinned: Commit = {
  parents: [new Uint8Array(32).fill(0x11)],
  rest: text("author a <a@example.com> 1 +0000\ncommitter a <a@example.com> 1 +0000\n\nmessage\n"),
  changes: [
    { path: text("dir/file.txt"), entry: { mode: 0o100644, id: new Uint8Array(32).fill(0x22) } },
    { path: text("gone"), entry: null },
  ],
};

test("a commit has the id the Rust side gives it", () => {
  assert.equal(hex(idOf(encode(pinned))), "22d0e51d694ac215a59f6b3aeaddcab4bf0b93a02f93d37d2c44d36d808093ae");
});

test("a commit survives encoding, and damaged bytes are refused", () => {
  const bytes = encode(pinned);
  assert.deepEqual(decode(bytes), pinned);
  assert.throws(() => decode(bytes.subarray(0, 20)));
  assert.throws(() => decode(Uint8Array.of(...bytes, 0)));
  assert.throws(() => decode(Uint8Array.of(0xff, 0xff, 0xff, 0xff)));
});

test("a commit that sets a file and something below it, or spells a change two ways, is refused", () => {
  // Twelfth round: `l` a link and `l/x` a file in one commit were taken and served.
  const set = (path: string, mode: number) => ({ path: text(path), entry: { mode, id: new Uint8Array(32).fill(1) } });
  const commit = (changes: Commit["changes"]): Commit => ({ parents: [], rest: text("r"), changes });
  for (const [first, mode] of [["l", 0o120000], ["a", 0o100644], ["s", 0o160000]] as const) {
    assert.throws(() => decode(encode(commit([set(first, mode), set(`${first}/x`, 0o100644)]))), /below/);
  }
  assert.doesNotThrow(() => decode(encode(commit([set("a", 0o100644), { path: text("a/x"), entry: null }]))));
  assert.doesNotThrow(() => decode(encode(commit([{ path: text("a"), entry: null }, set("a/x", 0o100644)]))));
  assert.doesNotThrow(() => decode(encode(commit([set("a", 0o100644), set("ab/x", 0o100644)]))));
  const bytes = encode(commit([set("a", 0o100644)]));
  const kind = bytes.length - 32 - 4 - 1;
  assert.equal(bytes[kind], 1);
  bytes[kind] = 2;
  assert.throws(() => decode(bytes), /kind/);
});

test("names Windows takes for others are refused, or folded together", () => {
  // Twelfth round, the same as `manifest::tests::names_windows_takes_for_others_are_refused_or_folded`.
  for (const name of [".git::$INDEX_ALLOCATION", ".nv::$INDEX_ALLOCATION", ".git:x", "nv~1:x", ".GIT :stream", ".git\\config", ".nv\\x", "..\\..\\x", "\\abs", "a\\..", "..:x", ".. "]) {
    assert.ok(!pathOk(text(name)), name);
  }
  for (const name of ["a:b", "a\\b", "...", "x.git", "git"]) assert.ok(pathOk(text(name)), name);
  // A tree's entries as strictly as a commit's paths: a directory set whole is written where its names say.
  for (const name of [".git:x", ".nv::$INDEX_ALLOCATION", "..\\x"]) assert.ok(!nameOk(text(name)), name);
  for (const name of ["a:b", "a\\b"]) assert.ok(nameOk(text(name)), name);
  for (const [one, other] of [["a", "a::$DATA"], ["a", "a."], ["a", "a "], ["a", "A."], ["a", "a:b"], ["d/a", "d/a."]]) {
    assert.equal(folded(one!), folded(other!), `${one} and ${other}`);
  }
});

test("moving a commit to another parent changes its id and nothing else", () => {
  const moved = { ...pinned, parents: [new Uint8Array(32).fill(0x33)] };
  assert.notEqual(hex(idOf(encode(moved))), hex(idOf(encode(pinned))));
  assert.deepEqual(decode(encode(moved)).changes, pinned.changes);
});

test("paths conflict when equal or nested, not when merely alike", () => {
  assert.ok(pathsConflict("a/b", "a/b"));
  assert.ok(pathsConflict("a", "a/b") && pathsConflict("a/b/c", "a/b"));
  assert.ok(!pathsConflict("a/b", "a/bc") && !pathsConflict("a.b", "a/b"));
  const theirs = ["src/x.ts", "docs", "pkg/core/deep/file"];
  assert.deepEqual(conflicts(["src/x.ts", "src/y.ts", "docs/guide.md", "pkg/core", "pkg/corex", "do"], theirs), ["src/x.ts", "docs/guide.md", "pkg/core"]);
  // Agrees with the pairwise rule on every pair.
  const mine = ["a", "a/b", "a/b/c", "ab", "b/a", "a.b"];
  for (const t of mine) assert.deepEqual(conflicts(mine, [t]), mine.filter((m) => pathsConflict(m, t)), t);
});
