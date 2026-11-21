//! Repositories under one namespace, and directories shared between them.
//!
//! Every repository in a store has a name, `/<owner>/<repo>`, which is a ref
//! to its latest commit. A directory's id says everything below it, so a
//! directory of one repository can stand in another simply by being there
//! with the same id: nothing is stored twice, a checkout of part of the
//! tree stays sound, and to Git it is an ordinary directory.
//!
//! Where a directory came from is not in the tree. It is in `nv.links`, a
//! tracked file that names the repository, the commit and the directory
//! within it, the way a lock file names versions. Following a repository
//! is resolving its name again and committing whatever differs.

use std::path::Path;

use crate::commit::root_of;
use crate::manifest::{Change, Entry, MODE_DIR, diff, lookup};
use crate::store::{Kind, Store};
use crate::workspace::{self, Look, Workspace};
use crate::{Id, Result, hex, unhex};

const FILE: &str = "nv.links";

fn valid(name: &str) -> bool {
  let part = |p: &str| !p.is_empty() && p != "." && p != ".." && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
  match name.strip_prefix('/').map(|rest| rest.split('/').collect::<Vec<_>>()).as_deref() {
    Some([owner, repo]) => part(owner) && part(repo),
    _ => false,
  }
}

/// Give a commit a repository's name, or move the name to it.
pub fn name(store: &Store, name: &str, commit: Id) -> Result<()> {
  if !valid(name) {
    return Err(format!("`{name}` is not a repository's name: /<owner>/<repo>"));
  }
  store.set_ref(&format!("repos{name}"), commit)
}

/// Every repository in the store, by name.
pub fn names(store: &Store) -> Vec<(String, Id)> {
  let owners = std::fs::read_dir(store.dir.join("refs").join("repos")).into_iter().flatten().flatten();
  let mut found: Vec<(String, Id)> = owners
    .filter_map(|owner| owner.file_name().into_string().ok())
    .flat_map(|owner| store.refs_under(&format!("repos/{owner}")).into_iter().map(move |(repo, id)| (format!("/{owner}/{repo}"), id)))
    .collect();
  found.sort();
  found
}

/// One id for the state of every repository: derived from the names, never
/// stored, and never something a write has to wait for.
pub fn everything(names: &[(String, Id)]) -> Id {
  let mut hasher = blake3::Hasher::new();
  for (name, id) in names {
    hasher.update(name.as_bytes());
    hasher.update(&[0]);
    hasher.update(id);
  }
  *hasher.finalize().as_bytes()
}

/// A line of `nv.links`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Link {
  /// Where the directory stands in this repository.
  pub path: String,
  pub name: String,
  pub commit: Id,
  /// The directory within that repository; empty for all of it.
  pub sub: String,
  /// Git's id of that commit, for a reader who has only Git. Empty if it has none.
  pub git: String,
}

fn read(root: &Path) -> Vec<Link> {
  parsed(&std::fs::read_to_string(root.join(FILE)).unwrap_or_default())
}

fn parsed(text: &str) -> Vec<Link> {
  let line = |line: &str| {
    let mut fields = line.split('\t');
    let (path, name, commit) = (fields.next()?, fields.next()?, fields.next()?);
    let (sub, git) = (fields.next().unwrap_or(""), fields.next().unwrap_or(""));
    Some(Link { path: path.into(), name: name.into(), commit: unhex(commit)?.try_into().ok()?, sub: sub.into(), git: git.into() })
  };
  text.lines().filter(|l| !l.starts_with('#')).filter_map(line).collect()
}

fn written(links: &[Link]) -> Vec<u8> {
  let mut text = String::from(
    "# Directories that come from other repositories: path, repository, commit, directory within it, Git's id of the commit.\n",
  );
  for link in links {
    text.push_str(&format!("{}\t{}\t{}\t{}\t{}\n", link.path, link.name, hex(&link.commit), link.sub, link.git));
  }
  text.into_bytes()
}

/// The repository the directory at `path` was linked from, by the links file of the tree `root`.
pub fn source_at(store: &mut Store, root: Id, path: &[u8]) -> Result<Option<String>> {
  let Some(file) = lookup(store, root, FILE.as_bytes())?.filter(|e| !e.is_dir()) else {
    return Ok(None);
  };
  let text = store.get(&file.id)?.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
  Ok(parsed(&text).into_iter().find(|link| link.path.as_bytes() == path).map(|link| link.name).filter(|name| valid(name)))
}

/// The id of a directory of a commit.
fn dir_of(store: &mut Store, commit: &Id, sub: &str) -> Result<Id> {
  let root = root_of(store, commit)?.ok_or("that commit has no files")?;
  if sub.is_empty() {
    return Ok(root);
  }
  match lookup(store, root, sub.as_bytes())? {
    Some(entry) if entry.mode == MODE_DIR => Ok(entry.id),
    Some(_) => Err(format!("`{sub}` is a file there, not a directory")),
    None => Err(format!("there is no `{sub}` in that repository")),
  }
}

/// What stands at `path` in the workspace's latest state, if it is a directory.
fn standing(store: &mut Store, ws: &Workspace, path: &str) -> Result<Option<Id>> {
  let Some(root) = (match ws.head {
    Some(head) => root_of(store, &head)?,
    None => None,
  }) else {
    return Ok(None);
  };
  match lookup(store, root, path.as_bytes())? {
    Some(entry) if entry.mode == MODE_DIR => Ok(Some(entry.id)),
    Some(_) => Err(format!("`{path}` is a file here")),
    None => Ok(None),
  }
}

