//! A workspace and a server (`server/`): pushing what was committed, and
//! catching up with what others pushed.
//!
//! The server holds two things. File contents, in a bucket, named by hash:
//! anyone may put one at any time. And for each ref, the line of commits it
//! went through: that is the one thing writers have to take turns at, and the
//! server decides the order.
//!
//! `push` sends the workspace's commits that the ref has not seen. If the ref
//! moved in the meantime the server puts the changes on its new tip, provided
//! nobody touched the same paths; the commit then has another parent and so
//! another id, and the workspace takes the server's. `sync` brings the
//! workspace to the ref's tip: it takes the commits (lists of changes, a few
//! hundred bytes each), derives the tree, and fetches and writes only the
//! files that changed inside the workspace's view. What others changed
//! outside the view costs this workspace nothing.
//!
//! A push is one request, and its answer is the catching up. A workspace
//! never walks the ref's history to find where it stands: it asks.

use crate::commit::{Commit, root_of};
use crate::manifest::{Change, Entry, MODE_GITLINK, diff_keeping, expand, in_view, whole_dirs};
use crate::store::{Kind, Store, object_id};
use crate::workspace::{self, Look, Workspace};
use crate::{Id, Result, hex, unhex};
use std::io::Read;

/// A file's content of this size or more is sent to the bucket by itself
/// and not inside a push. A push passes through a Durable Object's worker,
/// which has 128 MB for everything; the bucket takes a stream.
pub const LARGE: usize = 1 << 20;

/// The one client every request goes through: none waits for ever on a server that went quiet.
fn http() -> &'static ureq::Agent {
  static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
  AGENT.get_or_init(|| {
    let mut agent = ureq::AgentBuilder::new();
    if let Some(quiet) = crate::limits::http_quiet() {
      // Each request on a connection of its own: a connection kept for the next request is
      // read without the limit on how long it may stay silent.
      agent = agent
        .timeout_connect(quiet.min(std::time::Duration::from_secs(10)))
        .timeout_read(quiet)
        .timeout_write(quiet)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0);
    }
    agent.build()
  })
}

fn body(response: ureq::Response) -> Result<Vec<u8>> {
  let mut bytes = Vec::new();
  let most = crate::limits::answer_bytes();
  response.into_reader().take(most + 1).read_to_end(&mut bytes).map_err(|e| format!("cannot read the server's answer: {e}"))?;
  if bytes.len() as u64 > most {
    return Err(format!("the server's answer is larger than {} MB, and was not read to its end (NV_ANSWER_MB sets the limit)", most >> 20));
  }
  Ok(bytes)
}

/// `(status, body)`; a refusal by the server is an answer, not an error.
fn send(request: ureq::Request, payload: Option<&[u8]>) -> Result<(u16, Vec<u8>)> {
  let sent = match payload {
    Some(bytes) => request.send_bytes(bytes),
    None => request.call(),
  };
  match sent {
    Ok(response) => Ok((response.status(), body(response)?)),
    Err(ureq::Error::Status(status, response)) => Ok((status, body(response)?)),
    Err(e) => Err(format!("cannot reach the server: {e}")),
  }
}

fn json(bytes: &[u8]) -> Result<serde_json::Value> {
  serde_json::from_slice(bytes).map_err(|e| format!("the server's answer is not JSON: {e}"))
}

fn id_of(value: &serde_json::Value) -> Result<Id> {
  value.as_str().and_then(unhex).and_then(|b| b.try_into().ok()).ok_or_else(|| "the server's answer lacks an id".to_string())
}

/// Nothing is pushed or caught up with while a file still holds both versions from the last
/// sync: a second sync would forget that it does, and what was pushed next would have the
/// markers in it.
fn settled(ws: &Workspace) -> Result<()> {
  let left = workspace::unsettled(ws);
  if left.is_empty() {
    return Ok(());
  }
  Err(format!(
    "these still hold both versions between markers, from the last sync: {}\nEdit them to what they should be and `nv commit`; or `nv restore <dir> before-sync` to undo that sync.",
    left.join(", ")
  ))
}

fn cannot_merge(paths: &[String]) -> String {
  format!(
    "the ref and this workspace both changed, in ways that cannot be merged as text: {}\nNothing was touched. Either make those paths what the ref has and commit, or set this workspace's unpushed work aside with `nv restore <dir> pushed` (it says under which id the work is kept), sync, and make the change again on top.",
    paths.join(", ")
  )
}

/// Two changes get in each other's way when they name the same path, or one
/// names a directory the other is inside of. The server's rule, to the letter.
fn paths_conflict(a: &[u8], b: &[u8]) -> bool {
  let inside = |x: &[u8], y: &[u8]| x.len() > y.len() && x.starts_with(y) && x[y.len()] == b'/';
  a == b || inside(a, b) || inside(b, a)
}

fn remote_of(ws: &Workspace) -> Result<(&str, &str)> {
  match (&ws.remote, &ws.remote_ref) {
    (Some(url), Some(name)) => Ok((url.trim_end_matches('/'), name)),
    _ => Err("this workspace has no remote; set one with `nv remote`".to_string()),
  }
}

fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
  out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
  out.extend_from_slice(bytes);
}

fn frames(mut bytes: &[u8]) -> Result<Vec<&[u8]>> {
  let mut out = Vec::new();
  while !bytes.is_empty() {
    let cut = || "the server's answer is cut short".to_string();
    let len = u32::from_le_bytes(bytes.get(..4).ok_or_else(cut)?.try_into().unwrap()) as usize;
    out.push(bytes.get(4..4 + len).ok_or_else(cut)?);
    bytes = &bytes[4 + len..];
  }
  Ok(out)
}

/// Where the ref is, as far as an answer from the server says.
struct Tip {
  id: Id,
  seq: u64,
}

