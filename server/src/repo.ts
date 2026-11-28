// One repository: the part of it that has to be agreed on.
//
// Objects are immutable and named by their hash, so any number of writers
// can store them at once. What cannot be written at once is a ref: which
// commit a branch is at. A Durable Object runs one request at a time and its
// SQLite writes are synchronous, so a push is serialised here with no lock,
// no quorum round and no retry loop.
//
// A push hands over one commit. A commit is a list of changed paths on a
// parent, so the three outcomes need nothing but the paths:
//
// - The ref is at the commit's parent: the ref moves to the commit.
// - The ref has moved on, and nothing since touched the commit's paths: the
//   same changes are put on the new tip (only the parent, and so the id,
//   differs) and the ref moves there. The writer need not try again.
// - Something since touched the same paths: the commit is kept as a
//   divergence of the ref, for whoever resolves it. The push is not lost,
//   and the ref does not move.
//
// "Since" is answered from one table: for each path, the position in the
// ref's history at which it last changed. A push looks up its own paths and
// their parent directories, whatever the distance between its base and the
// tip, so a writer a million commits behind costs what one at the tip does.

import { DurableObject } from "cloudflare:workers";
import { type Commit, decode, encode, folded, hex, idOf, MAX_PATH, pathOk } from "./commit.ts";
import { MODE_DIR, MODE_GITLINK } from "./tree.ts";

export type Step = { seq: number; id: string; bytes: Uint8Array };
/// Where a file's content is in the bucket: at `at` in the pack `pack`, or
/// in an object of its own when `pack` is null.
export type Placed = { id: Uint8Array; pack: Uint8Array | null; at: number; length: number };
/// A push that landed: `merged` names the paths the server merged by union, if it did.
export type Landed = { status: "updated" | "rebased"; commit: string; commits: string[]; seq: number; behind: number; since: Step[]; merged?: string[] };
export type PushResult =
  | Landed
  | { status: "conflict"; commit: string; tip: string; paths: string[]; behind: number }
  | { status: "unknown-base"; tip: string };

/// A path's entry, as a pusher states it and as the server knows it.
export type Stated = { mode: number; id: string };
/// What a pusher says of the paths it changes that `.gitattributes` merges by union: each one's
/// entry at the push's base, and the `.gitattributes` files above it there (`null`: none).
export type Attested = { union: { path: string; base: Stated | null }[]; attributes: { path: string; entry: Stated | null }[] };
/// What a merge on the server needs, all of it checked: for each path, the entry at the push's
/// base, the ref's at its tip, and the push's in each of its commits (`null`: not changed there).
export type UnionPlan = {
  tip: string;
  tipSeq: number;
  paths: { path: string; mode: number; base: string; theirs: string; chain: (string | null)[] }[];
  attributes: { dir: string; id: string }[];
  contents: Placed[];
};