/// The paths that differ on disk from what is recorded: one walk, for however many links are asked about.
fn differing(ws: &Workspace) -> Result<Vec<Vec<u8>>> {
  Ok(workspace::status(ws, Look::Everywhere)?.0.into_iter().map(|(path, _, _)| path).collect())
}

fn changed_here(differing: &[Vec<u8>], path: &str) -> bool {
  let below = format!("{path}/");
  differing.iter().any(|changed| changed.starts_with(below.as_bytes()))
}

/// Put a directory of the repository `name` at `path`, as it is at the
/// repository's latest commit, and write the link down. Linking again
/// follows the repository. Only the files that differ are written; nothing
/// of the directory is stored again. Returns how many paths changed.
pub fn link(store: &mut Store, ws: &mut Workspace, path: &str, name: &str, sub: &str, author: &str) -> Result<usize> {
  // A place inside the workspace, named from its top: not above it, not from the root of
  // the file system, and not in the directory the workspace keeps its own state in.
  if path.starts_with('/') {
    return Err(format!("`{path}` is not a path inside the workspace: give it from the workspace's top"));
  }
  let path = String::from_utf8_lossy(&ws.inside(path.as_bytes())?).into_owned();
  let (path, sub) = (path.as_str(), sub.trim_matches('/'));
  let commit = store
    .read_ref(&format!("repos{name}"))
    .filter(|_| valid(name))
    .ok_or_else(|| format!("there is no repository named `{name}` in this store"))?;
  let theirs = dir_of(store, &commit, sub)?;
  if changed_here(&differing(ws)?, path) {
    return Err(format!("`{path}` has changes made here that linking would write over: commit or discard them first"));
  }
  let here = standing(store, ws, path)?;
  let paths = diff(store, here, Some(theirs))?.len();
  // One change, whatever the directory holds: this path is that directory.
  let mut changes: Vec<Change> = vec![(path.as_bytes().to_vec(), Some(Entry { mode: MODE_DIR, id: theirs }))];
  let mut links = read(&ws.root);
  links.retain(|link| link.path != path);
  let git = store.git_of(&commit).map(|oid| hex(&oid)).unwrap_or_default();
  links.push(Link { path: path.into(), name: name.into(), commit, sub: sub.into(), git });
  links.sort_by(|a, b| a.path.cmp(&b.path));
  let listed = store.put(Kind::Blob, &written(&links), None)?;
  changes.push((FILE.as_bytes().to_vec(), Some(Entry { mode: 0o100644, id: listed })));
  workspace::take_changes(store, ws, changes, author)?;
  // The proof that nothing was copied: the directory here is the directory there.
  debug_assert_eq!(standing(store, ws, path)?, Some(theirs));
  Ok(paths)
}

/// How a link stands.
pub struct Standing {
  pub link: Link,
  /// Whether this store has the commit the link names. Without it nothing below can be said.
  pub known: bool,
  /// The directory is what the link says: nobody changed it here.
  pub as_linked: bool,
  /// The repository's name still points at the commit the link names.
  pub up_to_date: bool,
}

pub fn links(store: &mut Store, ws: &Workspace) -> Result<Vec<Standing>> {
  let mut out = Vec::new();
  let differing = differing(ws)?;
  for link in read(&ws.root) {
    let known = store.get(&link.commit)?.is_some();
    let wanted = if known { dir_of(store, &link.commit, &link.sub).ok() } else { None };
    let as_linked = wanted.is_some() && standing(store, ws, &link.path)? == wanted && !changed_here(&differing, &link.path);
    let up_to_date = store.read_ref(&format!("repos{}", link.name)) == Some(link.commit);
    out.push(Standing { link, known, as_linked, up_to_date });
  }
  Ok(out)
}

/// Stop following: the directory stays, as ordinary files of this
/// repository, and its line leaves the links file.
pub fn unlink(store: &mut Store, ws: &mut Workspace, path: &str, author: &str) -> Result<()> {
  let path = path.trim_matches('/');
  let mut links = read(&ws.root);
  let before = links.len();
  links.retain(|link| link.path != path);
  if links.len() == before {
    return Err(format!("`{path}` is not a linked directory: `nv links <dir>` lists them"));
  }
  let entry = if links.is_empty() { None } else { Some(Entry { mode: 0o100644, id: store.put(Kind::Blob, &written(&links), None)? }) };
  workspace::take_changes(store, ws, vec![(FILE.as_bytes().to_vec(), entry)], author)?;
  Ok(())
}

/// Follow every link whose repository has moved on. Refused, with nothing
/// touched, if one of them was changed here. Returns `(path, paths changed)`.
pub fn update(store: &mut Store, ws: &mut Workspace, author: &str) -> Result<Vec<(String, usize)>> {
  // A repository this store does not have cannot be followed from here, and is left alone.
  let behind: Vec<Standing> = links(store, ws)?
    .into_iter()
    .filter(|standing| standing.known && !standing.up_to_date && store.read_ref(&format!("repos{}", standing.link.name)).is_some())
    .collect();
  if let Some(changed) = behind.iter().find(|standing| !standing.as_linked) {
    let Link { path, name, sub, .. } = &changed.link;
    return Err(format!(
      "`{path}` was changed here since it was linked: following its repository would write over that.\nTo take the repository's directory in place of what is here: nv link <dir> {path} {name} {sub}\nTo keep what is here and stop following: nv unlink <dir> {path}"
    ));
  }
  let mut done = Vec::new();
  for standing in behind {
    let Link { path, name, sub, .. } = standing.link;
    let paths = link(store, ws, &path, &name, &sub, author)?;
    done.push((path, paths));
  }
  Ok(done)
}