/// Store the commits an answer carries (`since`), checking each against its id.
fn take_commits(store: &mut Store, url: &str, answer: &serde_json::Value) -> Result<usize> {
  let mut taken = 0;
  let mut dirs: Vec<Id> = Vec::new();
  for step in answer["since"].as_array().into_iter().flatten() {
    let id = id_of(&step["id"])?;
    if store.contains(&id) {
      continue;
    }
    let bytes = step["bytes"].as_str().and_then(unhex).filter(|b| object_id(Kind::Commit, b) == id);
    let bytes = bytes.ok_or_else(|| format!("the server did not hand over commit {}", hex(&id)))?;
    let commit = Commit::decode(&bytes)?;
    // A ref is one line of commits, as the server keeps it: a merge would leave a parent here
    // nobody has, and a history nobody can write out.
    if commit.parents.len() > 1 {
      return Err(format!(
        "the server handed over commit {} with {} parents: a ref holds one line of commits",
        hex(&id),
        commit.parents.len()
      ));
    }
    if let Some(parent) = commit.parents.first().filter(|p| !store.contains(p)) {
      return Err(format!("the ref's history starts from a commit this store does not have ({})", hex(parent)));
    }
    dirs.extend(whole_dirs(&commit.changes).filter(|id| !store.contains(id)));
    store.put(Kind::Commit, &bytes, None)?;
    taken += 1;
  }
  // A commit that sets a whole directory does not list its files: the directories come
  // by themselves, in one answer. The files are fetched like any others, if they are in view.
  if !dirs.is_empty() {
    let asked: String = dirs.iter().map(|id| hex(id) + "\n").collect();
    let (status, answer) = send(http().post(&format!("{url}/fetch/trees")), Some(asked.as_bytes()))?;
    if status != 200 {
      return Err(format!("the server did not hand over a directory a commit names ({status})"));
    }
    for bytes in frames(&answer)? {
      // Read before it is kept: its names are where its files will be written.
      crate::manifest::decode(bytes)?;
      store.put(Kind::Tree, bytes, None)?;
    }
    if let Some(lost) = dirs.iter().find(|id| !store.contains(id)) {
      return Err(format!("the server did not hand over the directory {}", hex(lost)));
    }
    // A directory too deep for any file system to write is refused before anything is written.
    for dir in &dirs {
      crate::manifest::folded_twins(store, *dir, b"")?;
    }
  }
  Ok(taken)
}

/// A ref only goes forward: a tip the server names has to have what this workspace last saw of
/// the ref behind it, whether the tip came with a sync or with the answer to a push.
fn forward(store: &mut Store, seen: Option<Id>, tip: &Id) -> Result<()> {
  let Some(seen) = seen else { return Ok(()) };
  let mut at = Some(*tip);
  for _ in 0..1_000_000 {
    let Some(id) = at else { break };
    if id == seen {
      return Ok(());
    }
    at = match Commit::load(store, &id) {
      Ok(commit) => commit.parents.first().copied(),
      Err(_) => None,
    };
  }
  Err(format!(
    "the server says the ref is at {}, which does not have {} behind it, where this workspace last saw the ref: a ref only goes forward. Nothing was touched.",
    &hex(tip)[..12],
    &hex(&seen)[..12]
  ))
}

/// A workspace that has never looked at the ref asks where it stands: it
/// names its newest commits, and the server answers with the first one the
/// ref's history holds. `Ok(false)` when the ref does not exist yet.
fn find_place(store: &mut Store, ws: &mut Workspace, url: &str, name: &str) -> Result<bool> {
  // The workspace's commit stands on what it last saw of the ref, as a rule. If it was moved
  // elsewhere (a reset to an older commit, another branch), what it "last saw" is not behind it
  // any more: taken as the common point, the difference would read as this workspace having
  // undone the ref's commits. Its place is then asked for again.
  if let Some(pushed) = ws.pushed {
    // Back from the workspace's commit through the commits it made itself: what they stand on
    // has to be what it last saw of the ref. Another commit met first is one the ref brought
    // some other way (a branch that caught up, gone to after this one was taken back).
    let own: std::collections::HashSet<String> = ws.operations().into_iter().map(|(_, _, id)| id).collect();
    let mut at = ws.base;
    let mut stands = false;
    while let Some(id) = at {
      if id == pushed {
        stands = true;
        break;
      }
      if !own.contains(&hex(&id)) {
        break;
      }
      at = Commit::load(store, &id)?.parents.first().copied();
    }
    if !stands {
      (ws.pushed, ws.seq) = (None, 0);
    }
  }
  if ws.pushed.is_some() {
    return Ok(true);
  }
  let (status, _) = send(http().get(&format!("{url}/refs/{name}")), None)?;
  if status == 404 {
    return Ok(false);
  }
  // The workspace's base is usually in the history itself, or a few commits ahead of something that is.
  let mut at = ws.base;
  let mut batch = 64;
  while at.is_some() {
    let mut ids = Vec::with_capacity(batch * 32);
    for _ in 0..batch {
      let Some(id) = at else { break };
      ids.extend_from_slice(&id);
      at = Commit::load(store, &id)?.parents.first().copied();
    }
    let (status, answer) = send(http().post(&format!("{url}/locate/{name}")), Some(&ids))?;
    if status == 200 {
      let answer = json(&answer)?;
      (ws.pushed, ws.seq) = (Some(id_of(&answer["id"])?), answer["seq"].as_u64().unwrap_or(0));
      return Ok(true);
    }
    batch = 4096;
  }
  Err("the ref's history does not reach any commit of this workspace; check out a newer commit".to_string())
}

