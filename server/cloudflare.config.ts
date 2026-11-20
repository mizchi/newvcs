import { bindings, defineConfig, exports } from "cf/config";
import * as entrypoint from "./src/index.ts" with { type: "cf-worker" };

const name = "newvcs";

export default defineConfig({
  worker: {
    name,
    entrypoint,
    compatibilityDate: "2026-11-16",
    exports: { Repo: exports.durableObject({ storage: "sqlite" }) },
    env: {
      REPO: bindings.durableObject({ worker: name, exportName: "Repo" }),
      // File contents: immutable, named by their hash, written by anyone at any time.
      OBJECTS: bindings.r2({ name: "newvcs-objects" }),
    },
  },
});