const decoder = new TextDecoder();
const encoder = new TextEncoder();
const REGULAR = [0o100644, 0o100755, 0o100664];
/// The directories whose `.gitattributes` speak of a path, the top first (`union::attribute_dirs`).
const attributeDirs = (path: string) => ["", ...[...path.matchAll(/\//g)].map((m) => path.slice(0, m.index))];
const attributesAt = (dir: string) => (dir ? `${dir}/.gitattributes` : ".gitattributes");

export class Repo extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS refs (name TEXT PRIMARY KEY, tip BLOB NOT NULL, seq INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS history (ref TEXT NOT NULL, seq INTEGER NOT NULL, id BLOB NOT NULL, PRIMARY KEY (ref, seq));
      CREATE INDEX IF NOT EXISTS history_by_id ON history (ref, id);
      CREATE TABLE IF NOT EXISTS changed (ref TEXT NOT NULL, path TEXT NOT NULL, seq INTEGER NOT NULL, PRIMARY KEY (ref, path)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS commits (id BLOB PRIMARY KEY, bytes BLOB NOT NULL);
      CREATE TABLE IF NOT EXISTS divergent (ref TEXT NOT NULL, id BLOB NOT NULL, onto BLOB NOT NULL, paths TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS landed (ref TEXT NOT NULL, sent BLOB NOT NULL, landed BLOB NOT NULL, moved INTEGER NOT NULL, PRIMARY KEY (ref, sent)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS placed (id BLOB PRIMARY KEY, pack BLOB, at INTEGER NOT NULL, length INTEGER NOT NULL) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS trees (id BLOB PRIMARY KEY) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS tree_entries (tree BLOB NOT NULL, name TEXT NOT NULL, child BLOB NOT NULL, dir INTEGER NOT NULL, PRIMARY KEY (tree, name)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS tree_depth (id BLOB PRIMARY KEY, deepest INTEGER NOT NULL) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS live (ref TEXT NOT NULL, folded TEXT NOT NULL, path TEXT NOT NULL, PRIMARY KEY (ref, folded)) WITHOUT ROWID;
      CREATE INDEX IF NOT EXISTS live_by_path ON live (ref, path);
      CREATE TABLE IF NOT EXISTS path_index (ref TEXT NOT NULL, path TEXT NOT NULL, seq INTEGER NOT NULL, mode INTEGER, id BLOB, PRIMARY KEY (ref, path, seq)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS indexed (ref TEXT PRIMARY KEY, since INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS links_awaited (id BLOB PRIMARY KEY) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS tree_links (tree BLOB NOT NULL, child BLOB NOT NULL, PRIMARY KEY (tree, child)) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS tree_sums (id BLOB PRIMARY KEY, files INTEGER NOT NULL, linked INTEGER NOT NULL) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS links_checked (id BLOB PRIMARY KEY) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS trees_whole (id BLOB PRIMARY KEY) WITHOUT ROWID;
      CREATE TABLE IF NOT EXISTS landed_merged (ref TEXT NOT NULL, sent BLOB NOT NULL, paths TEXT NOT NULL, PRIMARY KEY (ref, sent)) WITHOUT ROWID;
    `);
  }

  /// The paths of `mine` that changed after position `since`, or that a
  /// change after it sits above or below.
  private clashes(ref: string, mine: string[], since: number): string[] {
    const sql = this.ctx.storage.sql;
    const moved = (path: string) => sql.exec("SELECT 1 FROM changed WHERE ref = ? AND path = ? AND seq > ?", ref, path, since).toArray().length > 0;
    return mine.filter((path) => {
      if (moved(path)) return true;
      // A file where this push has a directory...
      for (let i = path.indexOf("/"); i >= 0; i = path.indexOf("/", i + 1)) if (moved(path.slice(0, i))) return true;
      // ...or anything inside a directory where this push has a file. `0` is the character after `/`.
      return sql.exec("SELECT 1 FROM changed WHERE ref = ? AND path > ? AND path < ? AND seq > ? LIMIT 1", ref, path + "/", path + "0", since).toArray().length > 0;
    });
  }

  private steps(ref: string, after: number): Step[] {
    const rows = this.ctx.storage.sql.exec<{ seq: number; id: ArrayBuffer; bytes: ArrayBuffer }>(
      "SELECT h.seq AS seq, h.id AS id, c.bytes AS bytes FROM history h JOIN commits c ON c.id = h.id WHERE h.ref = ? AND h.seq > ? ORDER BY h.seq",
      ref,
      after,
    );
    return rows.toArray().map((r) => ({ seq: r.seq, id: hex(new Uint8Array(r.id)), bytes: new Uint8Array(r.bytes) }));
  }

  /// Everything between reading the ref and moving it is synchronous: no
  /// other push can run in between. `after`, if given, asks for the commits
  /// the ref went through since that position, so that the writer is caught
  /// up by the same answer.
  ///
  /// A push is a chain of commits, each on the one before, and it lands
  /// whole or not at all. What it is checked against is everything the ref
  /// took since the chain's base: checking each commit against its own
  /// parent would let the second of two overwrite what landed while the
  /// writer was away, since by then its parent is the tip.
  ///
  /// Sent again, a push that landed gets the answer it got: a writer that
  /// never heard back can simply repeat it.
  push(ref: string, chain: Uint8Array[], after: number | null, contents: Placed[] = []): PushResult {
    const sql = this.ctx.storage.sql;
    const commits = chain.map(decode);
    const first = commits[0];
    if (!first) throw new Error("a push carries a commit");
    for (let i = 1; i < commits.length; i++) {
      const parent = commits[i]!.parents[0];
      if (!parent || hex(parent) !== hex(idOf(chain[i - 1]!))) throw new Error("the commits of a push are a chain, each on the one before");
    }
    const paths = [...new Set(commits.flatMap((commit) => commit.changes.map((c) => decoder.decode(c.path))))];
    // A change may set a whole directory. Its files are not in the commit, so the
    // repository has to have been handed the directory first (`adopt`).
    const dirs = commits.flatMap((commit) => commit.changes.flatMap((c) => (c.entry?.mode === MODE_DIR ? [c.entry.id] : [])));
    const sent = idOf(chain[chain.length - 1]!);
    return this.ctx.storage.transactionSync(() => {
      // The contents are in the bucket already. Whatever becomes of the commits, the repository has them.
      this.place(contents);
      if (this.where(dirs).length !== dirs.length || this.trees(dirs).size !== new Set(dirs.map(hex)).size) {
        throw new Error("the commit sets a directory this repository was not handed");
      }
      // Whole: every file below it sent, as a file; and no path below it, from where it is set,
      // longer than a file system takes.
      this.whole(dirs);
      for (const commit of commits) {
        for (const c of commit.changes) {
          if (c.entry?.mode === MODE_DIR && c.path.length + 1 + this.deepest(c.entry.id) > MAX_PATH) {
            throw new Error("the commit sets a directory where paths below it grow longer than a file system takes");
          }
        }
      }
      // Nor may a ref name contents nobody sent: whoever synced next could not, and nobody after them.
      const files = commits.flatMap((commit) => commit.changes.flatMap((c) => (c.entry && c.entry.mode !== MODE_DIR && c.entry.mode !== MODE_GITLINK ? [c.entry.id] : [])));
      const given = new Set(this.where(files).map((c) => hex(c.id)));
      if (files.some((id) => !given.has(hex(id)))) throw new Error("the commit names contents this repository was not sent");
      // A file is a file: a directory handed over is not a file's content.
      if (this.trees(files).size > 0) throw new Error("the commit gives a file the id of a directory");
      const at = sql.exec<{ tip: ArrayBuffer; seq: number }>("SELECT tip, seq FROM refs WHERE name = ?", ref).toArray()[0];
      const tip = at ? new Uint8Array(at.tip) : null;
      const since = () => (after === null ? [] : this.steps(ref, after));

      // Landed before, as it was or moved: the same answer again.
      const before = sql.exec<{ landed: ArrayBuffer; moved: number }>("SELECT landed, moved FROM landed WHERE ref = ? AND sent = ?", ref, sent).toArray()[0];
      if (before && at) {
        const landed = new Uint8Array(before.landed);
        const seq = sql.exec<{ seq: number }>("SELECT seq FROM history WHERE ref = ? AND id = ?", ref, landed).toArray()[0]!.seq;
        const ids = sql.exec<{ id: ArrayBuffer }>("SELECT id FROM history WHERE ref = ? AND seq > ? AND seq <= ? ORDER BY seq", ref, seq - chain.length, seq).toArray();
        // How far behind it was, as it was told the first time: from where its base was to where
        // the chain was put (twelfth round: sent again, it was told 0).
        const parent = first.parents[0];
        // A chain with no parent was put on the tip as from the ref's start, as the first time.
        const base = parent ? sql.exec<{ seq: number }>("SELECT seq FROM history WHERE ref = ? AND id = ?", ref, parent).toArray()[0]?.seq : 0;
        const behind = before.moved && base !== undefined ? Math.max(0, seq - chain.length - base) : 0;
        // And what the server merged of it, as it was told the first time (thirteenth round).
        const merged = sql.exec<{ paths: string }>("SELECT paths FROM landed_merged WHERE ref = ? AND sent = ?", ref, sent).toArray()[0];
        return {
          status: before.moved ? "rebased" : "updated",
          commit: hex(landed),
          commits: ids.map((r) => hex(new Uint8Array(r.id))),
          seq,
          behind,
          since: since(),
          ...(merged ? { merged: JSON.parse(merged.paths) as string[] } : {}),
        };
      }

      const parent = first.parents[0] ?? null;
      const accept = (onto: Uint8Array | null, status: "updated" | "rebased", behind: number): PushResult =>
        this.accept(ref, at ?? null, parent, commits, (i, previous) => (previous && status === "rebased" ? encode({ ...commits[i]!, parents: [previous, ...commits[i]!.parents.slice(1)] }) : chain[i]!), onto, status, behind, sent, since);
      // A new ref starts wherever its first commit says.
      if (!tip || !at || (parent && hex(parent) === hex(tip))) return accept(null, "updated", 0);

      // The ref has moved on. Where was it when this chain was started?
      let base = 0;
      if (parent) {
        const known = sql.exec<{ seq: number }>("SELECT seq FROM history WHERE ref = ? AND id = ?", ref, parent).toArray()[0];
        if (!known) return { status: "unknown-base", tip: hex(tip) };
        base = known.seq;
      }
      const behind = at.seq - base;
      const clash = this.clashes(ref, paths, base);
      if (clash.length > 0) {
        for (const bytes of chain) sql.exec("INSERT OR IGNORE INTO commits (id, bytes) VALUES (?, ?)", idOf(bytes), bytes);
        // Kept aside once, however often it is sent.
        sql.exec("DELETE FROM divergent WHERE ref = ? AND id = ?", ref, sent);
        sql.exec("INSERT INTO divergent (ref, id, onto, paths) VALUES (?, ?, ?, ?)", ref, sent, tip, JSON.stringify(clash));
        return { status: "conflict", commit: hex(sent), tip: hex(tip), paths: clash, behind };
      }
      // Nothing in the way: the same changes, on the tip.
      return accept(tip, "rebased", behind);
    });
  }

  /// Put commits on the ref: `bytes(i, previous)` is the i-th as it lands, on `previous`.
  private accept(
    ref: string,
    at: { tip: ArrayBuffer; seq: number } | null,
    parent: Uint8Array | null,
    commits: Commit[],
    bytes: (i: number, previous: Uint8Array | null) => Uint8Array,
    onto: Uint8Array | null,
    status: "updated" | "rebased",
    behind: number,
    sent: Uint8Array,
    since: () => Step[],
  ): PushResult {
    const sql = this.ctx.storage.sql;
    let seq = at?.seq ?? 0;
    // A ref's first commit brings its parent along as position 0, so that
    // a workspace still on that parent can find itself in the history.
    if (!at && parent) sql.exec("INSERT INTO history (ref, seq, id) VALUES (?, 0, ?)", ref, parent);
    // Every change to every path from here on is in `path_index`: what a merge on the server
    // reads a path's entry at a push's base from. A ref older than the index is not merged on.
    sql.exec("INSERT OR IGNORE INTO indexed (ref, since) VALUES (?, ?)", ref, seq);
    const ids: Uint8Array[] = [];
    let previous = onto;
    commits.forEach((commit, i) => {
      const encoded = bytes(i, previous);
      const landed = decode(encoded);
      const id = idOf(encoded);
      seq += 1;
      sql.exec("INSERT OR IGNORE INTO commits (id, bytes) VALUES (?, ?)", id, encoded);
      sql.exec("INSERT INTO history (ref, seq, id) VALUES (?, ?, ?)", ref, seq, id);
      for (const c of landed.changes) {
        const path = decoder.decode(c.path);
        sql.exec("INSERT INTO changed (ref, path, seq) VALUES (?, ?, ?) ON CONFLICT (ref, path) DO UPDATE SET seq = excluded.seq", ref, path, seq);
        sql.exec("INSERT OR REPLACE INTO path_index (ref, path, seq, mode, id) VALUES (?, ?, ?, ?, ?)", ref, path, seq, c.entry?.mode ?? null, c.entry?.id ?? null);
        this.spell(ref, path, c.entry);
      }
      ids.push(id);
      previous = id;
    });
    const last = ids[ids.length - 1]!;
    sql.exec("INSERT INTO refs (name, tip, seq) VALUES (?, ?, ?) ON CONFLICT (name) DO UPDATE SET tip = excluded.tip, seq = excluded.seq", ref, last, seq);
    sql.exec("INSERT OR REPLACE INTO landed (ref, sent, landed, moved) VALUES (?, ?, ?, ?)", ref, sent, last, status === "rebased" ? 1 : 0);
    return { status, commit: hex(last), commits: ids.map(hex), seq, behind, since: since() };
  }

  /// The entry `path` had at position `seq` of the ref, as far as the index knows: `undefined`
  /// where no change to it at or before `seq` is in it.
  private entryAt(ref: string, path: string, seq: number): { mode: number; id: Uint8Array } | null | undefined {
    const row = this.ctx.storage.sql
      .exec<{ mode: number | null; id: ArrayBuffer | null }>("SELECT mode, id FROM path_index WHERE ref = ? AND path = ? AND seq <= ? ORDER BY seq DESC LIMIT 1", ref, path, seq)
      .toArray()[0];
    if (!row) return undefined;
    return row.mode === null || row.id === null ? null : { mode: row.mode, id: new Uint8Array(row.id) };
  }

  /// Whether a push refused for clashing only on paths `.gitattributes` merges by union can be
  /// merged here, and with what: every entry checked against the ref's own history where it has
  /// one, and taken from the pusher where the path is as it was before the ref began (nothing
  /// the pusher says can lose a line of anyone else's: a union keeps both sides). A reason in
  /// words where it cannot be.
  unionPlan(ref: string, chain: Uint8Array[], attested: Attested): UnionPlan | { why: string } {
    const sql = this.ctx.storage.sql;
    const commits = chain.map(decode);
    const at = sql.exec<{ tip: ArrayBuffer; seq: number }>("SELECT tip, seq FROM refs WHERE name = ?", ref).toArray()[0];
    const parent = commits[0]?.parents[0];
    if (!at || !parent) return { why: "no ref, or a chain with no base" };
    const known = sql.exec<{ seq: number }>("SELECT seq FROM history WHERE ref = ? AND id = ?", ref, parent).toArray()[0];
    if (!known) return { why: "the base is not on the ref" };
    const base = known.seq;
    const indexed = sql.exec<{ since: number }>("SELECT since FROM indexed WHERE ref = ?", ref).toArray()[0];
    if (!indexed || indexed.since !== 0) return { why: "the ref began before its paths were indexed" };
    const paths = [...new Set(commits.flatMap((c) => c.changes.map((x) => decoder.decode(x.path))))];
    const clash = this.clashes(ref, paths, base);
    const stated = new Map(attested.union.map((u) => [u.path, u.base]));
    const statedAttributes = new Map(attested.attributes.map((a) => [a.path, a.entry]));
    if (stated.size !== attested.union.length || statedAttributes.size !== attested.attributes.length) return { why: "a path stated twice" };
    const sameAs = (entry: { mode: number; id: Uint8Array } | null, said: Stated | null | undefined) =>
      said !== undefined && (entry === null ? said === null : said !== null && said.mode === entry.mode && said.id === hex(entry.id));
    const unhexed = (said: Stated) => {
      if (!/^[0-9a-f]{64}$/.test(said.id) || !REGULAR.includes(said.mode)) throw new Error("a stated entry is not a file's");
      return { mode: said.mode, id: Uint8Array.from(said.id.match(/../g)!.map((b) => parseInt(b, 16))) };
    };
    const plan: UnionPlan = { tip: hex(new Uint8Array(at.tip)), tipSeq: at.seq, paths: [], attributes: [], contents: [] };
    const wanted: Uint8Array[] = [];
    const dirs = new Set<string>();
    for (const path of clash) {
      if (!stated.has(path)) return { why: `${path} is not stated as merged by union` };
      if (!pathOk(encoder.encode(path))) return { why: "a path no tree may have" };
      // The path itself moved, as a file, and nothing above or below it did.
      const theirs = this.entryAt(ref, path, at.seq);
      if (!theirs || !REGULAR.includes(theirs.mode)) return { why: `${path} is not a file on the ref` };
      const above = attributeDirs(path).slice(1).some((dir) => sql.exec("SELECT 1 FROM changed WHERE ref = ? AND path = ? AND seq > ?", ref, dir, base).toArray().length > 0);
      const below = sql.exec("SELECT 1 FROM changed WHERE ref = ? AND path > ? AND path < ? AND seq > ? LIMIT 1", ref, path + "/", path + "0", base).toArray().length > 0;
      if (above || below) return { why: `a directory moved at ${path}` };
      // Its entry at the base: the ref's word where it has one, the pusher's where it has none.
      const indexedBase = this.entryAt(ref, path, base);
      const said = stated.get(path) ?? null;
      if (indexedBase !== undefined && !sameAs(indexedBase, said)) return { why: `${path} was not at the base what the push says` };
      if (said === null) return { why: `${path} is new on both sides` };
      const baseEntry = unhexed(said);
      const changes = commits.map((c) => c.changes.find((x) => decoder.decode(x.path) === path));
      if (changes.some((c) => c && (!c.entry || c.entry.mode !== theirs.mode))) return { why: `${path} is removed or changes mode` };
      if (baseEntry.mode !== theirs.mode) return { why: `${path} changes mode` };
      plan.paths.push({ path, mode: theirs.mode, base: hex(baseEntry.id), theirs: hex(theirs.id), chain: changes.map((c) => (c?.entry ? hex(c.entry.id) : null)) });
      wanted.push(baseEntry.id, theirs.id, ...changes.flatMap((c) => (c?.entry ? [c.entry.id] : [])));
      for (const dir of attributeDirs(path)) dirs.add(dir);
    }
    // The rules are the ones the pusher started from, and still the ref's: where they moved since
    // the base, or move in the push, they are not the pusher's to state.
    for (const dir of dirs) {
      const file = attributesAt(dir);
      if (!statedAttributes.has(file)) return { why: `${file} is not stated` };
      if (paths.includes(file)) return { why: `the push changes ${file}` };
      if (sql.exec("SELECT 1 FROM changed WHERE ref = ? AND path = ? AND seq > ?", ref, file, base).toArray().length > 0) return { why: `${file} changed on the ref since the base` };
      const indexedRule = this.entryAt(ref, file, base);
      const said = statedAttributes.get(file) ?? null;
      if (indexedRule !== undefined && !sameAs(indexedRule, said)) return { why: `${file} was not at the base what the push says` };
      if (said) {
        const entry = unhexed(said);
        plan.attributes.push({ dir, id: hex(entry.id) });
        wanted.push(entry.id);
      }
    }
    // Every content read for the merge is one this repository was given.
    const found = this.where(wanted);
    if (new Set(found.map((c) => hex(c.id))).size !== new Set(wanted.map(hex)).size) return { why: "a content the merge needs was not sent" };
    plan.contents = found;
    return plan;
  }

  /// Land what the server merged for a push: `merged` is the push's chain made again on the tip
  /// the plan was made at, with the union paths' merged contents, which are in `contents`.
  /// `{ moved: true }` if the ref moved since, for the merge to be made again.
  landMerged(ref: string, chain: Uint8Array[], merged: Uint8Array[], tipSeq: number, unionPaths: string[], contents: Placed[], after: number | null): PushResult | { moved: true } {
    const sql = this.ctx.storage.sql;
    const commits = chain.map(decode);
    const remade = merged.map(decode);
    const sent = idOf(chain[chain.length - 1]!);
    return this.ctx.storage.transactionSync(() => {
      const at = sql.exec<{ tip: ArrayBuffer; seq: number }>("SELECT tip, seq FROM refs WHERE name = ?", ref).toArray()[0];
      if (!at || at.seq !== tipSeq) return { moved: true as const };
      this.place(contents);
      // The same commits, but for the parents and the union paths' contents, which the repository has.
      const union = new Set(unionPaths);
      if (remade.length !== commits.length) throw new Error("a merge on the server is a chain as long as the push");
      remade.forEach((commit, i) => {
        const on = i === 0 ? new Uint8Array(at.tip) : idOf(merged[i - 1]!);
        if (commit.parents.length !== 1 || hex(commit.parents[0]!) !== hex(on)) throw new Error("a merge on the server is a chain on the tip");
      });
      remade.forEach((commit, i) => {
        const was = commits[i]!;
        if (hex(commit.rest) !== hex(was.rest) || commit.changes.length !== was.changes.length) throw new Error("a merge on the server changed more than contents");
        commit.changes.forEach((c, j) => {
          const w = was.changes[j]!;
          if (hex(c.path) !== hex(w.path)) throw new Error("a merge on the server changed a path");
          const same = (c.entry === null && w.entry === null) || (c.entry && w.entry && c.entry.mode === w.entry.mode && hex(c.entry.id) === hex(w.entry.id));
          if (!same && !union.has(decoder.decode(c.path))) throw new Error("a merge on the server changed a path that is not merged by union");
        });
      });
      const ids = remade.flatMap((c) => c.changes.flatMap((x) => (x.entry ? [x.entry.id] : [])));
      if (new Set(this.where(ids).map((c) => hex(c.id))).size !== new Set(ids.map(hex)).size) throw new Error("a merged content is not in the repository");
      const base = sql.exec<{ seq: number }>("SELECT seq FROM history WHERE ref = ? AND id = ?", ref, commits[0]!.parents[0]!).toArray()[0]!.seq;
      sql.exec("DELETE FROM divergent WHERE ref = ? AND id = ?", ref, sent);
      const result = this.accept(ref, at, commits[0]!.parents[0] ?? null, remade, (i) => merged[i]!, new Uint8Array(at.tip), "rebased", at.seq - base, sent, () => (after === null ? [] : this.steps(ref, after)));
      if (result.status === "rebased" || result.status === "updated") {
        sql.exec("INSERT OR REPLACE INTO landed_merged (ref, sent, paths) VALUES (?, ?, ?)", ref, sent, JSON.stringify(unionPaths));
      }
      return result;
    });
  }

  /// Keep the spellings of the ref's paths up to date with one change, and refuse one a file
  /// system that folds case would take for a path the ref has under another spelling: on macOS
  /// one would be written on, or through, the other, and no one could sync the ref there. The
  /// directories on the way count, and so do the entries of a directory set whole.
  private spell(ref: string, path: string, entry: { mode: number; id: Uint8Array } | null): void {
    const sql = this.ctx.storage.sql;
    // What was at the path, and below it, is gone: replaced or removed. Two statements, each on
    // the index by path: joined by `OR`, the delete read the whole table, once for every path of
    // a push, and a push of many paths took the square of their number (twelfth round).
    sql.exec("DELETE FROM live WHERE ref = ? AND path = ?", ref, path);
    sql.exec("DELETE FROM live WHERE ref = ? AND path > ? AND path < ?", ref, path + "/", path + "0");
    const parts = path.split("/");
    if (!entry) {
      // A directory left with nothing in it is gone as well.
      for (let n = parts.length - 1; n >= 1; n--) {
        const dir = parts.slice(0, n).join("/");
        if (sql.exec("SELECT 1 FROM live WHERE ref = ? AND path > ? AND path < ? LIMIT 1", ref, dir + "/", dir + "0").toArray().length > 0) break;
        sql.exec("DELETE FROM live WHERE ref = ? AND path = ?", ref, dir);
      }
      return;
    }
    const names = parts.map((_, n) => parts.slice(0, n + 1).join("/"));
    if (entry.mode === MODE_DIR) {
      for (const row of sql.exec<{ name: string }>("SELECT name FROM tree_entries WHERE tree = ?", entry.id).toArray()) names.push(`${path}/${row.name}`);
    }
    for (const name of names) {
      const key = folded(name);
      const had = sql.exec<{ path: string }>("SELECT path FROM live WHERE ref = ? AND folded = ?", ref, key).toArray()[0];
      if (had && had.path !== name) throw new Error(`the ref has \`${had.path}\`, which macOS and Windows take for \`${name}\``);
      if (!had) sql.exec("INSERT INTO live (ref, folded, path) VALUES (?, ?, ?)", ref, key, name);
    }
  }

  /// Refuse, of directories to be set whole, one with a file below it this repository was not
  /// sent, or whose entry for a file holds a directory's id. Walked without recursion.
  private whole(dirs: Uint8Array[]): void {
    const sql = this.ctx.storage.sql;
    const seen = new Set<string>();
    const walked: Uint8Array[] = [];
    const todo = [...dirs];
    while (todo.length > 0) {
      const dir = todo.pop()!;
      if (seen.has(hex(dir))) continue;
      seen.add(hex(dir));
      // Found whole before, it is whole still: what the repository was sent it keeps (fifth pass:
      // a directory set again unchanged was walked again).
      if (sql.exec("SELECT 1 FROM trees_whole WHERE id = ?", dir).toArray().length > 0) continue;
      walked.push(dir);
      for (const row of sql.exec<{ child: ArrayBuffer; dir: number }>("SELECT child, dir FROM tree_entries WHERE tree = ?", dir).toArray()) {
        const child = new Uint8Array(row.child);
        if (row.dir) {
          if (this.trees([child]).size === 0) throw new Error("the commit sets a directory with a directory below it this repository was not handed");
          todo.push(child);
        } else if (row.dir === 0) {
          if (this.where([child]).length === 0) throw new Error("the commit sets a directory with a file below it this repository was not sent");
          if (this.trees([child]).size > 0) throw new Error("the commit sets a directory whose entry for a file holds a directory's id");
        }
      }
    }
    for (const id of walked) sql.exec("INSERT OR IGNORE INTO trees_whole (id) VALUES (?)", id);
  }

  /// The longest path below a directory handed over, from its top.
  deepest(id: Uint8Array): number {
    return this.ctx.storage.sql.exec<{ deepest: number }>("SELECT deepest FROM tree_depth WHERE id = ?", id).toArray()[0]?.deepest ?? 0;
  }

  /// Note what a directory handed over holds, and the longest path below it.
  describe(
    id: Uint8Array,
    entries: { name: string; child: Uint8Array; dir: boolean; gitlink: boolean; link: boolean }[],
    deepest: number,
    files: number,
    linked: boolean,
  ): void {
    const sql = this.ctx.storage.sql;
    sql.exec("INSERT OR REPLACE INTO tree_sums (id, files, linked) VALUES (?, ?, ?)", id, files, linked ? 1 : 0);
    for (const e of entries) {
      if (e.link) sql.exec("INSERT OR IGNORE INTO tree_links (tree, child) VALUES (?, ?)", id, e.child);
      // Another repository's commit is neither: nothing below it is this repository's to have.
      sql.exec("INSERT OR IGNORE INTO tree_entries (tree, name, child, dir) VALUES (?, ?, ?, ?)", id, e.name, e.child, e.gitlink ? 2 : e.dir ? 1 : 0);
    }
    sql.exec("INSERT OR REPLACE INTO tree_depth (id, deepest) VALUES (?, ?)", id, deepest);
  }

  /// Note where contents are. One that is placed already stays where it is.
  place(contents: Placed[]): void {
    for (const c of contents) {
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO placed (id, pack, at, length) VALUES (?, ?, ?, ?)", c.id, c.pack, c.at, c.length);
    }
  }

  /// How many files a directory handed over holds, and whether a link is below it; `null` for one
  /// handed over before these were counted.
  sums(id: Uint8Array): { files: number; linked: boolean } | null {
    const row = this.ctx.storage.sql.exec<{ files: number; linked: number }>("SELECT files, linked FROM tree_sums WHERE id = ?", id).toArray()[0];
    return row ? { files: row.files, linked: row.linked === 1 } : null;
  }

  /// The contents of every link below these directories, walked as `whole` walks them: what a
  /// push that sets one of them has to hold to what a link may be, however the content came.
  linksBelow(dirs: Uint8Array[]): { links: Uint8Array[]; walked: Uint8Array[] } {
    const sql = this.ctx.storage.sql;
    const seen = new Set<string>();
    const found = new Map<string, Uint8Array>();
    const walked: Uint8Array[] = [];
    const todo = [...dirs];
    while (todo.length > 0) {
      const dir = todo.pop()!;
      if (seen.has(hex(dir))) continue;
      seen.add(hex(dir));
      // Nothing to look for below one with no link anywhere in it, nor below one whose links were
      // found sound when it was set before: a link's content is what its id says (fifth pass: every
      // tree of a large directory was walked at each push that set it).
      if (this.sums(dir)?.linked === false) continue;
      if (sql.exec("SELECT 1 FROM links_checked WHERE id = ?", dir).toArray().length > 0) continue;
      walked.push(dir);
      for (const row of sql.exec<{ child: ArrayBuffer }>("SELECT child FROM tree_links WHERE tree = ?", dir).toArray()) {
        const child = new Uint8Array(row.child);
        found.set(hex(child), child);
      }
      for (const row of sql.exec<{ child: ArrayBuffer }>("SELECT child FROM tree_entries WHERE tree = ? AND dir = 1", dir).toArray()) {
        todo.push(new Uint8Array(row.child));
      }
    }
    return { links: [...found.values()], walked };
  }

  /// Note directories whose links below were all found sound.
  linksSound(dirs: Uint8Array[]): void {
    for (const id of dirs) this.ctx.storage.sql.exec("INSERT OR IGNORE INTO links_checked (id) VALUES (?)", id);
  }

  /// Note that these objects are directories: handed over as trees, and read as trees.
  markTrees(ids: Uint8Array[]): void {
    for (const id of ids) this.ctx.storage.sql.exec("INSERT OR IGNORE INTO trees (id) VALUES (?)", id);
  }

  /// Of these ids, the ones this repository was handed as directories.
  trees(ids: Uint8Array[]): Set<string> {
    const found = new Set<string>();
    for (const id of ids) if (this.ctx.storage.sql.exec("SELECT 1 FROM trees WHERE id = ?", id).toArray().length > 0) found.add(hex(id));
    return found;
  }

  /// Where each of these contents is, for those this repository has.
  where(ids: Uint8Array[]): Placed[] {
    const found: Placed[] = [];
    for (const id of ids) {
      const row = this.ctx.storage.sql.exec<{ pack: ArrayBuffer | null; at: number; length: number }>("SELECT pack, at, length FROM placed WHERE id = ?", id).toArray()[0];
      if (row) found.push({ id, pack: row.pack ? new Uint8Array(row.pack) : null, at: row.at, length: row.length });
    }
    return found;
  }

  storage(): { contents: number; packs: number; direct: number; bytes: number } {
    const row = this.ctx.storage.sql
      .exec<{ contents: number; packs: number; direct: number; bytes: number }>(
        "SELECT COUNT(*) AS contents, COUNT(DISTINCT pack) AS packs, SUM(pack IS NULL) AS direct, COALESCE(SUM(length), 0) AS bytes FROM placed",
      )
      .toArray()[0]!;
    return { contents: row.contents, packs: row.packs, direct: row.direct ?? 0, bytes: row.bytes };
  }

  head(ref: string): { tip: string; seq: number } | null {
    const at = this.ctx.storage.sql.exec<{ tip: ArrayBuffer; seq: number }>("SELECT tip, seq FROM refs WHERE name = ?", ref).toArray()[0];
    return at ? { tip: hex(new Uint8Array(at.tip)), seq: at.seq } : null;
  }

  /// The ref's tip and the commits it went through after position `after`.
  since(ref: string, after: number): { tip: string; seq: number; since: Step[] } | null {
    const head = this.head(ref);
    return head ? { ...head, since: this.steps(ref, after) } : null;
  }

  /// The first of `ids` that the ref's history holds, and its position.
  /// A workspace sends its newest commits first and learns where it stands.
  locate(ref: string, ids: Uint8Array[]): { id: string; seq: number } | null {
    for (const id of ids) {
      const row = this.ctx.storage.sql.exec<{ seq: number }>("SELECT seq FROM history WHERE ref = ? AND id = ?", ref, id).toArray()[0];
      if (row) return { id: hex(id), seq: row.seq };
    }
    return null;
  }

  log(ref: string, after: number): { seq: number; id: string }[] {
    const rows = this.ctx.storage.sql.exec<{ seq: number; id: ArrayBuffer }>("SELECT seq, id FROM history WHERE ref = ? AND seq > ? ORDER BY seq", ref, after);
    return rows.toArray().map((r) => ({ seq: r.seq, id: hex(new Uint8Array(r.id)) }));
  }

  divergent(ref: string): { id: string; onto: string; paths: string[] }[] {
    const rows = this.ctx.storage.sql.exec<{ id: ArrayBuffer; onto: ArrayBuffer; paths: string }>("SELECT id, onto, paths FROM divergent WHERE ref = ?", ref);
    return rows.toArray().map((r) => ({ id: hex(new Uint8Array(r.id)), onto: hex(new Uint8Array(r.onto)), paths: JSON.parse(r.paths) }));
  }

  commit(id: Uint8Array): Uint8Array | null {
    const row = this.ctx.storage.sql.exec<{ bytes: ArrayBuffer }>("SELECT bytes FROM commits WHERE id = ?", id).toArray()[0];
    return row ? new Uint8Array(row.bytes) : null;
  }
}
