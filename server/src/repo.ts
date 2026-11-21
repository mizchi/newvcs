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
import { decode, encode, hex, idOf } from "./commit.ts";
import { MODE_DIR, MODE_GITLINK } from "./tree.ts";

export type Step = { seq: number; id: string; bytes: Uint8Array };
/// Where a file's content is in the bucket: at `at` in the pack `pack`, or
/// in an object of its own when `pack` is null.
export type Placed = { id: Uint8Array; pack: Uint8Array | null; at: number; length: number };
export type PushResult =
  | { status: "updated" | "rebased"; commit: string; commits: string[]; seq: number; behind: number; since: Step[] }
  | { status: "conflict"; commit: string; tip: string; paths: string[]; behind: number }
  | { status: "unknown-base"; tip: string };

const decoder = new TextDecoder();

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
        return { status: before.moved ? "rebased" : "updated", commit: hex(landed), commits: ids.map((r) => hex(new Uint8Array(r.id))), seq, behind: 0, since: since() };
      }

      const parent = first.parents[0] ?? null;
      const accept = (onto: Uint8Array | null, status: "updated" | "rebased", behind: number): PushResult => {
        let seq = at?.seq ?? 0;
        // A ref's first commit brings its parent along as position 0, so that
        // a workspace still on that parent can find itself in the history.
        if (!at && parent) sql.exec("INSERT INTO history (ref, seq, id) VALUES (?, 0, ?)", ref, parent);
        const ids: Uint8Array[] = [];
        let previous = onto;
        commits.forEach((commit, i) => {
          // Only the parent, and so the id, differs from what was sent.
          const bytes = previous && status === "rebased" ? encode({ ...commit, parents: [previous, ...commit.parents.slice(1)] }) : chain[i]!;
          const id = idOf(bytes);
          seq += 1;
          sql.exec("INSERT OR IGNORE INTO commits (id, bytes) VALUES (?, ?)", id, bytes);
          sql.exec("INSERT INTO history (ref, seq, id) VALUES (?, ?, ?)", ref, seq, id);
          for (const c of commit.changes) {
            sql.exec("INSERT INTO changed (ref, path, seq) VALUES (?, ?, ?) ON CONFLICT (ref, path) DO UPDATE SET seq = excluded.seq", ref, decoder.decode(c.path), seq);
          }
          ids.push(id);
          previous = id;
        });
        const last = ids[ids.length - 1]!;
        sql.exec("INSERT INTO refs (name, tip, seq) VALUES (?, ?, ?) ON CONFLICT (name) DO UPDATE SET tip = excluded.tip, seq = excluded.seq", ref, last, seq);
        sql.exec("INSERT OR REPLACE INTO landed (ref, sent, landed, moved) VALUES (?, ?, ?, ?)", ref, sent, last, status === "rebased" ? 1 : 0);
        return { status, commit: hex(last), commits: ids.map(hex), seq, behind, since: since() };
      };
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

  /// Note where contents are. One that is placed already stays where it is.
  place(contents: Placed[]): void {
    for (const c of contents) {
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO placed (id, pack, at, length) VALUES (?, ?, ?, ?)", c.id, c.pack, c.at, c.length);
    }
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
