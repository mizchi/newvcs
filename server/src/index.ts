// Routes to the repository's Durable Object:
//
//   POST /<owner>/<repo>/push/<ref>?after=N&commits=M[&stated=1]
//                                     body: frames, M commits (one if not said) each on the one
//                                     before, then, with `stated`, what the pusher says of the
//                                     paths it changes that merge by union (JSON, `Attested`),
//                                     then the file contents they name
//                                     -> PushResult, with the commits since N if asked. A push
//                                        that clashes only on such paths is merged here, as
//                                        `nv sync` would merge it, and lands; the answer names them
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

import { decode, encode, hex, idOf, linkOk, MAX_PATH, MODE_LINK, refOk, unhex } from "./commit.ts";
import { blobHasher, blobId, hash, objectId } from "./hash.ts";
import { merging } from "./merger.ts";
import compiledMerger from "./merger.wasm";
import { type Attested, type Landed, Repo, type Placed, type Stated, type Step } from "./repo.ts";
import { decodeTree, KIND_TREE, MODE_DIR, MODE_GITLINK } from "./tree.ts";

/// The most files a directory handed over may hold, all below it counted: more than any client
/// would write, and few enough that one could (2^22).
const MAX_FILES = 4_194_304;

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

const merger = merging(compiledMerger);

/// What a pusher states of its union paths, if it is well formed: nothing in it is believed yet.
function statement(bytes: Uint8Array): Attested | null {
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes));
  } catch {
    return null;
  }
  const entry = (e: unknown): e is Stated | null =>
    e === null || (typeof e === "object" && typeof (e as Stated).mode === "number" && typeof (e as Stated).id === "string" && /^[0-9a-f]{64}$/.test((e as Stated).id));
  const v = value as Attested;
  const ok =
    typeof v === "object" && v !== null && Array.isArray(v.union) && Array.isArray(v.attributes) && v.union.length <= 1000 && v.attributes.length <= 10_000 &&
    v.union.every((u) => typeof u?.path === "string" && entry(u.base)) &&
    v.attributes.every((a) => typeof a?.path === "string" && (a.path === ".gitattributes" || a.path.endsWith("/.gitattributes")) && entry(a.entry));
  return ok ? { union: v.union.map((u) => ({ path: u.path, base: u.base })), attributes: v.attributes.map((a) => ({ path: a.path, entry: a.entry })) } : null;
}

/// How large a file the server merges by union, and how often it makes a merge again where the
/// ref moved while it was made.
const MERGE_LIMIT = 256 * 1024;
const MERGE_ATTEMPTS = 16;
const MERGE_PUSH_LIMIT = 1024 * 1024;

/// The contents of these placed ids, from the bucket.
async function read(env: Env, placed: Placed[]): Promise<Map<string, Uint8Array>> {
  const out = new Map<string, Uint8Array>();
  await Promise.all(
    placed.map(async (c) => {
      // Nothing to read of an empty content: a range of no bytes is one the bucket refuses.
      if (c.length === 0) return void out.set(hex(c.id), new Uint8Array());
      const object = c.pack
        ? await env.OBJECTS.get(`packs/${hex(c.pack)}`, { range: { offset: c.at, length: c.length } })
        : await env.OBJECTS.get(`objects/${hex(c.id)}`);
      if (!object) throw new Error(`content ${hex(c.id)} is not in the bucket`);
      out.set(hex(c.id), new Uint8Array(await object.arrayBuffer()));
    }),
  );
  return out;
}

