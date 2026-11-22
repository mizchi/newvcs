// The compiled merge is nv's own: the answers its unit tests in `src/union.rs` expect.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { merging } from "../src/merger.ts";

const Module = WebAssembly.Module as unknown as new (bytes: Uint8Array) => WebAssembly.Module;
const merger = merging(new Module(readFileSync(join(import.meta.dirname, "../src/merger.wasm"))));
const text = (s: string) => new TextEncoder().encode(s);

test("a union keeps both sides, ours first", () => {
  assert.equal(new TextDecoder().decode(merger.mergeUnion(text("start\n"), text("start\nbob\n"), text("start\nann\n"))), "start\nbob\nann\n");
  // Large enough to grow the memory more than once.
  const big = Array.from({ length: 200_000 }, (_, i) => `line ${i}\n`).join("");
  const merged = merger.mergeUnion(text(big), text(big + "ours\n"), text(big + "theirs\n"));
  assert.equal(new TextDecoder().decode(merged.subarray(merged.length - 12)), "ours\ntheirs\n");
});

test("the nearest rule that speaks of merge decides", () => {
  const files = new Map([
    ["", text("*.txt merge=union\nlist merge=union\n")],
    ["sub", text("a.txt -merge\nb.txt text\n")],
  ]);
  assert.equal(merger.isUnion("list", files), true);
  assert.equal(merger.isUnion("x.txt", files), true);
  assert.equal(merger.isUnion("sub/a.txt", files), false);
  assert.equal(merger.isUnion("sub/b.txt", files), true);
  assert.equal(merger.isUnion("plain", files), false);
});
