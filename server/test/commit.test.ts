import assert from "node:assert/strict";
import { test } from "node:test";
import { conflicts, decode, encode, hex, idOf, pathsConflict, type Commit } from "../src/commit.ts";

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