/// Hand the server a directory: every tree object from `dir` down, in one
/// request. The server works out which files below it has, takes over from
/// the repository the directory was linked from those that one has, and
/// names the rest. Returns how many it took over, and the ids to send.
fn hand_over(store: &mut Store, url: &str, links_root: Option<Id>, path: &[u8], dir: Id) -> Result<(usize, Vec<Id>)> {
  let mut body = Vec::new();
  let mut todo = vec![dir];
  let mut seen = std::collections::HashSet::new();
  while let Some(id) = todo.pop() {
    if !seen.insert(id) {
      continue;
    }
    let bytes = store.get(&id)?.ok_or_else(|| format!("tree {} is not in the store", hex(&id)))?;
    todo.extend(crate::manifest::decode(&bytes)?.values().filter(|e| e.is_dir()).map(|e| e.id));
    frame(&mut body, &bytes);
  }
  let from = match links_root {
    Some(root) => crate::links::source_at(store, root, path)?,
    None => None,
  };
  let query = from.map(|name| format!("?from={name}")).unwrap_or_default();
  let (status, answer) = send(http().post(&format!("{url}/adopt/trees{query}")), Some(&body))?;
  if status != 200 {
    return Err(format!("the server refused a directory ({status}): {}", String::from_utf8_lossy(&answer)));
  }
  let answer = json(&answer)?;
  let missing = answer["missing"].as_array().into_iter().flatten().map(id_of).collect::<Result<Vec<Id>>>()?;
  Ok((answer["adopted"].as_u64().unwrap_or(0) as usize, missing))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pushed {
  /// Commits the ref took as they were, and commits the server moved onto a newer tip.
  pub updated: usize,
  pub rebased: usize,
  pub contents_sent: usize,
  /// Of those, the ones sent to the bucket by themselves, outside the push.
  pub large_sent: usize,
  /// Contents of linked directories that the server took over from the repository they
  /// came from, or had already: named, and not sent.
  pub adopted: usize,
  pub synced: Synced,
}

/// Send the workspace's published commits that the ref has not seen, and
/// catch up with the ref. One request per commit: it carries the commit and
/// the file contents the commit names, and its answer carries what the ref
/// went through meanwhile. On a conflict nothing of the workspace changes
/// and the error names the paths; the server has kept the commit aside.
pub fn push(store: &mut Store, ws: &mut Workspace, author: &str) -> Result<Pushed> {
  settled(ws)?;
  let (url, name) = remote_of(ws)?;
  let (url, name) = (url.to_string(), name.to_string());
  let mut done = Pushed::default();
  let known = find_place(store, ws, &url, &name)?;
  // The published commits the ref has not seen, oldest first. A ref that does
  // not exist yet is started by every commit made since the workspace was.
  let until = if known { ws.pushed } else { ws.start };
  let mut chain: Vec<Commit> = Vec::new();
  let mut at = ws.base;
  while let Some(id) = at.filter(|id| Some(*id) != until) {
    let commit = Commit::load(store, &id)?;
    at = commit.parents.first().copied();
    chain.push(commit);
  }
  if chain.is_empty() {
    done.synced = sync(store, ws, author)?;
    return Ok(done);
  }
  // A file holding both versions between a sync's markers is not published, however it was
  // committed (with `nv git commit` after a restore, the sync's own note is gone).
  for commit in &chain {
    for (path, entry) in &commit.changes {
      let Some(entry) = entry.filter(|e| !e.is_dir() && e.mode != MODE_GITLINK) else { continue };
      let Some(bytes) = store.get(&entry.id)? else { continue };
      let lines = || bytes.split(|&b| b == b'\n');
      if lines().any(|line| line == b"<<<<<<< yours") && lines().any(|line| line == b">>>>>>> theirs") {
        // A commit after it does not take the markers out of the one that has them: the commits
        // not pushed are made one again, with the file settled.
        let back = chain.len();
        return Err(format!(
          "`{}` holds both versions between the markers a sync leaves, in a commit not pushed yet. Nothing was sent.\nA commit on top does not take them out. Commit again, with the file settled: `nv git reset --soft HEAD~{back}` (what the {back} not pushed made becomes changes to commit), edit the file to what it should be, `nv git add` it, `nv git commit`, and push again.",
          String::from_utf8_lossy(path)
        ));
      }
    }
  }
  // A ref is one line of commits. A merge has a second parent the ref would not have, and whoever
  // synced it could not read its history.
  if let Some(merge) = chain.iter().find(|commit| commit.parents.len() > 1) {
    let said = String::from_utf8_lossy(&merge.rest).lines().last().unwrap_or_default().to_string();
    return Err(format!(
      "the commit \"{said}\" is a merge of two lines of commits, and a ref holds one line: it cannot be pushed as it is.\nNothing was sent. Make its changes one ordinary commit on the branch (with `nv git`: `reset --soft` to the commit before the merge, then `commit`), and push again."
    ));
  }
  chain.reverse();
  // A workspace starting a ref may find, by the time its push arrives, that
  // another started it first. Either way the ref and this workspace agree on
  // the commit this work was made on.
  if ws.pushed.is_none() {
    ws.pushed = chain[0].parents.first().copied();
  }
  let seen_before = ws.pushed;
  // Where linked directories came from is in a file of the latest published commit.
  let links_root = match ws.base {
    Some(base) => root_of(store, &base)?,
    None => None,
  };
  // One request for all of them: they land together or not at all, checked
  // against everything the ref took since this workspace last looked.
  let mut body = Vec::new();
  for commit in &chain {
    frame(&mut body, &commit.encode());
  }
  // What goes with the commits: the contents they name, and for a directory one sets
  // whole, the contents below it that the server could not find for itself.
  let mut named: Vec<Id> = Vec::new();
  for (path, entry) in chain.iter().flat_map(|commit| &commit.changes) {
    match entry {
      Some(dir) if dir.is_dir() => {
        let (adopted, missing) = hand_over(store, &url, links_root, path, dir.id)?;
        done.adopted += adopted;
        named.extend(missing);
      }
      Some(entry) if entry.mode != MODE_GITLINK => named.push(entry.id),
      _ => {}
    }
  }
  named.sort_unstable();
  named.dedup();
  for id in named {
    let bytes = store.get(&id)?.ok_or_else(|| format!("blob {} is not in the store", hex(&id)))?;
    if bytes.len() >= LARGE {
      // By itself and ahead of the push: it is streamed into the bucket and never passes
      // through the object that orders the ref, which could not hold it.
      let (status, answer) = send(http().put(&format!("{url}/objects/{}", hex(&id))), Some(&bytes))?;
      if status != 204 {
        return Err(format!("the server refused a large file ({status}): {}", String::from_utf8_lossy(&answer)));
      }
      done.large_sent += 1;
    } else {
      frame(&mut body, &bytes);
    }
    done.contents_sent += 1;
  }
  // From here until an answer is heard, the push may land without this workspace knowing.
  let last = object_id(Kind::Commit, &chain.last().expect("a push carries a commit").encode());
  crate::write_whole(&workspace::unheard(ws), format!("{} {}\n", chain.len(), hex(&last)))?;
  let (_, answer) = send(http().post(&format!("{url}/push/{name}?after={}&commits={}", ws.seq, chain.len())), Some(&body))?;
  let answer = json(&answer)?;
  // Heard: refused, or landed and caught up with below. Known either way, unless catching up fails.
  let note = workspace::unheard(ws);
  let heard = || {
    let _ = std::fs::remove_file(&note);
  };
  match answer["status"].as_str() {
    Some("updated") => done.updated += chain.len(),
    Some("rebased") => done.rebased += chain.len(),
    Some("conflict") => {
      heard();
      let paths: Vec<&str> = answer["paths"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).collect();
      let behind = answer["behind"]
        .as_u64()
        .map(|n| format!(" took {n} {} since this workspace last saw it, and", if n == 1 { "commit" } else { "commits" }))
        .unwrap_or_default();
      return Err(format!(
        "the push was refused: the ref{behind} has changes to the same paths: {}.\n`nv sync` brings them in and merges them with yours; then push again",
        paths.join(", ")
      ));
    }
    Some("unknown-base") => {
      heard();
      return Err("the ref never was at this workspace's base commit".to_string());
    }
    _ => {
      heard();
      return Err(format!("the server refused the push: {answer}"));
    }
  }
  done.synced.commits += take_commits(store, &url, &answer)?;
  let landed = id_of(&answer["commit"])?;
  // What the push landed as stands on where the ref was when the workspace last saw it.
  if let Err(why) = forward(store, seen_before, &landed) {
    (ws.pushed, ws.seq) = (None, 0);
    ws.save()?;
    return Err(format!("the push landed, but {why}"));
  }
  let tip = Some(Tip { id: landed, seq: answer["seq"].as_u64().unwrap_or(0) });
  if let Some(tip) = tip {
    let commits = done.synced.commits;
    let landed_at = tip.seq;
    let arrived = arrive(store, ws, &url, tip, author);
    if arrived.is_err() {
      // The ref has these commits, under ids the server gave them. Where the workspace stands on
      // the ref is asked again next time, and its commits are known there by what they are.
      let _ = landed_at;
      (ws.pushed, ws.seq) = (None, 0);
      ws.save()?;
    }
    done.synced = arrived.map_err(|why| {
      format!(
        "the push landed: its {} are on the ref. Catching up with the ref afterwards did not come about, and the files are as they were:\n{why}",
        if chain.len() == 1 { "commit is".to_string() } else { format!("{} commits are", chain.len()) }
      )
      .replace("commit is are on", "commit is on")
      .replace("commits are are on", "commits are on")
    })?;
    done.synced.commits = commits;
    // Its own commits under the ids the ref gave them are the push itself, not commits dropped.
    done.synced.superseded = 0;
  }
  heard();
  // What was pushed stands on what the last sync brought: there is no sync left to take back.
  workspace::Standing::forget(ws);
  Ok(done)
}

#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct Synced {
  pub commits: usize,
  pub written: usize,
  pub removed: usize,
  pub contents_fetched: usize,
  /// Of those, the ones fetched from the bucket one by one.
  pub large_fetched: usize,
  /// Changes others made outside this workspace's view: known, and not fetched.
  pub outside_view: usize,
  /// Files both sides changed, in different places: theirs and this workspace's, merged.
  pub merged: usize,
  /// Files both sides changed in the same place. Each is left on disk holding both
  /// versions between markers, as a change yet to be recorded.
  pub conflicts: Vec<String>,
  /// The workspace's state before any of this, to go back to: set when something was merged.
  pub before: Option<Id>,
  /// How many commits, published here and not pushed, were put on the tip as one.
  pub folded: usize,
  /// How many commits, published here and not pushed, came to no change between them.
  pub cancelled: usize,
  /// How many commits, published here and not pushed, were dropped because the ref already has
  /// the changes they made, under commits of its own.
  pub superseded: usize,
}

