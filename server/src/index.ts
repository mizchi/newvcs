// Routes to the repository's Durable Object:
//
//   POST /<owner>/<repo>/push/<ref>?after=N&commits=M
//                                     body: frames, M commits (one if not said) each on the one
//                                     before, then the file contents they name
//                                     -> PushResult, with the commits since N if asked
//   GET  /<owner>/<repo>/since/<ref>?after=N  -> { tip, seq, since: the commits after N } | 404
//   POST /<owner>/<repo>/locate/<ref>         body: commit ids, 32 bytes each, newest first
//                                     -> the first one the ref's history holds, and its position | 404
//   GET  /<owner>/<repo>/refs/<ref>           -> { tip, seq } | 404
//   GET  /<owner>/<repo>/log/<ref>?after=N    -> ids of the commits since
//   GET  /<owner>/<repo>/divergent/<ref>      -> pushes kept aside
//   GET  /<owner>/<repo>/commits/<id>         -> the encoded commit
//
// and to the bucket of file contents, which needs no agreement at all:
//
//   PUT  /<owner>/<repo>/objects/<id>         body: the file's bytes -> 204, or 400 if they do not hash to <id>
//   GET  /<owner>/<repo>/objects/<id>         -> the bytes | 404
//   POST /<owner>/<repo>/fetch/objects        body: ids in hex, one per line
//                                     -> frames: { direct: ids to GET one by one }, then id and bytes in turn
//   POST /<owner>/<repo>/adopt/trees?from=/<owner>/<repo>
//                                     body: frames, tree objects: a directory and those below it
//                                     -> { adopted, missing }: the files below taken over from the
//                                        repository `from`, and the ids of those still to be sent
//   POST /<owner>/<repo>/fetch/trees  body: ids of directories -> frames: they and every directory below
//   GET  /<owner>/<repo>/storage/now          -> how many contents the repository has, in how many objects
//
// A frame is a little-endian u32 length and that many bytes. One push is one
// request: contents first into the bucket, then the commit to the object
// that orders the ref, so a ref never names contents the bucket lacks.
//
// The bucket has two kinds of object, both named by what is in them and so
// written once and never changed:
//
//   packs/<hash>    the contents that came with one push, end to end. A push
//                   of two hundred files is one write to the bucket, not two
//                   hundred; where each file is in the pack is a row in the
//                   repository's own table.
//   objects/<id>    one content by itself: what a writer chose to send
//                   outside a push because it is large. It is streamed into
//                   the bucket and hashed on the way, never held whole.
//
// Names are not per repository, so the same content is stored once for all
// of them. What a repository may read is what its table says it was given:
// knowing an id is not having the content.

import { hex, unhex } from "./commit.ts";
import { blobHasher, blobId, hash, objectId } from "./hash.ts";
import { Repo, type Placed, type Step } from "./repo.ts";
import { decodeTree, KIND_TREE, MODE_DIR, MODE_GITLINK } from "./tree.ts";

export { Repo };

const json = (value: unknown, status = 200) => Response.json(value, { status });

function frames(bytes: Uint8Array): Uint8Array[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out: Uint8Array[] = [];
  for (let at = 0; at < bytes.length; ) {
    if (at + 4 > bytes.length) throw new Error("a frame is cut short");
    const length = view.getUint32(at, true);
    if (at + 4 + length > bytes.length) throw new Error("a frame is cut short");
    out.push(bytes.subarray(at + 4, at + 4 + length));
    at += 4 + length;
  }
  return out;
}

function framed(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + 4 + p.length, 0));
  const view = new DataView(out.buffer);
  let at = 0;
  for (const part of parts) {
    view.setUint32(at, part.length, true);
    out.set(part, at + 4);
    at += 4 + part.length;
  }
  return out;
}

/// Contents end to end as one object for the bucket, and where each begins in it.
function packed(contents: Uint8Array[]): [Uint8Array, number[]] {
  const pack = new Uint8Array(contents.reduce((n, bytes) => n + bytes.length, 0));
  const at: number[] = [];
  let end = 0;
  for (const bytes of contents) {
    at.push(end);
    pack.set(bytes, end);
    end += bytes.length;
  }
  return [pack, at];
}

