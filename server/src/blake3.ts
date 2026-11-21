// BLAKE3 from the crate nv itself hashes with, compiled to WebAssembly (`hasher/`).
// The JavaScript one in `commit.ts` gives the same answers at a twentieth of the speed,
// which is fine for a commit and not for a file.

type Compiled = {
  memory: WebAssembly.Memory;
  input(): number;
  input_size(): number;
  state(): number;
  state_size(): number;
  init(): void;
  update(length: number): void;
  finish(): number;
};

export type Hasher = { update(bytes: Uint8Array): void; digest(): Uint8Array };

const KIND_BLOB = 1;

export function hashing(module: WebAssembly.Module) {
  const wasm = new WebAssembly.Instance(module).exports as unknown as Compiled;
  const [input, inputSize, state, stateSize] = [wasm.input(), wasm.input_size(), wasm.state(), wasm.state_size()];
  // The memory never grows (nothing in there allocates), so one view of it stays good.
  const memory = new Uint8Array(wasm.memory.buffer);

  // The compiled side holds one hasher. Whoever used it last is still in there; anyone
  // else's state is kept here, and goes back in when they come to hash again.
  let inside: object | null = null;
  let putAway = () => {};

  const create = (): Hasher => {
    const self = {};
    let kept: Uint8Array | null = null;
    const enter = () => {
      if (inside === self) return;
      putAway();
      if (kept) memory.set(kept, state);
      else wasm.init();
      inside = self;
      putAway = () => {
        kept ??= new Uint8Array(stateSize);
        kept.set(memory.subarray(state, state + stateSize));
      };
    };
    return {
      update(bytes) {
        enter();
        for (let at = 0; at < bytes.length; at += inputSize) {
          const piece = bytes.subarray(at, at + inputSize);
          memory.set(piece, input);
          wasm.update(piece.length);
        }
      },
      digest() {
        enter();
        const out = wasm.finish();
        return memory.slice(out, out + 32);
      },
    };
  };

  const hash = (bytes: Uint8Array) => {
    const hasher = create();
    hasher.update(bytes);
    return hasher.digest();
  };
  /// An object's id as its bytes go by: BLAKE3 over its kind and then the bytes.
  const hasherOf = (kind: number) => {
    const hasher = create();
    hasher.update(Uint8Array.of(kind));
    return hasher;
  };
  const objectId = (kind: number, bytes: Uint8Array) => {
    const hasher = hasherOf(kind);
    hasher.update(bytes);
    return hasher.digest();
  };
  const blobHasher = () => hasherOf(KIND_BLOB);
  const blobId = (bytes: Uint8Array) => objectId(KIND_BLOB, bytes);
  return { hash, objectId, blobHasher, blobId, memoryBytes: () => wasm.memory.buffer.byteLength };
}