/// Bring the workspace to the ref's tip, keeping its own unpushed work on
/// top. One request for the commits, and one for the contents of the files
/// that changed inside the view, if any did.
pub fn sync(store: &mut Store, ws: &mut Workspace, author: &str) -> Result<Synced> {
  settled(ws)?;
  let (url, name) = remote_of(ws)?;
  let (url, name) = (url.to_string(), name.to_string());
  if !find_place(store, ws, &url, &name)? {
    return Ok(Synced::default());
  }
  resume(store, ws, &url, author)?;
  let (status, answer) = send(http().get(&format!("{url}/since/{name}?after={}", ws.seq)), None)?;
  if status != 200 {
    return Err(format!("the server refused to say where the ref is ({status})"));
  }
  let answer = json(&answer)?;
  let tip = Tip { id: id_of(&answer["tip"])?, seq: answer["seq"].as_u64().unwrap_or(0) };
  if Some(tip.id) == ws.pushed {
    return Ok(Synced::default());
  }
  let commits = take_commits(store, &url, &answer)?;
  forward(store, ws.pushed, &tip.id)?;
  let mut done = arrive(store, ws, &url, tip, author)?;
  done.commits = commits;
  Ok(done)
}

/// Bring contents from the server into the store: `(id, a version of the same
/// file this store has, to store it against)`. One request, and one more for
/// each large one. Returns how many came, and how many of those were large.
fn fetch_contents(store: &mut Store, url: &str, wanted: &[(Id, Option<Id>)]) -> Result<(usize, usize)> {
  let (mut fetched, mut large) = (0, 0);
  if wanted.is_empty() {
    return Ok((0, 0));
  }
  let asked: String = wanted.iter().map(|(id, _)| hex(id) + "\n").collect();
  let (status, answer) = send(http().post(&format!("{url}/fetch/objects")), Some(asked.as_bytes()))?;
  if status != 200 {
    return Err(format!("the server refused to hand over file contents ({status})"));
  }
  let answer = frames(&answer)?;
  let (head, pairs) = answer.split_first().ok_or("the server's answer is empty")?;
  for pair in pairs.chunks(2) {
    let [id, bytes] = pair else { return Err("the server's answer is cut short".to_string()) };
    // Stored under the id its bytes hash to; one that was not asked for is simply never looked up.
    let base = wanted.iter().find(|(w, _)| w[..] == **id).and_then(|(_, base)| *base);
    store.put(Kind::Blob, bytes, base)?;
    fetched += 1;
  }
  // The large ones are not in the answer: each is its own object in the bucket.
  for id in json(head)?["direct"].as_array().into_iter().flatten().filter_map(|id| id.as_str()) {
    // Only what was asked for, by its full name: the id goes into a URL.
    let asked = unhex(id).and_then(|bytes| Id::try_from(bytes).ok()).is_some_and(|id| wanted.iter().any(|(want, _)| *want == id));
    if !asked {
      return Err(format!("the server named {id} as a large file, which was not asked for"));
    }
    let (status, bytes) = send(http().get(&format!("{url}/objects/{id}")), None)?;
    if status != 200 {
      return Err(format!("the server did not hand over the large file {id} ({status})"));
    }
    store.put(Kind::Blob, &bytes, None)?;
    fetched += 1;
    large += 1;
  }
  Ok((fetched, large))
}

