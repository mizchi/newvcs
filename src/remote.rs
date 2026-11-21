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

fn body(response: ureq::Response) -> Result<Vec<u8>> {
  let mut bytes = Vec::new();
  response.into_reader().read_to_end(&mut bytes).map_err(|e| format!("cannot read the server's answer: {e}"))?;
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

fn cannot_merge(paths: &[String]) -> String {
  format!(
    "the ref and this workspace both changed, in ways that cannot be merged as text: {}\nNothing was touched. Name your state (`nv mark <dir> mine`), give up your change to those paths, sync, and make the change again on top; `nv restore <dir> mine` goes back to where you were.",
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
    let (status, answer) = send(ureq::post(&format!("{url}/fetch/trees")), Some(asked.as_bytes()))?;
    if status != 200 {
      return Err(format!("the server did not hand over a directory a commit names ({status})"));
    }
    for bytes in frames(&answer)? {
      store.put(Kind::Tree, bytes, None)?;
    }
    if let Some(lost) = dirs.iter().find(|id| !store.contains(id)) {
      return Err(format!("the server did not hand over the directory {}", hex(lost)));
    }
  }
  Ok(taken)
}

/// A workspace that has never looked at the ref asks where it stands: it
/// names its newest commits, and the server answers with the first one the
/// ref's history holds. `Ok(false)` when the ref does not exist yet.
fn find_place(store: &mut Store, ws: &mut Workspace, url: &str, name: &str) -> Result<bool> {
  if ws.pushed.is_some() {
    return Ok(true);
  }
  let (status, _) = send(ureq::get(&format!("{url}/refs/{name}")), None)?;
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
    let (status, answer) = send(ureq::post(&format!("{url}/locate/{name}")), Some(&ids))?;
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
  let (status, answer) = send(ureq::post(&format!("{url}/adopt/trees{query}")), Some(&body))?;
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
  chain.reverse();
  // A workspace starting a ref may find, by the time its push arrives, that
  // another started it first. Either way the ref and this workspace agree on
  // the commit this work was made on.
  if ws.pushed.is_none() {
    ws.pushed = chain[0].parents.first().copied();
  }
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
      let (status, answer) = send(ureq::put(&format!("{url}/objects/{}", hex(&id))), Some(&bytes))?;
      if status != 204 {
        return Err(format!("the server refused a large file ({status}): {}", String::from_utf8_lossy(&answer)));
      }
      done.large_sent += 1;
    } else {
      frame(&mut body, &bytes);
    }
    done.contents_sent += 1;
  }
  let (_, answer) = send(ureq::post(&format!("{url}/push/{name}?after={}&commits={}", ws.seq, chain.len())), Some(&body))?;
  let answer = json(&answer)?;
  match answer["status"].as_str() {
    Some("updated") => done.updated += chain.len(),
    Some("rebased") => done.rebased += chain.len(),
    Some("conflict") => {
      let paths: Vec<&str> = answer["paths"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).collect();
      return Err(format!(
        "the ref has changes to the same paths: {}.\n`nv sync` brings them in and merges them with yours; then push again",
        paths.join(", ")
      ));
    }
    Some("unknown-base") => return Err("the ref never was at this workspace's base commit".to_string()),
    _ => return Err(format!("the server refused the push: {answer}")),
  }
  done.synced.commits += take_commits(store, &url, &answer)?;
  let tip = Some(Tip { id: id_of(&answer["commit"])?, seq: answer["seq"].as_u64().unwrap_or(0) });
  if let Some(tip) = tip {
    let commits = done.synced.commits;
    done.synced = arrive(store, ws, &url, tip, author)?;
    done.synced.commits = commits;
  }
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
}

