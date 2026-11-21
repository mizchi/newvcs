// The Worker's hashing: the compiled BLAKE3, made ready once for the isolate.
import compiled from "./blake3.wasm";
import { hashing } from "./blake3.ts";

export const { hash, objectId, blobHasher, blobId } = hashing(compiled);