/// Writing history into a Git repository needs Git's id of every file of
/// every commit written, and so the content of each file this store never
/// gave a Git id: the versions between the ones a sync brought, and files
/// outside the workspace's view. They are fetched here, if there are any.
pub fn fetch_for_export(store: &mut Store, ws: &Workspace) -> Result<usize> {
  let mut wanted: Vec<(Id, Option<Id>)> = Vec::new();
  let mut seen = std::collections::HashSet::new();
  let mut todo: Vec<Id> = ws.base.into_iter().collect();
  while let Some(at) = todo.pop() {
    // A commit with a Git id had every file below it given one.
    if !seen.insert(at) || store.git_of(&at).is_some() {
      continue;
    }
    let commit = Commit::load(store, &at)?;
    for (_, entry) in &commit.changes {
      let Some(entry) = entry.filter(|e| e.mode != MODE_GITLINK) else { continue };
      let mut files = Vec::new();
      if entry.is_dir() {
        crate::manifest::walk(store, entry.id, &[], &mut |_, file| {
          files.push(file);
          Ok(())
        })?;
      } else {
        files.push(entry);
      }
      for file in files.into_iter().filter(|f| f.mode != MODE_GITLINK) {
        if store.git_of(&file.id).is_none() && !store.contains(&file.id) && seen.insert(file.id) {
          wanted.push((file.id, None));
        }
      }
    }
    todo.extend(commit.parents);
  }
  if wanted.is_empty() {
    return Ok(0);
  }
  let (url, _) = remote_of(ws).map_err(|_| {
    "this store lacks the content of files in the history to write, and the workspace has no server to fetch them from".to_string()
  })?;
  let url = url.to_string();
  let fetched = fetch_contents(store, &url, &wanted)?.0;
  store.flush()?;
  Ok(fetched)
}

fn arriving(ws: &Workspace) -> std::path::PathBuf {
  workspace::arriving(ws)
}

/// Finish an arrival that a killed process left half done. Its files may be
/// partly written, so the disk is not looked at: what the workspace had
/// recorded when that arrival began is what it brings along.
fn resume(store: &mut Store, ws: &mut Workspace, url: &str, author: &str) -> Result<()> {
  let Ok(text) = std::fs::read_to_string(arriving(ws)) else { return Ok(()) };
  let mut fields = text.split_whitespace();
  let tip = fields.next().and_then(unhex).and_then(|bytes| Id::try_from(bytes).ok());
  let seq = fields.next().and_then(|seq| seq.parse().ok());
  match (tip, seq) {
    (Some(id), Some(seq)) if store.get(&id)?.is_some() => arrive_from(store, ws, url, Tip { id, seq }, author, true).map(|_| ()),
    _ => std::fs::remove_file(arriving(ws)).map_err(|e| e.to_string()),
  }
}

/// Move the workspace to `tip`, whose commits the store already has.
fn arrive(store: &mut Store, ws: &mut Workspace, url: &str, tip: Tip, author: &str) -> Result<Synced> {
  resume(store, ws, url, author)?;
  arrive_from(store, ws, url, tip, author, false)
}

const MODE_KIND: u32 = 0o170000;
const MODE_FILE: u32 = 0o100000;