/// Bring the workspace to the ref's tip, keeping its own unpushed work on
/// top. One request for the commits, and one for the contents of the files
/// that changed inside the view, if any did.
pub fn sync(store: &mut Store, ws: &mut Workspace, author: &str) -> Result<Synced> {
  let (url, name) = remote_of(ws)?;
  let (url, name) = (url.to_string(), name.to_string());
  if !find_place(store, ws, &url, &name)? {
    return Ok(Synced::default());
  }
  resume(store, ws, &url, author)?;
  let (status, answer) = send(ureq::get(&format!("{url}/since/{name}?after={}", ws.seq)), None)?;
  if status != 200 {
    return Err(format!("the server refused to say where the ref is ({status})"));
  }
  let answer = json(&answer)?;
  let tip = Tip { id: id_of(&answer["tip"])?, seq: answer["seq"].as_u64().unwrap_or(0) };
  if Some(tip.id) == ws.pushed {
    return Ok(Synced::default());
  }
  let commits = take_commits(store, &url, &answer)?;
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
  let (status, answer) = send(ureq::post(&format!("{url}/fetch/objects")), Some(asked.as_bytes()))?;
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
    let (status, bytes) = send(ureq::get(&format!("{url}/objects/{id}")), None)?;
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
  ws.root.join(".nv").join("arriving")
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

/// A file both sides changed: one text holding both changes, or, where they changed
/// the same lines, both versions between markers and the entry the ref has.
enum Merged {
  Clean(Entry),
  Both(Vec<u8>, Entry),
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

  // Both sides as changes since the point they last agreed on.
  // A workspace that just started the ref agrees with it on its own commit.
  let common = ws.pushed.or(ws.base);
  let root = |store: &mut Store, id: Option<Id>| match id {
    Some(id) => root_of(store, &id),
    None => Ok(None),
  };
  let from = root(store, common)?;
  let (theirs_root, published_root, mine_root) = (root(store, Some(tip))?, root(store, ws.base)?, root(store, ws.head)?);
  // A directory either side set whole, and that still is that directory, stays one change.
  let mut whole = workspace::whole_since(store, Some(tip), common)?;
  whole.extend(workspace::whole_since(store, ws.head, common)?);
  let mut theirs = diff_keeping(store, from, theirs_root, &whole)?;
  let mut mine = diff_keeping(store, from, mine_root, &whole)?;
  let mut published = diff_keeping(store, from, published_root, &whole)?;
  // What both did alike is nobody's change: after a push, that is this workspace's own work.
  let same: Vec<Change> = mine.iter().filter(|m| theirs.contains(m)).cloned().collect();
  mine.retain(|m| !same.contains(m));
  published.retain(|p| !same.contains(p));
  theirs.retain(|t| !same.contains(t));
  // A file both changed is merged, as text, the way Git merges it. Anything else both
  // touched (one removed it, one made it a directory, a link) is for a person to decide.
  let file = |entry: &Option<Entry>| entry.filter(|e| e.mode & MODE_KIND == MODE_FILE);
  let both: Vec<(Vec<u8>, Entry, Entry)> = mine
    .iter()
    .filter_map(|(path, ours)| {
      let (_, theirs) = theirs.iter().find(|(theirs, _)| theirs == path)?;
      Some((path.clone(), file(ours)?, file(theirs)?))
    })
    .collect();
  let clash: Vec<String> = mine
    .iter()
    .filter(|(m, _)| !both.iter().any(|(path, _, _)| path == m) && theirs.iter().any(|(t, _)| paths_conflict(m, t)))
    .map(|(m, _)| String::from_utf8_lossy(m).into_owned())
    .collect();
  if !clash.is_empty() {
    return Err(cannot_merge(&clash));
  }

  // On disk a directory is its files.
  let theirs = workspace::removals_first(expand(store, from, &theirs)?);
  // Their changes onto the disk, inside the view only. The contents this
  // store lacks come in one request.
  let mut index = ws.load_index();
  let mut placed = workspace::Placed::default();
  let mut wanted: Vec<(Id, Option<Id>)> = Vec::new();
  for (path, entry) in &theirs {
    if !in_view(&ws.prefixes, path, false) {
      done.outside_view += 1;
    } else if let Some(entry) = entry.filter(|e| e.mode != MODE_GITLINK && !store.contains(&e.id)) {
      wanted.push((entry.id, index.get(path).map(|known| known.entry.id)));
    }
  }
  for (_, ours, theirs) in &both {
    if !store.contains(&theirs.id) && !wanted.iter().any(|(id, _)| *id == theirs.id) {
      wanted.push((theirs.id, Some(ours.id)));
    }
  }
  let (fetched, large) = fetch_contents(store, url, &wanted)?;
  done.contents_fetched += fetched;
  done.large_fetched += large;
  // The files both changed, merged before anything is written: one that cannot be merged stops the sync here.
  let mut merges: std::collections::HashMap<Vec<u8>, Merged> = Default::default();
  let mut binary = Vec::new();
  for (path, ours, theirs) in &both {
    let content =
      |store: &mut Store, id: &Id| store.get(id)?.ok_or_else(|| format!("the content of {} is missing", String::from_utf8_lossy(path)));
    let base = match from {
      Some(root) => crate::manifest::lookup(store, root, path)?.filter(|e| !e.is_dir()),
      None => None,
    };
    let base = match base {
      Some(base) => content(store, &base.id)?,
      None => Default::default(),
    };
    let (ours_bytes, theirs_bytes) = (content(store, &ours.id)?, content(store, &theirs.id)?);
    if ours_bytes.contains(&0) || theirs_bytes.contains(&0) {
      binary.push(String::from_utf8_lossy(path).into_owned());
      continue;
    }
    let (text, clean) = crate::gitcli::merge_text_named(&base, &ours_bytes, &theirs_bytes, "yours", "theirs");
    merges.insert(
      path.clone(),
      if clean {
        Merged::Clean(Entry { mode: ours.mode, id: store.put(Kind::Blob, &text, Some(ours.id))? })
      } else {
        Merged::Both(text, *theirs)
      },
    );
  }
  if !binary.is_empty() {
    return Err(cannot_merge(&binary));
  }
  // From here files are written. Should this process be killed, the next one finishes the arrival
  // from what is recorded, and does not take half-written files for the workspace's own work.
  store.flush()?;
  if !resuming {
    standing.keep(ws)?;
  }
  crate::write_whole(&arriving(ws), format!("{} {seq}\n", hex(&tip)))?;
  for (path, entry) in &theirs {
    if !in_view(&ws.prefixes, path, false) {
      continue;
    }
    match merges.get(path) {
      // Theirs and this workspace's in one file: that is what the workspace now has changed.
      Some(Merged::Clean(merged)) => {
        placed.add(path.clone(), workspace::place(store, &ws.root, path, *merged)?);
        for side in [&mut mine, &mut published] {
          side.iter_mut().filter(|(p, _)| p == path).for_each(|(_, entry)| *entry = Some(*merged));
        }
        done.merged += 1;
        continue;
      }
      // Both versions between markers, on disk and not recorded: theirs is what the workspace is on.
      Some(Merged::Both(text, theirs)) => {
        std::fs::write(ws.root.join(workspace::to_path(path)), text).map_err(|e| e.to_string())?;
        index.insert(path.clone(), crate::index::Indexed { entry: *theirs, size: 0, mtime: 0 });
        mine.retain(|(p, _)| p != path);
        published.retain(|(p, _)| p != path);
        done.conflicts.push(String::from_utf8_lossy(path).into_owned());
        continue;
      }
      None => {}
    }
    match entry {
      Some(entry) => {
        if entry.mode != MODE_GITLINK && !store.contains(&entry.id) {
          return Err(format!("the server did not hand over the content of {}", String::from_utf8_lossy(path)));
        }
        placed.add(path.clone(), workspace::place(store, &ws.root, path, *entry)?);
        done.written += 1;
      }
      None => {
        workspace::unplace(&ws.root, path);
        index.remove(path);
        done.removed += 1;
      }
    }
  }
  // This workspace's own work, now on top of the tip: what it had published
  // but not pushed stays one published commit, with the message it had, and
  // what it had only recorded stays recorded on top of that.
  let base = match (published.is_empty(), ws.base) {
    (false, Some(was)) => {
      let rest = Commit::load(store, &was)?.rest;
      let id = Commit { parents: vec![tip], rest, changes: published.clone() }.store(store)?;
      root_of(store, &id)?;
      id
    }
    _ => tip,
  };
  let unpublished: Vec<Change> = mine.into_iter().filter(|m| !published.contains(m)).collect();
  let head = if unpublished.is_empty() { base } else { workspace::carry(store, base, unpublished, author)? };
  store.flush()?;
  (ws.pushed, ws.seq, ws.base, ws.head) = (Some(tip), seq, Some(base), Some(head));
  placed.into_index(&mut index);
  ws.adopt(&mut index)?;
  let _ = std::fs::remove_file(arriving(ws));
  if done.merged > 0 || !done.conflicts.is_empty() {
    done.before = before;
  }
  workspace::set_unsettled(ws, &done.conflicts)?;
  Ok(done)
}
