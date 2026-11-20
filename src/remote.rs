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
use crate::manifest::{Change, MODE_GITLINK, diff, in_view};
use crate::store::{Kind, Store, object_id};
use crate::workspace::{self, Look, Workspace};
use crate::{Id, Result, hex, unhex};
use std::io::Read;

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
fn take_commits(store: &mut Store, answer: &serde_json::Value) -> Result<usize> {
  let mut taken = 0;
  for step in answer["since"].as_array().into_iter().flatten() {
    let id = id_of(&step["id"])?;
    if store.contains(&id) {
      continue;
    }
    let bytes = step["bytes"].as_str().and_then(unhex).filter(|b| object_id(Kind::Commit, b) == id);
    let bytes = bytes.ok_or_else(|| format!("the server did not hand over commit {}", hex(&id)))?;
    if let Some(parent) = Commit::decode(&bytes)?.parents.first().filter(|p| !store.contains(p)) {
      return Err(format!("the ref's history starts from a commit this store does not have ({})", hex(parent)));
    }
    store.put(Kind::Commit, &bytes, None)?;
    taken += 1;
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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pushed {
  /// Commits the ref took as they were, and commits the server moved onto a newer tip.
  pub updated: usize,
  pub rebased: usize,
  pub contents_sent: usize,
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
  // The published commits the ref has not seen, oldest first. On a ref that
  // does not exist yet, the newest one alone starts it.
  let mut chain: Vec<Commit> = Vec::new();
  let mut at = ws.base;
  while let Some(id) = at.filter(|id| Some(*id) != ws.pushed) {
    let commit = Commit::load(store, &id)?;
    at = commit.parents.first().copied();
    chain.push(commit);
    if !known {
      break;
    }
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
  let mut onto: Option<Id> = None;
  let mut tip = None;
  for mut commit in chain {
    // The commit before this one may have been given another id by the server.
    if let (Some(parent), Some(first)) = (onto, commit.parents.first_mut()) {
      *first = parent;
    }
    let mut body = Vec::new();
    frame(&mut body, &commit.encode());
    for (_, entry) in &commit.changes {
      if let Some(entry) = entry.filter(|e| e.mode != MODE_GITLINK) {
        let bytes = store.get(&entry.id)?.ok_or_else(|| format!("blob {} is not in the store", hex(&entry.id)))?;
        frame(&mut body, &bytes);
        done.contents_sent += 1;
      }
    }
    let (_, answer) = send(ureq::post(&format!("{url}/push/{name}?after={}", ws.seq)), Some(&body))?;
    let answer = json(&answer)?;
    match answer["status"].as_str() {
      Some("updated") => done.updated += 1,
      Some("rebased") => done.rebased += 1,
      Some("conflict") => {
        let paths: Vec<&str> = answer["paths"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).collect();
        return Err(format!("the ref has changes to the same paths: {}. `nv sync` to see them", paths.join(", ")));
      }
      Some("unknown-base") => return Err("the ref never was at this workspace's base commit".to_string()),
      _ => return Err(format!("the server refused the push: {answer}")),
    }
    let id = id_of(&answer["commit"])?;
    onto = Some(id);
    done.synced.commits += take_commits(store, &answer)?;
    tip = Some(Tip { id, seq: answer["seq"].as_u64().unwrap_or(0) });
  }
  if let Some(tip) = tip {
    let commits = done.synced.commits;
    done.synced = arrive(store, ws, &url, tip, author)?;
    done.synced.commits = commits;
  }
  Ok(done)
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Synced {
  pub commits: usize,
  pub written: usize,
  pub removed: usize,
  pub contents_fetched: usize,
  /// Changes others made outside this workspace's view: known, and not fetched.
  pub outside_view: usize,
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
  let (status, answer) = send(ureq::get(&format!("{url}/since/{name}?after={}", ws.seq)), None)?;
  if status != 200 {
    return Err(format!("the server refused to say where the ref is ({status})"));
  }
  let answer = json(&answer)?;
  let tip = Tip { id: id_of(&answer["tip"])?, seq: answer["seq"].as_u64().unwrap_or(0) };
  if Some(tip.id) == ws.pushed {
    return Ok(Synced::default());
  }
  let commits = take_commits(store, &answer)?;
  let mut done = arrive(store, ws, &url, tip, author)?;
  done.commits = commits;
  Ok(done)
}

/// Move the workspace to `tip`, whose commits the store already has.
fn arrive(store: &mut Store, ws: &mut Workspace, url: &str, tip: Tip, author: &str) -> Result<Synced> {
  let mut done = Synced::default();
  let Tip { id: tip, seq } = tip;
  // What is on disk first, so that nothing unrecorded is written over.
  workspace::snapshot(store, ws, author, Look::Everywhere)?;

  // Both sides as changes since the point they last agreed on.
  // A workspace that just started the ref agrees with it on its own commit.
  let common = ws.pushed.or(ws.base);
  let root = |store: &mut Store, id: Option<Id>| match id {
    Some(id) => root_of(store, &id),
    None => Ok(None),
  };
  let from = root(store, common)?;
  let (theirs_root, published_root, mine_root) = (root(store, Some(tip))?, root(store, ws.base)?, root(store, ws.head)?);
  let mut theirs = diff(store, from, theirs_root)?;
  let mut mine = diff(store, from, mine_root)?;
  let mut published = diff(store, from, published_root)?;
  // What both did alike is nobody's change: after a push, that is this workspace's own work.
  let same: Vec<Change> = mine.iter().filter(|m| theirs.contains(m)).cloned().collect();
  mine.retain(|m| !same.contains(m));
  published.retain(|p| !same.contains(p));
  theirs.retain(|t| !same.contains(t));
  let clash: Vec<String> = mine
    .iter()
    .filter(|(m, _)| theirs.iter().any(|(t, _)| paths_conflict(m, t)))
    .map(|(m, _)| String::from_utf8_lossy(m).into_owned())
    .collect();
  if !clash.is_empty() {
    return Err(format!("the ref and this workspace both changed: {}", clash.join(", ")));
  }

  // Their changes onto the disk, inside the view only. The contents this
  // store lacks come in one request.
  let mut index = ws.load_index();
  let mut wanted: Vec<(Id, Option<Id>)> = Vec::new();
  for (path, entry) in &theirs {
    if !in_view(&ws.prefixes, path, false) {
      done.outside_view += 1;
    } else if let Some(entry) = entry.filter(|e| e.mode != MODE_GITLINK && !store.contains(&e.id)) {
      wanted.push((entry.id, index.get(path).map(|known| known.entry.id)));
    }
  }
  if !wanted.is_empty() {
    let asked: String = wanted.iter().map(|(id, _)| hex(id) + "\n").collect();
    let (status, answer) = send(ureq::post(&format!("{url}/fetch/objects")), Some(asked.as_bytes()))?;
    if status != 200 {
      return Err(format!("the server refused to hand over file contents ({status})"));
    }
    for pair in frames(&answer)?.chunks(2) {
      let [id, bytes] = pair else { return Err("the server's answer is cut short".to_string()) };
      // Stored under the id its bytes hash to; one that was not asked for is simply never looked up.
      let base = wanted.iter().find(|(w, _)| w[..] == **id).and_then(|(_, base)| *base);
      store.put(Kind::Blob, bytes, base)?;
      done.contents_fetched += 1;
    }
  }
  for (path, entry) in &theirs {
    if !in_view(&ws.prefixes, path, false) {
      continue;
    }
    match entry {
      Some(entry) => {
        if entry.mode != MODE_GITLINK && !store.contains(&entry.id) {
          return Err(format!("the server did not hand over the content of {}", String::from_utf8_lossy(path)));
        }
        let indexed = workspace::place(store, &ws.root, path, *entry)?;
        index.insert(path.clone(), indexed);
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
  ws.adopt(&mut index)?;
  Ok(done)
}