fn arrive_from(store: &mut Store, ws: &mut Workspace, url: &str, tip: Tip, author: &str, resuming: bool) -> Result<Synced> {
  let mut done = Synced::default();
  let Tip { id: tip, seq } = tip;
  // What is on disk first, so that nothing unrecorded is written over.
  if !resuming {
    workspace::snapshot(store, ws, author, Look::Everywhere)?;
  }
  // Where the workspace stands now, all of it: what `before-sync` goes back to.
  let before = ws.head;
  let standing = workspace::Standing { head: ws.head, base: ws.base, pushed: ws.pushed, seq: ws.seq };

  // Three states and the point they last agreed on: what the ref has (`tip`), what this
  // workspace published and has not pushed (`base`), and its files as recorded (`head`).
  // A workspace that just started the ref agrees with it on its own commit.
  let common = ws.pushed.or(ws.base);
  let root = |store: &mut Store, id: Option<Id>| match id {
    Some(id) => root_of(store, &id),
    None => Ok(None),
  };
  // Commits of this workspace's own that the ref has though the workspace never heard so (a push
  // killed after it landed): the ref's commits since the common point, in order, that are these
  // commits again (the same message, the same changes) on whatever the server put them on. The
  // last of them is where the two agree now, and only what came after is this workspace's to merge.
  let (mine_since, own_landed, own_on_ref) = {
    let walk = |store: &mut Store, from: Option<Id>| -> Result<Vec<Id>> {
      let mut out = Vec::new();
      let mut at = from;
      while let Some(id) = at.filter(|id| Some(*id) != common) {
        out.push(id);
        at = Commit::load(store, &id)?.parents.first().copied();
        if out.len() > 100_000 {
          break;
        }
      }
      out.reverse();
      Ok(out)
    };
    let (mine, refs) = (walk(store, ws.base)?, walk(store, Some(tip))?);
    let key = |store: &mut Store, id: &Id| -> Result<(Vec<u8>, Vec<Change>)> {
      let commit = Commit::load(store, id)?;
      Ok((commit.rest, commit.changes))
    };
    let mut matched: Option<(Id, Id)> = None;
    let mut their_copies: Vec<Change> = Vec::new();
    let mut from_ref = 0;
    let mut count = 0;
    'mine: for own in &mine {
      let wanted = key(store, own)?;
      while from_ref < refs.len() {
        let candidate = refs[from_ref];
        from_ref += 1;
        if candidate == *own || key(store, &candidate)? == wanted {
          their_copies.extend(wanted.1.iter().cloned());
          matched = Some((*own, candidate));
          count += 1;
          continue 'mine;
        }
      }
      break;
    }
    (mine.len(), matched.map(|(own, on_ref)| (own, on_ref, count)), their_copies)
  };
  // What this workspace has to put on the tip starts after the last of those; and what they
  // changed, the ref has from this workspace, not from anyone else.
  let mut published_from = None;
  if let Some((own, _, _)) = own_landed {
    published_from = Some(root(store, Some(own))?);
  }
  let from = root(store, common)?;
  let (tip_root, base_root, head_root) = (root(store, Some(tip))?, root(store, ws.base)?, root(store, ws.head)?);
  // A directory either side set whole, and that still is that directory, stays one change.
  let mut whole = workspace::whole_since(store, Some(tip), common)?;
  whole.extend(workspace::whole_since(store, ws.head, common)?);
  let mut theirs = diff_keeping(store, from, tip_root, &whole)?;
  if published_from.is_some() {
    // A change of this workspace's landed commits, as the ref still has it, is no change of theirs.
    theirs.retain(|change| !own_on_ref.contains(change));
  }
  // And what it published since is merged on what those commits left, not on the old common
  // point: a line it changed again (or back) after them is its change, not a match for theirs.
  // Where they changed a file, nobody else did before they landed, or they would not have; where
  // they did not, what they left is what the old common point had.
  let merge_base = published_from.unwrap_or(from);
  let mut published = diff_keeping(store, merge_base, base_root, &whole)?;
  let _ = mine_since;
  // What the files have become since the last commit made here: kept apart from that commit all
  // the way through. Merged with theirs as if it were committed, it would come out committed.
  let unpublished = diff_keeping(store, base_root, head_root, &whole)?;
  // The commits made here and not yet on the ref.
  let still_mine = mine_since - own_landed.map_or(0, |(_, _, count)| count);
  let chain = |_: &mut Store| -> Result<usize> { Ok(still_mine) };
  if published.is_empty() && ws.base != common && Some(tip) != ws.base {
    done.cancelled = chain(store)?;
  }
  // What the ref has that this workspace published is this workspace's own work, landed: not a
  // change of theirs.
  let landed: Vec<Change> = published.iter().filter(|p| theirs.contains(p)).cloned().collect();
  published.retain(|p| !landed.contains(p));
  theirs.retain(|t| !landed.contains(t));
  if !published.is_empty() {
    // Commits published here and not pushed come out of this as one commit: said, not done silently.
    done.folded = chain(store)?;
  } else if !landed.is_empty() && ws.base != common && Some(tip) != ws.base {
    // The ref has the same changes, and not these commits: they are dropped, and that is said too.
    let mut on_the_ref = false;
    let mut at = Some(tip);
    while let Some(id) = at.filter(|id| Some(*id) != common) {
      if Some(id) == ws.base {
        on_the_ref = true;
        break;
      }
      at = Commit::load(store, &id)?.parents.first().copied();
    }
    if !on_the_ref {
      done.superseded = chain(store)?;
    }
  }

  let file = |entry: &Option<Entry>| entry.filter(|e| e.mode & MODE_KIND == MODE_FILE);
  let at = |store: &mut Store, root: Option<Id>, path: &[u8]| -> Result<Option<Entry>> {
    Ok(match root {
      Some(root) => crate::manifest::lookup(store, root, path)?,
      None => None,
    })
  };
  // The contents this store lacks: what is to be written inside the view, and theirs of every
  // file that has to be merged. One request.
  // Nothing too long or too deep for this file system, from where the workspace is, before a
  // directory set whole is so much as opened: checked at every arrival, not only when its
  // commits first came, so a refusal is a refusal each time and never a crash or half a write.
  let room = crate::manifest::MAX_PATH.saturating_sub(std::fs::canonicalize(&ws.root).map_or(0, |r| r.as_os_str().len() + 1) + 1);
  for (path, entry) in &theirs {
    if path.len() > room {
      return Err(format!(
        "the ref has `{}…`, a path longer than this file system takes here ({room} bytes, below {}). Nothing was touched; a workspace nearer the top of the disk could hold it.",
        String::from_utf8_lossy(&path[..60.min(path.len())]),
        ws.root.display()
      ));
    }
    if let Some(dir) = entry.filter(|e| e.is_dir()) {
      crate::manifest::folded_twins_within(store, dir.id, path, room)
        .map_err(|why| format!("{why}: longer than this file system takes here ({room} bytes). Nothing was touched."))?;
    }
  }
  let theirs_files = workspace::removals_first(expand(store, from, &theirs)?);
  // Where this file system takes two names for one, a directory of the ref's that has both cannot
  // be written: one would land on the other, or be written through it.
  if ws.root.join(".NV").join("workspace").is_file() {
    let mut dirs: Vec<&[u8]> = theirs_files
      .iter()
      .filter(|(_, entry)| entry.is_some())
      .map(|(path, _)| path.iter().rposition(|&b| b == b'/').map_or(&path[..0], |at| &path[..at]))
      .collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
      if let Some((a, b)) = crate::manifest::dir_twins(store, tip_root, dir)? {
        return Err(format!(
          "the ref has both `{}` and `{}`, which this file system takes for one name. Nothing was touched; one of them has to be renamed on the ref, from a workspace on a file system that tells them apart.",
          String::from_utf8_lossy(&a),
          String::from_utf8_lossy(&b)
        ));
      }
    }
  }
  let index = ws.load_index();
  let mut wanted: Vec<(Id, Option<Id>)> = Vec::new();
  for (path, entry) in &theirs_files {
    if !in_view(&ws.prefixes, path, false) {
      done.outside_view += 1;
    } else if let Some(entry) = entry.filter(|e| e.mode != MODE_GITLINK && !store.contains(&e.id)) {
      wanted.push((entry.id, index.get(path).map(|known| known.entry.id)));
    }
  }
  for (path, entry) in &theirs {
    let touched = published.iter().chain(&unpublished).any(|(mine, _)| mine == path);
    if let Some(entry) = file(entry).filter(|e| touched && !store.contains(&e.id) && !wanted.iter().any(|(id, _)| *id == e.id)) {
      wanted.push((entry.id, None));
    }
  }
  drop(index);
  let (fetched, large) = fetch_contents(store, url, &wanted)?;
  done.contents_fetched += fetched;
  done.large_fetched += large;

  // One file, three ways. `Ok(None)`: not text, not to be merged.
  let content = |store: &mut Store, entry: Option<Entry>| -> Result<std::sync::Arc<Vec<u8>>> {
    match entry {
      Some(entry) => store.get(&entry.id)?.ok_or_else(|| format!("the content {} is missing", hex(&entry.id))),
      None => Ok(Default::default()),
    }
  };
  let three_way = |store: &mut Store, was: Option<Entry>, ours: Entry, theirs: Entry| -> Result<Option<(Vec<u8>, bool, u32)>> {
    // The mode is merged as the text is: whoever changed it from what both had, has it.
    let mode = match was {
      Some(was) if was.mode == ours.mode => theirs.mode,
      _ => ours.mode,
    };
    let (was, ours, theirs) = (content(store, was.filter(|e| !e.is_dir()))?, content(store, Some(ours))?, content(store, Some(theirs))?);
    if ours.contains(&0) || theirs.contains(&0) {
      return Ok(None);
    }
    let (text, clean) = crate::gitcli::merge_text_named(&was, &ours, &theirs, "yours", "theirs");
    Ok(Some((text, clean, mode)))
  };
  let mut clash: Vec<String> = Vec::new();
  let name = |path: &[u8]| String::from_utf8_lossy(path).into_owned();

  // First what was published here against what the ref took: commit against commit.
  let mut merged_published: std::collections::HashMap<Vec<u8>, Entry> = Default::default();
  let mut unsettled: std::collections::HashSet<Vec<u8>> = Default::default();
  for (path, ours) in &published {
    match theirs.iter().find(|(other, _)| other == path) {
      Some((_, their)) => match (file(ours), file(their)) {
        (Some(ours), Some(their)) => {
          let was = at(store, merge_base, path)?;
          match three_way(store, was, ours, their)? {
            Some((text, true, mode)) => {
              merged_published.insert(path.clone(), Entry { mode, id: store.put(Kind::Blob, &text, Some(ours.id))? });
            }
            Some(_) => {
              unsettled.insert(path.clone());
            }
            None => clash.push(name(path)),
          }
        }
        _ => clash.push(name(path)),
      },
      None if theirs.iter().any(|(other, _)| paths_conflict(path, other)) => clash.push(name(path)),
      None => {}
    }
  }
  // What this workspace's commit is, put on the tip: its changes, the merged ones merged. A file
  // left for a person is out of it: there the workspace is on what the ref has.
  let had_some = !published.is_empty();
  let published: Vec<Change> = published
    .into_iter()
    .filter(|(path, _)| !unsettled.contains(path))
    .map(|(path, entry)| match merged_published.get(&path) {
      Some(merged) => (path, Some(*merged)),
      None => (path, entry),
    })
    // Merged, a change may come to exactly what the ref has: then it is no change to put on the tip.
    .filter(|(path, entry)| theirs.iter().find(|(other, _)| other == path).is_none_or(|(_, their)| their != entry))
    .collect();
  if had_some && published.is_empty() && unsettled.is_empty() {
    (done.superseded, done.folded) = (done.folded, 0);
  }
  // What the new commit has at a path, where that is not what the old one had.
  let moved = |path: &[u8]| -> Option<Option<Entry>> {
    if let Some(merged) = merged_published.get(path) {
      return Some(Some(*merged));
    }
    theirs.iter().find(|(other, _)| other == path).map(|(_, entry)| *entry)
  };

  // Then what the files have become since, onto that: each change against the commit it was made on.
  enum Outcome {
    /// The file is to hold this, and it differs from the new commit by it.
    Pending(Option<Entry>),
    /// Both versions between markers, for a person.
    Both(Vec<u8>, u32),
  }
  let mut outcome: std::collections::HashMap<Vec<u8>, Outcome> = Default::default();
  let mut merges = 0;
  for (path, now) in &unpublished {
    if unsettled.contains(path) {
      continue;
    }
    let Some(new) = moved(path) else {
      // The commit below it did not change at this path: the change stands as it is.
      if theirs.iter().any(|(other, _)| paths_conflict(path, other)) {
        clash.push(name(path));
      }
      outcome.insert(path.clone(), Outcome::Pending(*now));
      continue;
    };
    if *now == new {
      continue;
    }
    let was = at(store, base_root, path)?;
    match (file(now), file(&new)) {
      (Some(ours), Some(their)) => match three_way(store, was, ours, their)? {
        Some((text, true, mode)) => {
          let merged = Entry { mode, id: store.put(Kind::Blob, &text, Some(ours.id))? };
          merges += 1;
          if Some(merged) != new {
            outcome.insert(path.clone(), Outcome::Pending(Some(merged)));
          }
        }
        Some((text, false, mode)) => {
          outcome.insert(path.clone(), Outcome::Both(text, mode));
        }
        None => clash.push(name(path)),
      },
      _ => clash.push(name(path)),
    }
  }
  // A file the two commits changed in the same lines: both versions, the workspace's being the
  // file as it is now.
  for path in &unsettled {
    let (was, their) = (at(store, merge_base, path)?, moved(path).flatten());
    let ours = file(&at(store, head_root, path)?).or(file(&at(store, base_root, path)?));
    match (ours, their) {
      (Some(ours), Some(their)) => match three_way(store, was, ours, their)? {
        Some((text, _, mode)) => {
          outcome.insert(path.clone(), Outcome::Both(text, mode));
        }
        None => clash.push(name(path)),
      },
      _ => clash.push(name(path)),
    }
  }
  if !clash.is_empty() {
    clash.sort();
    clash.dedup();
    return Err(cannot_merge(&clash));
  }
  done.merged = merged_published.len() + merges;

  // From here files are written. Should this process be killed, the next one finishes the arrival
  // from what is recorded, and does not take half-written files for the workspace's own work.
  store.flush()?;
  if !resuming {
    standing.keep(ws)?;
  }
  crate::write_whole(&arriving(ws), format!("{} {seq}\n", hex(&tip)))?;
  let mut index = ws.load_index();
  let mut placed = workspace::Placed::default();
  let mut pending: Vec<Change> = Vec::new();
  // The paths the files may have to change at: where the commit below them moved, and where a merge was made.
  let mut paths: Vec<(Vec<u8>, Option<Entry>)> = theirs_files.clone();
  for path in outcome.keys() {
    if !paths.iter().any(|(other, _)| other == path) {
      let new = moved(path).unwrap_or(at(store, base_root, path)?);
      paths.push((path.clone(), new));
    }
  }
  for (path, new) in workspace::removals_first(paths) {
    if !in_view(&ws.prefixes, &path, false) {
      continue;
    }
    // The merged commit's, where the two commits were merged in this file.
    let new = merged_published.get(&path).map_or(new, |merged| Some(*merged));
    let on_disk = at(store, head_root, &path)?;
    match outcome.remove(&path) {
      // Both versions between markers, on disk and not recorded: theirs is what the workspace is on.
      Some(Outcome::Both(text, mode)) => {
        let file = ws.root.join(workspace::to_path(&path));
        if let Some(parent) = file.parent() {
          std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&file, text).map_err(|e| e.to_string())?;
        // The text is for a person to settle; the mode is settled already, and is on the file.
        use std::os::unix::fs::PermissionsExt;
        let bits = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(bits)).map_err(|e| e.to_string())?;
        match new {
          Some(new) => index.insert(path.clone(), crate::index::Indexed { entry: new, size: 0, mtime: 0, ctime: 0 }),
          None => index.remove(&path),
        }
        done.conflicts.push(name(&path));
        continue;
      }
      Some(Outcome::Pending(wanted)) => {
        // The file as this workspace has it (merged with theirs, if theirs changed it): a
        // difference from the new commit, as it was from the old.
        if wanted != on_disk {
          match wanted {
            Some(entry) => placed.add(path.clone(), workspace::place(store, &ws.root, &path, entry)?),
            None => {
              workspace::unplace(&ws.root, &path);
              index.remove(&path);
            }
          }
        }
        if wanted != new {
          pending.push((path.clone(), wanted));
        }
        continue;
      }
      None => {}
    }
    if new == on_disk {
      continue;
    }
    match new {
      Some(entry) => {
        if entry.mode != MODE_GITLINK && !store.contains(&entry.id) {
          return Err(format!("the server did not hand over the content of {}", name(&path)));
        }
        placed.add(path.clone(), workspace::place(store, &ws.root, &path, entry)?);
        done.written += 1;
      }
      None => {
        workspace::unplace(&ws.root, &path);
        index.remove(&path);
        done.removed += 1;
      }
    }
  }
  // Changes whose paths were not walked above (outside the view there are none of the workspace's).
  for (path, outcome) in outcome {
    if let Outcome::Pending(wanted) = outcome {
      pending.push((path, wanted));
    }
  }
  pending.sort_by(|a, b| a.0.cmp(&b.0));
  // This workspace's own work, now on top of the tip: what it had published
  // but not pushed stays one published commit, with the message it had, and
  // what it had only recorded stays recorded on top of that.
  let base = match (published.is_empty(), ws.base) {
    (false, Some(was)) => {
      let rest = Commit::load(store, &was)?.rest;
      let id = Commit { parents: vec![tip], rest, changes: published.clone() }.store(store)?;
      root_of(store, &id)?;
      // A commit of this workspace's own, like the one it stands for: logged once it is stored.
      store.flush()?;
      ws.log("commit", &id)?;
      id
    }
    _ => tip,
  };
  let head = if pending.is_empty() { base } else { workspace::carry(store, base, pending, author)? };
  store.flush()?;
  let based = ws.base;
  (ws.pushed, ws.seq, ws.base, ws.head) = (Some(tip), seq, Some(base), Some(head));
  placed.into_index(&mut index);
  ws.adopt(&mut index)?;
  workspace::take_branch_along(store, ws, based)?;
  workspace::Standing::arrived_at(ws, base);
  let _ = std::fs::remove_file(arriving(ws));
  if done.merged > 0 || !done.conflicts.is_empty() {
    done.before = before;
  }
  done.conflicts.sort();
  workspace::set_unsettled(ws, &done.conflicts)?;
  // A push that was not heard back from, found on the ref: the workspace knows now.
  if let Some((sent, last)) = workspace::unheard_push(ws)
    && let Some((own, _, count)) = own_landed
    && (count >= sent || own == last)
  {
    let _ = std::fs::remove_file(workspace::unheard(ws));
  }
  Ok(done)
}
