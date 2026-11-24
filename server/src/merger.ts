// nv's union merge and the `.gitattributes` rule that asks for it, from `src/union.rs` of nv
// itself compiled to WebAssembly (`merger/`): what the server merges is what a sync would have.

type Compiled = {
  memory: WebAssembly.Memory;
  alloc(length: number): number;
  dealloc(at: number, length: number): void;
  answer(): number;
  answer_len(): number;
  merge_union(base: number, baseLength: number, ours: number, oursLength: number, theirs: number, theirsLength: number): number;
  is_union(path: number, pathLength: number, files: number, filesLength: number): number;
};

export type Merger = {
  /// Both sides' lines where they changed the same ones, ours first.
  mergeUnion(base: Uint8Array, ours: Uint8Array, theirs: Uint8Array): Uint8Array;
  /// Whether `.gitattributes` gives `path` the union merge: `files` maps each directory that
  /// has one (`""` for the top) to its content.
  isUnion(path: string, files: Map<string, Uint8Array>): boolean;
};

const encoder = new TextEncoder();

export function merging(module: WebAssembly.Module): Merger {
  const wasm = new WebAssembly.Instance(module).exports as unknown as Compiled;
  // Memory grows as the merge allocates: a view is taken afresh each time it is used.
  const into = (bytes: Uint8Array): [number, number] => {
    const at = wasm.alloc(bytes.length);
    new Uint8Array(wasm.memory.buffer, at, bytes.length).set(bytes);
    return [at, bytes.length];
  };
  const answer = () => new Uint8Array(wasm.memory.buffer, wasm.answer(), wasm.answer_len()).slice();
  return {
    mergeUnion(base, ours, theirs) {
      const args = [into(base), into(ours), into(theirs)];
      try {
        wasm.merge_union(...(args.flat() as [number, number, number, number, number, number]));
        return answer();
      } finally {
        for (const [at, length] of args) wasm.dealloc(at, length);
      }
    },
    isUnion(path, files) {
      const frames: Uint8Array[] = [];
      for (const [dir, bytes] of files) frames.push(encoder.encode(dir), bytes);
      const length = frames.reduce((n, f) => n + 4 + f.length, 0);
      const framed = new Uint8Array(length);
      let at = 0;
      for (const frame of frames) {
        new DataView(framed.buffer).setUint32(at, frame.length, true);
        framed.set(frame, at + 4);
        at += 4 + frame.length;
      }
      const args = [into(encoder.encode(path)), into(framed)];
      try {
        const said = wasm.is_union(...(args.flat() as [number, number, number, number]));
        if (said === 2) throw new Error("the attribute files were not framed");
        return said === 1;
      } finally {
        for (const [at2, length2] of args) wasm.dealloc(at2, length2);
      }
    },
  };
}
