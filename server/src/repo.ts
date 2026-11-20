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

export type Step = { seq: number; id: string; bytes: Uint8Array };
export type PushResult =
  | { status: "updated" | "rebased"; commit: string; seq: number; behind: number; since: Step[] }
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
  push(ref: string, encoded: Uint8Array, after: number | null): PushResult {
    const sql = this.ctx.storage.sql;
    const commit = decode(encoded);
    const paths = commit.changes.map((c) => decoder.decode(c.path));
    return this.ctx.storage.transactionSync(() => {
      const at = sql.exec<{ tip: ArrayBuffer; seq: number }>("SELECT tip, seq FROM refs WHERE name = ?", ref).toArray()[0];
      const tip = at ? new Uint8Array(at.tip) : null;
      const parent = commit.parents[0] ?? null;
      const accept = (bytes: Uint8Array, status: "updated" | "rebased", behind: number): PushResult => {
        const id = idOf(bytes);
        const seq = (at?.seq ?? 0) + 1;
        // A ref's first commit brings its parent along as position 0, so that
        // a workspace still on that parent can find itself in the history.
        if (!at && parent) sql.exec("INSERT INTO history (ref, seq, id) VALUES (?, 0, ?)", ref, parent);
        sql.exec("INSERT OR IGNORE INTO commits (id, bytes) VALUES (?, ?)", id, bytes);
        sql.exec("INSERT INTO history (ref, seq, id) VALUES (?, ?, ?)", ref, seq, id);
        for (const path of paths) sql.exec("INSERT INTO changed (ref, path, seq) VALUES (?, ?, ?) ON CONFLICT (ref, path) DO UPDATE SET seq = excluded.seq", ref, path, seq);
        sql.exec("INSERT INTO refs (name, tip, seq) VALUES (?, ?, ?) ON CONFLICT (name) DO UPDATE SET tip = excluded.tip, seq = excluded.seq", ref, id, seq);
        return { status, commit: hex(id), seq, behind, since: after === null ? [] : this.steps(ref, after) };
      };
      // A new ref starts wherever its first commit says.
      if (!tip || !at || (parent && hex(parent) === hex(tip))) return accept(encoded, "updated", 0);

      // The ref has moved on. Where was it when this commit was made?
      let since = 0;
      if (parent) {
        const known = sql.exec<{ seq: number }>("SELECT seq FROM history WHERE ref = ? AND id = ?", ref, parent).toArray()[0];
        if (!known) return { status: "unknown-base", tip: hex(tip) };
        since = known.seq;
      }
      const behind = at.seq - since;
      const clash = this.clashes(ref, paths, since);
      if (clash.length > 0) {
        const id = idOf(encoded);
        sql.exec("INSERT OR IGNORE INTO commits (id, bytes) VALUES (?, ?)", id, encoded);
        sql.exec("INSERT INTO divergent (ref, id, onto, paths) VALUES (?, ?, ?, ?)", ref, id, tip, JSON.stringify(clash));
        return { status: "conflict", commit: hex(id), tip: hex(tip), paths: clash, behind };
      }
      // Nothing in the way: the same changes, on the tip.
      return accept(encode({ ...commit, parents: [tip, ...commit.parents.slice(1)] }), "rebased", behind);
    });
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
