// The compiled BLAKE3 has to give the ids the JavaScript one gives, however the bytes arrive.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { blake3 } from "@noble/hashes/blake3.js";
import { hashing } from "../src/blake3.ts";
import { blobHasher, blobIdOf, hex } from "../src/commit.ts";

// The Worker's types say a module cannot be made from bytes, which is true there and not in node.
const Module = WebAssembly.Module as unknown as new (bytes: Uint8Array) => WebAssembly.Module;
const compiled = hashing(new Module(readFileSync(join(import.meta.dirname, "../src/blake3.wasm"))));
const bytesOf = (length: number, seed: number) => Uint8Array.from({ length }, (_, i) => (i * 31 + seed) % 251);

test("the same hash as the JavaScript one, at every size around a block and a chunk", () => {
  for (const length of [0, 1, 63, 64, 65, 1023, 1024, 1025, 65535, 65536, 65537, 200_000, 3_000_001]) {
    const bytes = bytesOf(length, length);
    assert.equal(hex(compiled.hash(bytes)), hex(blake3(bytes)), `${length} bytes`);
  }
});

test("a blob's id, whole and in pieces of any size", () => {
  const bytes = bytesOf(1_500_000, 7);
  assert.equal(hex(compiled.blobId(bytes)), hex(blobIdOf(bytes)));
  for (const piece of [1, 7, 4096, 65536, 100_000, 1_000_000]) {
    const hasher = compiled.blobHasher();
    for (let at = 0; at < bytes.length; at += piece) hasher.update(bytes.subarray(at, at + piece));
    assert.equal(hex(hasher.digest()), hex(blobIdOf(bytes)), `pieces of ${piece}`);
  }
});

test("two contents hashed at the same time do not mix", () => {
  const a = bytesOf(300_000, 1);
  const b = bytesOf(300_000, 2);
  const [ha, hb] = [compiled.blobHasher(), compiled.blobHasher()];
  for (let at = 0; at < a.length; at += 5000) {
    ha.update(a.subarray(at, at + 5000));
    hb.update(b.subarray(at, at + 5000));
  }
  assert.equal(hex(ha.digest()), hex(blobIdOf(a)));
  assert.equal(hex(hb.digest()), hex(blobIdOf(b)));
});

test("a hasher that is dropped leaves room for the next: memory does not grow with use", () => {
  const before = compiled.memoryBytes();
  for (let i = 0; i < 10_000; i++) {
    const hasher = compiled.blobHasher();
    hasher.update(bytesOf(100, i));
    hasher.digest();
  }
  assert.equal(compiled.memoryBytes(), before);
  // The reference stays the same as the JavaScript streaming one.
  const js = blobHasher();
  js.update(bytesOf(100, 3));
  const wasm = compiled.blobHasher();
  wasm.update(bytesOf(100, 3));
  assert.equal(hex(wasm.digest()), hex(js.digest()));
});