const steps = (since: Step[]) => since.map((s) => ({ seq: s.seq, id: s.id, bytes: hex(s.bytes) }));

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const [owner, repository, what, ...rest] = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
    const arg = rest.join("/");
    if (!owner || !repository || !what || !arg) return json({ error: "expected /<owner>/<owner>/<repo>/<operation>/<name>" }, 404);
    const name = `${owner}/${repository}`;
    // The generated binding type does not name the class behind it.
    const repos = env.REPO as unknown as DurableObjectNamespace<Repo>;
    const repo = repos.get(repos.idFromName(name));
    const after = url.searchParams.has("after") ? Number(url.searchParams.get("after")) : null;
    const body = async () => new Uint8Array(await request.arrayBuffer());
    try {
      if (request.method === "POST") {
        if (what === "push") {
          // The commits of the push, oldest first, then the contents they name.
          const all = frames(await body());
          const count = Number(url.searchParams.get("commits") ?? 1);
          if (!Number.isInteger(count) || count < 1 || all.length < count) return json({ error: "a push carries a commit" }, 400);
          // Their own bytes: a view into the request would take the whole request along to the object.
          const commits = all.slice(0, count).map((bytes) => bytes.slice());
          const contents = all.slice(count);
          // A name is a promise about the content, so the name is worked out here, not taken.
          // All of them go into the bucket as one object.
          const placed: Placed[] = [];
          if (contents.length > 0) {
            const [pack, at] = packed(contents);
            const packId = hash(pack);
            contents.forEach((bytes, i) => placed.push({ id: blobId(bytes), pack: packId, at: at[i]!, length: bytes.length }));
            await env.OBJECTS.put(`packs/${hex(packId)}`, pack);
          }
          const result = await repo.push(arg, commits, after, placed);
          if (result.status === "conflict" || result.status === "unknown-base") return json(result, 409);
          return json({ ...result, since: steps(result.since) });
        }
        if (what === "locate") {
          const ids = await body();
          const found = await repo.locate(arg, Array.from({ length: Math.floor(ids.length / 32) }, (_, i) => ids.slice(i * 32, i * 32 + 32)));
          return found ? json(found) : json({ error: "none of these commits is in the ref's history" }, 404);
        }
        if (what === "adopt") {
          // Directories, as tree objects. The repository is handed them, and with them every
          // file below that it has already or that the repository `from` has: those change
          // hands as rows of a table, and no content moves. What neither has is named in the
          // answer, for the writer to send.
          const trees = frames(await body()).map((bytes) => ({ id: objectId(KIND_TREE, bytes), bytes, entries: decodeTree(bytes) }));
          const given = new Set(trees.map((t) => hex(t.id)));
          const below = (dirs: boolean) => {
            const ids = new Map<string, Uint8Array>();
            for (const t of trees) for (const e of t.entries) if (e.mode !== MODE_GITLINK && (e.mode === MODE_DIR) === dirs) ids.set(hex(e.id), e.id);
            return ids;
          };
          const has = async (ids: Uint8Array[]) => new Set((await repo.where(ids)).map((c) => hex(c.id)));
          const outside = [...below(true)].filter(([id]) => !given.has(id)).map(([, id]) => id);
          if ((await has(outside)).size !== outside.length) return json({ error: "a directory below these was not handed over" }, 400);
          const files = [...below(false).values()];
          const had = await has(files);
          let missing = files.filter((id) => !had.has(hex(id)));
          let adopted = 0;
          const from = url.searchParams.get("from")?.match(/^\/([^/]+\/[^/]+)$/)?.[1];
          if (from && missing.length > 0) {
            const theirs = await repos.get(repos.idFromName(from)).where(missing);
            await repo.place(theirs);
            adopted = theirs.length;
            const taken = new Set(theirs.map((c) => hex(c.id)));
            missing = missing.filter((id) => !taken.has(hex(id)));
          }
          const known = await has(trees.map((t) => t.id));
          const fresh = trees.filter((t) => !known.has(hex(t.id)));
          if (fresh.length > 0) {
            const [pack, at] = packed(fresh.map((t) => t.bytes));
            const packId = hash(pack);
            await env.OBJECTS.put(`packs/${hex(packId)}`, pack);
            await repo.place(fresh.map((t, i) => ({ id: t.id, pack: packId, at: at[i]!, length: t.bytes.length })));
          }
          return json({ adopted, missing: missing.map(hex) });
        }
        if (what === "fetch" && arg === "trees") {
          // These directories and every directory below them, each read once.
          const packs = new Map<string, Uint8Array>();
          const out: Uint8Array[] = [];
          const seen = new Set<string>();
          let level = (await request.text()).split("\n").filter(Boolean).map(unhex);
          while (level.length > 0) {
            const found = await repo.where(level);
            if (found.length !== level.length) return json({ error: "no such directory" }, 404);
            const next = new Map<string, Uint8Array>();
            for (const c of found) {
              if (!c.pack) throw new Error("a directory is not in a pack");
              const key = hex(c.pack);
              if (!packs.has(key)) {
                const object = await env.OBJECTS.get(`packs/${key}`);
                if (!object) throw new Error(`pack ${key} is not in the bucket`);
                packs.set(key, new Uint8Array(await object.arrayBuffer()));
              }
              const bytes = packs.get(key)!.subarray(c.at, c.at + c.length);
              out.push(bytes);
              seen.add(hex(c.id));
              for (const e of decodeTree(bytes)) if (e.mode === MODE_DIR && !seen.has(hex(e.id))) next.set(hex(e.id), e.id);
            }
            level = [...next.values()].filter((id) => !seen.has(hex(id)));
          }
          return new Response(framed(out));
        }
        if (what === "fetch") {
          const ids = (await request.text()).split("\n").filter(Boolean).map(unhex);
          const found = await repo.where(ids);
          // By pack: one read of the bucket for each, and of only the bytes wanted when it is one content.
          const packs = new Map<string, Placed[]>();
          for (const c of found) if (c.pack) packs.set(hex(c.pack), [...(packs.get(hex(c.pack)) ?? []), c]);
          const read = await Promise.all(
            [...packs].map(async ([pack, wanted]) => {
              const only = wanted.length === 1 ? wanted[0]! : null;
              const object = await env.OBJECTS.get(`packs/${pack}`, only ? { range: { offset: only.at, length: only.length } } : {});
              if (!object) throw new Error(`pack ${pack} is not in the bucket`);
              const bytes = new Uint8Array(await object.arrayBuffer());
              return wanted.flatMap((c) => [c.id, only ? bytes : bytes.subarray(c.at, c.at + c.length)]);
            }),
          );
          const direct = found.filter((c) => !c.pack).map((c) => hex(c.id));
          return new Response(framed([new TextEncoder().encode(JSON.stringify({ direct })), ...read.flat()]));
        }
        return json({ error: `unknown operation ${what}` }, 404);
      }
      if (what === "objects") {
        const id = unhex(arg);
        const key = `objects/${hex(id)}`;
        if (request.method === "PUT") {
          const length = Number(request.headers.get("content-length"));
          if (!request.body || !Number.isFinite(length)) return json({ error: "an upload says how long it is" }, 411);
          // Into the bucket as it arrives, hashed on the way: the content is never held whole.
          const hasher = blobHasher();
          const hashing = new TransformStream<Uint8Array, Uint8Array>({
            transform(chunk, controller) {
              hasher.update(chunk);
              controller.enqueue(chunk);
            },
          });
          await env.OBJECTS.put(key, request.body.pipeThrough(hashing).pipeThrough(new FixedLengthStream(length)));
          if (hex(hasher.digest()) !== arg) {
            // Nobody was told it was there: a repository reads only what its table lists.
            await env.OBJECTS.delete(key);
            return json({ error: "the bytes do not hash to that id" }, 400);
          }
          await repo.place([{ id, pack: null, at: 0, length }]);
          return new Response(null, { status: 204 });
        }
        const [placed] = await repo.where([id]);
        if (!placed) return json({ error: "no such object" }, 404);
        const object = placed.pack
          ? await env.OBJECTS.get(`packs/${hex(placed.pack)}`, { range: { offset: placed.at, length: placed.length } })
          : await env.OBJECTS.get(key);
        return object ? new Response(object.body) : json({ error: "no such object" }, 404);
      }
      if (request.method !== "GET") return json({ error: "method not allowed" }, 405);
      if (what === "since") {
        const result = await repo.since(arg, after ?? 0);
        return result ? json({ ...result, since: steps(result.since) }) : json({ error: "no such ref" }, 404);
      }
      if (what === "refs") {
        const head = await repo.head(arg);
        return head ? json(head) : json({ error: "no such ref" }, 404);
      }
      if (what === "log") return json(await repo.log(arg, after ?? 0));
      if (what === "storage") return json(await repo.storage());
      if (what === "divergent") return json(await repo.divergent(arg));
      if (what === "commits") {
        const bytes = await repo.commit(unhex(arg));
        return bytes ? new Response(bytes) : json({ error: "no such commit" }, 404);
      }
      return json({ error: `unknown operation ${what}` }, 404);
    } catch (error) {
      return json({ error: String(error instanceof Error ? error.message : error) }, 400);
    }
  },
} satisfies ExportedHandler<Env>;
