// Routes to the repository's Durable Object:
//
//   POST /<repo>/push/<ref>?after=N   body: frames, the commit then the file contents it names
//                                     -> PushResult, with the commits since N if asked
//   GET  /<repo>/since/<ref>?after=N  -> { tip, seq, since: the commits after N } | 404
//   POST /<repo>/locate/<ref>         body: commit ids, 32 bytes each, newest first
//                                     -> the first one the ref's history holds, and its position | 404
//   GET  /<repo>/refs/<ref>           -> { tip, seq } | 404
//   GET  /<repo>/log/<ref>?after=N    -> ids of the commits since
//   GET  /<repo>/divergent/<ref>      -> pushes kept aside
//   GET  /<repo>/commits/<id>         -> the encoded commit
//
// and to the bucket of file contents, which needs no agreement at all:
//
//   PUT  /<repo>/objects/<id>         body: the file's bytes -> 204, or 400 if they do not hash to <id>
//   GET  /<repo>/objects/<id>         -> the bytes | 404
//   POST /<repo>/fetch/objects        body: ids in hex, one per line -> frames of id followed by bytes
//
// A frame is a little-endian u32 length and that many bytes. One push is one
// request: contents first into the bucket, then the commit to the object
// that orders the ref, so a ref never names contents the bucket lacks.

import { blobIdOf, hex, unhex } from "./commit.ts";
import { Repo, type Step } from "./repo.ts";

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

const steps = (since: Step[]) => since.map((s) => ({ seq: s.seq, id: s.id, bytes: hex(s.bytes) }));

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const [name, what, ...rest] = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
    const arg = rest.join("/");
    if (!name || !what || !arg) return json({ error: "expected /<repo>/<operation>/<name>" }, 404);
    // The generated binding type does not name the class behind it.
    const repos = env.REPO as unknown as DurableObjectNamespace<Repo>;
    const repo = repos.get(repos.idFromName(name));
    const after = url.searchParams.has("after") ? Number(url.searchParams.get("after")) : null;
    const body = async () => new Uint8Array(await request.arrayBuffer());
    try {
      if (request.method === "POST") {
        if (what === "push") {
          const [commit, ...contents] = frames(await body());
          if (!commit) return json({ error: "a push carries a commit" }, 400);
          // A name is a promise about the content, so the name is worked out here, not taken.
          await Promise.all(contents.map((bytes) => env.OBJECTS.put(`${name}/${hex(blobIdOf(bytes))}`, bytes)));
          const result = await repo.push(arg, commit, after);
          if (result.status === "conflict" || result.status === "unknown-base") return json(result, 409);
          return json({ ...result, since: steps(result.since) });
        }
        if (what === "locate") {
          const ids = await body();
          const found = await repo.locate(arg, Array.from({ length: Math.floor(ids.length / 32) }, (_, i) => ids.slice(i * 32, i * 32 + 32)));
          return found ? json(found) : json({ error: "none of these commits is in the ref's history" }, 404);
        }
        if (what === "fetch") {
          const ids = (await request.text()).split("\n").filter(Boolean).map(unhex);
          const found = await Promise.all(ids.map(async (id) => ({ id, object: await env.OBJECTS.get(`${name}/${hex(id)}`) })));
          const parts: Uint8Array[] = [];
          for (const { id, object } of found) if (object) parts.push(id, new Uint8Array(await object.arrayBuffer()));
          return new Response(framed(parts));
        }
        return json({ error: `unknown operation ${what}` }, 404);
      }
      if (what === "objects") {
        const key = `${name}/${hex(unhex(arg))}`;
        if (request.method === "PUT") {
          const bytes = await body();
          if (hex(blobIdOf(bytes)) !== arg) return json({ error: "the bytes do not hash to that id" }, 400);
          await env.OBJECTS.put(key, bytes);
          return new Response(null, { status: 204 });
        }
        const object = await env.OBJECTS.get(key);
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