/// A push refused for clashing only on paths `.gitattributes` merges by union, merged here as a
/// sync would merge it: each such path of each commit of the chain, on what the one before it
/// became, the first on the ref's tip; and landed. `null` where it cannot be: the push stays
/// refused, and its writer syncs. Made again if the ref moved meanwhile: as often as other writers'
/// pushes landing at the same moment take for it to land, within a bound (thirteenth round: with a
/// dozen at once, three tries refused half of them).
async function mergeOnServer(env: Env, repo: DurableObjectStub<Repo>, ref: string, chain: Uint8Array[], attested: Attested, after: number | null): Promise<Landed | null> {
  for (let attempt = 0; attempt < MERGE_ATTEMPTS; attempt++) {
    if (attempt > 0) await new Promise((done) => setTimeout(done, Math.random() * 5 * attempt));
    const plan = await repo.unionPlan(ref, chain, attested);
    if ("why" in plan) return null;
    // A merge here is the list a sync would make, and costs the server what it costs: a file too
    // large to merge in a moment is the writer's to merge, by a sync (thirteenth round: one push of
    // 200,000 reordered lines held the server for seconds).
    if (plan.contents.some((c) => c.length > MERGE_LIMIT)) return null;
    // And the push as a whole: so many files and commits, each under the bound, held the server
    // for seconds as one file did (second pass).
    const size = new Map(plan.contents.map((c) => [hex(c.id), c.length]));
    const largest = (p: (typeof plan.paths)[number]) => Math.max(...[p.base, p.theirs, ...p.chain].map((id) => (id === null ? 0 : (size.get(id) ?? 0))));
    if (plan.paths.reduce((n, p) => n + p.chain.length * largest(p), 0) > MERGE_PUSH_LIMIT) return null;
    const bytes = await read(env, plan.contents);
    const of = (id: string) => bytes.get(id)!;
    const rules = new Map(plan.attributes.map((a) => [a.dir, of(a.id)]));
    // The ref's rules, as the pusher started from them, say union for every one of them; and
    // they are text.
    if (!plan.paths.every((p) => merger.isUnion(p.path, rules))) return null;
    if (plan.paths.some((p) => [p.base, p.theirs, ...p.chain].some((id) => id !== null && of(id).includes(0)))) return null;
    const commits = chain.map(decode);
    const made: Uint8Array[] = [];
    const contents: Uint8Array[] = [];
    // For each path, what it was before the commit being made again, as sent and as made.
    const sentBefore = new Map(plan.paths.map((p) => [p.path, of(p.base)]));
    const madeBefore = new Map(plan.paths.map((p) => [p.path, of(p.theirs)]));
    let previous = unhex(plan.tip);
    for (const [i, commit] of commits.entries()) {
      const changes = commit.changes.map((c) => {
        const path = new TextDecoder().decode(c.path);
        const p = plan.paths.find((x) => x.path === path);
        if (!p || !c.entry) return c;
        const ours = of(p.chain[i]!);
        const mergedBytes = merger.mergeUnion(sentBefore.get(path)!, ours, madeBefore.get(path)!);
        sentBefore.set(path, ours);
        madeBefore.set(path, mergedBytes);
        contents.push(mergedBytes);
        return { path: c.path, entry: { mode: c.entry.mode, id: blobId(mergedBytes) } };
      });
      const encoded = encode({ ...commit, parents: [previous, ...commit.parents.slice(1)], changes });
      made.push(encoded);
      previous = idOf(encoded);
    }
    const placed: Placed[] = [];
    if (contents.length > 0) {
      const [pack, at] = packed(contents);
      const packId = hash(pack);
      contents.forEach((c, i) => placed.push({ id: blobId(c), pack: packId, at: at[i]!, length: c.length }));
      await env.OBJECTS.put(`packs/${hex(packId)}`, pack);
    }
    const landed = await repo.landMerged(ref, chain, made, plan.tipSeq, plan.paths.map((p) => p.path), placed, after);
    if ("moved" in landed) continue;
    if (landed.status !== "updated" && landed.status !== "rebased") return null;
    return { ...landed, merged: plan.paths.map((p) => p.path) };
  }
  return null;
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    let parts: string[];
    try {
      parts = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
    } catch {
      return json({ error: "a part of that address is not a well-formed escape" }, 400);
    }
    const [owner, repository, what, ...rest] = parts;
    // An owner or a repository is one part of the address: a `/` spelled `%2F` would make two addresses one repository.
    if (owner?.includes("/") || repository?.includes("/")) return json({ error: "an owner or a repository name has no `/` in it" }, 400);
    const arg = rest.join("/");
    if (!owner || !repository || !what || !arg) return json({ error: "expected /<owner>/<owner>/<repo>/<operation>/<name>" }, 404);
    const name = `${owner}/${repository}`;
    // The generated binding type does not name the class behind it.
    const repos = env.REPO as unknown as DurableObjectNamespace<Repo>;
    const repo = repos.get(repos.idFromName(name));
    // Written as a number is written, and nothing else (thirteenth round: `0x1`, `1e0` and an empty one were taken).
    const said = url.searchParams.get("after");
    if (said !== null && !/^(-1|0|[1-9][0-9]{0,15})$/.test(said)) return json({ error: "`after` is a position in a ref's history: a whole number, -1 or more" }, 400);
    const after = said !== null ? Number(said) : null;
    // A position in a ref's history; -1 is before its first commit, which is at 0.
    if (after !== null && !(Number.isSafeInteger(after) && after >= -1)) return json({ error: "`after` is a position in a ref's history: a whole number, -1 or more" }, 400);
    const body = async () => new Uint8Array(await request.arrayBuffer());
    try {
      if (request.method === "POST") {
        if (what === "push" && !refOk(arg)) return json({ error: "that is not a name for a ref" }, 400);
        if (what === "push") {
          // The commits of the push, oldest first, then the contents they name.
          const all = frames(await body());
          const count = Number(url.searchParams.get("commits") ?? 1);
          if (!Number.isInteger(count) || count < 1 || all.length < count) return json({ error: "a push carries a commit" }, 400);
          // Their own bytes: a view into the request would take the whole request along to the object.
          const commits = all.slice(0, count).map((bytes) => bytes.slice());
          const stated = url.searchParams.get("stated") === "1";
          let attested: Attested | null = null;
          if (stated) {
            const said = all[count];
            if (!said) return json({ error: "a push that states its union paths says what of them" }, 400);
            attested = statement(said);
            if (!attested) return json({ error: "what a push states of its union paths is not well formed" }, 400);
          }
          const contents = all.slice(count + (stated ? 1 : 0));
          // A name is a promise about the content, so the name is worked out here, not taken.
          // All of them go into the bucket as one object.
          const placed: Placed[] = [];
          if (contents.length > 0) {
            const [pack, at] = packed(contents);
            const packId = hash(pack);
            contents.forEach((bytes, i) => placed.push({ id: blobId(bytes), pack: packId, at: at[i]!, length: bytes.length }));
            await env.OBJECTS.put(`packs/${hex(packId)}`, pack);
          }
          // A link is written where its content says it leads, by every client that syncs: a target
          // no file system takes would leave the ref one nobody can sync (thirteenth round).
          const links = commits.flatMap((bytes) => decode(bytes).changes.flatMap((c) => (c.entry?.mode === MODE_LINK ? [c.entry.id] : [])));
          // And the links below a directory set whole, however their contents came: checked where
          // the directory comes into the ref, not where a content comes in, as the same bytes may be
          // any file's (fourth pass: a note of them refused honest files, and missed adoption).
          const dirs = commits.flatMap((bytes) => decode(bytes).changes.flatMap((c) => (c.entry?.mode === MODE_DIR ? [c.entry.id] : [])));
          // What a push sets whole is bounded as a whole: many directories set to one large tree
          // hold its files many times over (sixth pass: 64 of 2^22 each, in 3 KB).
          if (dirs.length > 0) {
            const sums = await repo.sumsOf(dirs);
            const set = commits.flatMap((bytes) => decode(bytes).changes.flatMap((c) => (c.entry?.mode === MODE_DIR ? [c.entry.id] : [])));
            const total = set.reduce((n, id) => n + (sums.get(hex(id))?.files ?? 0), 0);
            if (total > MAX_FILES) return json({ error: `the directories this push sets hold more than ${MAX_FILES} files` }, 400);
          }
          const below = dirs.length > 0 ? await repo.linksBelow(dirs) : { links: [], walked: [] };
          links.push(...below.links);
          if (links.length > 0) {
            const sent = new Map(contents.map((bytes) => [hex(blobId(bytes)), bytes]));
            const elsewhere = links.filter((id) => !sent.has(hex(id)));
            const stored = elsewhere.length > 0 ? await read(env, await repo.where(elsewhere)) : new Map<string, Uint8Array>();
            for (const id of links) {
              const target = sent.get(hex(id)) ?? stored.get(hex(id));
              if (target && !linkOk(target)) return json({ error: "a link leads nowhere a file system can write: nothing, a NUL, or more than 1,024 bytes" }, 400);
            }
          }
          // Their links all sound, the directories walked need not be again.
          if (below.walked.length > 0) {
            const sent = new Set(contents.map((bytes) => hex(blobId(bytes))));
            const known = new Set((await repo.where(below.links)).map((c) => hex(c.id)));
            if (below.links.every((id) => sent.has(hex(id)) || known.has(hex(id)))) await repo.linksSound(below.walked);
          }
          const result = await repo.push(arg, commits, after, placed);
          if (result.status === "conflict" && attested) {
            const merged = await mergeOnServer(env, repo, arg, commits, attested, after);
            if (merged) return json({ ...merged, since: steps(merged.since) });
          }
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
          // No path below these longer than a file system takes, those handed over before
          // included: the longest below each, worked out from the bottom up without recursion.
          const deepest = new Map<string, number>();
          // And how many files each holds, and whether any link is below it: a directory of shared
          // directories can name more files than any client could write (fifth pass: 2^60 in 3 KB),
          // and a push need look for links only where there are some.
          const fileCount = new Map<string, number>();
          const linkBelow = new Map<string, boolean>();
          {
            const byId = new Map(trees.map((t) => [hex(t.id), t]));
            const waiting = new Map<string, number>();
            const above = new Map<string, string[]>();
            for (const t of trees) {
              const inside = t.entries.filter((e) => e.mode === MODE_DIR && byId.has(hex(e.id)) && hex(e.id) !== hex(t.id));
              waiting.set(hex(t.id), new Set(inside.map((e) => hex(e.id))).size);
              for (const id of new Set(inside.map((e) => hex(e.id)))) above.set(id, [...(above.get(id) ?? []), hex(t.id)]);
            }
            // What these hold that was handed over before, asked for at once.
            const before = new Map<string, Uint8Array>();
            for (const t of trees) for (const e of t.entries) if (e.mode === MODE_DIR && !byId.has(hex(e.id))) before.set(hex(e.id), e.id);
            const earlier = before.size > 0 ? await repo.sumsOf([...before.values()]) : new Map<string, { files: number; linked: boolean; deepest: number }>();
            const ready = [...waiting].filter(([, n]) => n === 0).map(([id]) => id);
            while (ready.length > 0) {
              const id = ready.pop()!;
              let longest = 0;
              let count = 0;
              let anyLink = false;
              for (const e of byId.get(id)!.entries) {
                const below = e.mode === MODE_DIR ? 1 + (deepest.get(hex(e.id)) ?? earlier.get(hex(e.id))?.deepest ?? 0) : 0;
                longest = Math.max(longest, e.name.length + below);
                if (e.mode === MODE_DIR) {
                  const known = fileCount.has(hex(e.id)) ? { files: fileCount.get(hex(e.id))!, linked: linkBelow.get(hex(e.id))! } : earlier.get(hex(e.id));
                  // One handed over before these were counted: its files are not known, and it may hold links.
                  count += known?.files ?? 0;
                  anyLink ||= known?.linked ?? true;
                } else {
                  count += 1;
                  anyLink ||= e.mode === MODE_LINK;
                }
              }
              if (longest > MAX_PATH) return json({ error: "a directory below these has a path longer than a file system takes" }, 400);
              if (count > MAX_FILES) return json({ error: `a directory below these holds more than ${MAX_FILES} files` }, 400);
              deepest.set(id, longest);
              fileCount.set(id, count);
              linkBelow.set(id, anyLink);
              for (const parent of above.get(id) ?? []) {
                const left = waiting.get(parent)! - 1;
                waiting.set(parent, left);
                if (left === 0) ready.push(parent);
              }
            }
            if (deepest.size !== trees.length) return json({ error: "these directories are each other's, round in a circle" }, 400);
          }
          const below = (dirs: boolean) => {
            const ids = new Map<string, Uint8Array>();
            for (const t of trees) for (const e of t.entries) if (e.mode !== MODE_GITLINK && (e.mode === MODE_DIR) === dirs) ids.set(hex(e.id), e.id);
            return ids;
          };
          const has = async (ids: Uint8Array[]) => new Set((await repo.where(ids)).map((c) => hex(c.id)));
          const outside = [...below(true)].filter(([id]) => !given.has(id)).map(([, id]) => id);
          if ((await has(outside)).size !== outside.length || (await repo.trees(outside)).size !== new Set(outside.map(hex)).size) {
            return json({ error: "a directory below these was not handed over" }, 400);
          }
          // A link below these leads where its content says, for every client that syncs: one no
          // file system can make is refused here, or when its content comes (second pass: a link
          // in a directory set whole got by the check of a commit's own changes).
          const linkIds = [...new Map(trees.flatMap((t) => t.entries.filter((e) => e.mode === MODE_LINK).map((e) => [hex(e.id), e.id] as const))).values()];
          const badLink = async (placed: Placed[]) => [...(await read(env, placed)).values()].some((bytes) => !linkOk(bytes));
          const linkRefused = () => json({ error: "a link below these leads nowhere a file system can write: nothing, a NUL, or more than 1,024 bytes" }, 400);
          if (linkIds.length > 0 && (await badLink(await repo.where(linkIds)))) return linkRefused();
          const files = [...below(false).values()];
          const had = await has(files);
          let missing = files.filter((id) => !had.has(hex(id)));
          let adopted = 0;
          const from = url.searchParams.get("from")?.match(/^\/([^/]+\/[^/]+)$/)?.[1];
          if (from && missing.length > 0) {
            const source = repos.get(repos.idFromName(from));
            // A file's content, not a directory: what the source repository was handed as one stays one.
            const dirs = await source.trees(missing);
            const theirs = (await source.where(missing)).filter((c) => !dirs.has(hex(c.id)));
            const links = new Set(linkIds.map(hex));
            if (await badLink(theirs.filter((c) => links.has(hex(c.id))))) return linkRefused();
            await repo.place(theirs);
            adopted = theirs.length;
            const taken = new Set(theirs.map((c) => hex(c.id)));
            missing = missing.filter((id) => !taken.has(hex(id)));
          }
          // Those whose content is still to come are checked when a push sets a directory holding them.
          const known = await has(trees.map((t) => t.id));
          const fresh = trees.filter((t) => !known.has(hex(t.id)));
          if (fresh.length > 0) {
            const [pack, at] = packed(fresh.map((t) => t.bytes));
            const packId = hash(pack);
            await env.OBJECTS.put(`packs/${hex(packId)}`, pack);
            await repo.place(fresh.map((t, i) => ({ id: t.id, pack: packId, at: at[i]!, length: t.bytes.length })));
          }
          // Each of them a directory from now on, with what it holds and how deep it goes.
          const names = new TextDecoder();
          for (const t of trees) {
            const entries = t.entries.map((e) => ({
              name: names.decode(e.name),
              child: e.id,
              dir: e.mode === MODE_DIR,
              gitlink: e.mode === MODE_GITLINK,
              link: e.mode === MODE_LINK,
            }));
            await repo.describe(t.id, entries, deepest.get(hex(t.id))!, fileCount.get(hex(t.id))!, linkBelow.get(hex(t.id))!);
          }
          await repo.markTrees(trees.map((t) => t.id));
          return json({ adopted, missing: missing.map(hex) });
        }
        if (what === "fetch" && arg === "trees") {
          // These directories and every directory below them, each read once.
          const packs = new Map<string, Uint8Array>();
          const out: Uint8Array[] = [];
          const seen = new Set<string>();
          let level = [...new Set((await request.text()).split("\n").filter(Boolean))].map(unhex);
          while (level.length > 0) {
            const found = await repo.where(level);
            const asTrees = await repo.trees(level);
            if (found.length !== level.length || asTrees.size !== new Set(level.map(hex)).size) return json({ error: "no such directory" }, 404);
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
          // Each content once, however often it is asked for: an answer is no larger than what the
          // repository holds (twelfth round: one id a thousand times was a thousand copies).
          const ids = [...new Set((await request.text()).split("\n").filter(Boolean))].map(unhex);
          const found = await repo.where(ids);
          // By pack: one read of the bucket for each, and of only the bytes wanted when it is one content.
          const packs = new Map<string, Placed[]>();
          for (const c of found) if (c.pack) packs.set(hex(c.pack), [...(packs.get(hex(c.pack)) ?? []), c]);
          const read = await Promise.all(
            [...packs].map(async ([pack, wanted]) => {
              const only = wanted.length === 1 ? wanted[0]! : null;
              // An empty content alone: a range of no bytes is one the bucket refuses (thirteenth round).
              if (only && only.length === 0) return [only.id, new Uint8Array()];
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
          // It goes under a name of its own first. The name it claims is shared by every
          // repository, and bytes that turn out not to be what they claim must never have been there.
          const hasher = blobHasher();
          const hashing = new TransformStream<Uint8Array, Uint8Array>({
            transform(chunk, controller) {
              hasher.update(chunk);
              controller.enqueue(chunk);
            },
          });
          const arriving = `uploads/${crypto.randomUUID()}`;
          await env.OBJECTS.put(arriving, request.body.pipeThrough(hashing).pipeThrough(new FixedLengthStream(length)));
          try {
            if (hex(hasher.digest()) !== arg) return json({ error: "the bytes do not hash to that id" }, 400);
            // Stored once: if it is there already, these bytes are the same bytes.
            if (!(await env.OBJECTS.head(key))) {
              const checked = await env.OBJECTS.get(arriving);
              if (!checked) throw new Error("the upload is gone from the bucket");
              await env.OBJECTS.put(key, checked.body);
            }
          } finally {
            await env.OBJECTS.delete(arriving);
          }
          await repo.place([{ id, pack: null, at: 0, length }]);
          return new Response(null, { status: 204 });
        }
        const [placed] = await repo.where([id]);
        if (!placed) return json({ error: "no such object" }, 404);
        if (placed.length === 0) return new Response(new Uint8Array());
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
