//! A server that lies, in front of the real one: what the client takes from an answer it must
//! not take. Needs the server running, as `tests/remote.rs` does:
//!
//!   NV_SERVER=http://localhost:8799 cargo test --test lying_server
//!
//! The lying server passes every request on to the real one, and changes the answers it is
//! told to change. Without `NV_SERVER` the test says so and passes.

mod common;

use newvcs::commit::Commit;
use newvcs::manifest::{Dir, Entry, MODE_DIR};
use newvcs::store::{Kind, object_id};
use newvcs::{Id, hex};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

const NV: &str = env!("CARGO_BIN_EXE_nv");

/// `(method, path, status, body)` to the answer to send instead, or `None` to send it as it is.
type Lie = Box<dyn Fn(&str, &str, u16, &[u8]) -> Option<(u16, Vec<u8>)> + Send>;

/// A server on a port of its own, passing requests on to `real` and answering as `lie` says.
fn lying(real: String, lie: Arc<Mutex<Lie>>) -> String {
  let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  let address = format!("http://{}", listener.local_addr().unwrap());
  std::thread::spawn(move || {
    for stream in listener.incoming().flatten() {
      let (real, lie) = (real.clone(), lie.clone());
      std::thread::spawn(move || {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
          return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
        let mut length = 0usize;
        loop {
          let mut header = String::new();
          if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
          }
          if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
          {
            length = value.trim().parse().unwrap_or(0);
          }
        }
        let mut body = vec![0; length];
        let _ = reader.read_exact(&mut body);
        let request = ureq::request(&method, &format!("{real}{path}"));
        let answer = if method == "GET" { request.call() } else { request.send_bytes(&body) };
        let (status, bytes) = match answer {
          Ok(r) | Err(ureq::Error::Status(_, r)) => {
            let status = r.status();
            let mut bytes = Vec::new();
            let _ = r.into_reader().read_to_end(&mut bytes);
            (status, bytes)
          }
          Err(_) => (502, Vec::new()),
        };
        let (status, bytes) = (lie.lock().unwrap())(&method, &path, status, &bytes).unwrap_or((status, bytes));
        let mut stream = stream;
        let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
        let _ = stream.write_all(&bytes);
      });
    }
  });
  address
}

fn sh(dir: &Path, script: &str) -> Output {
  let mut c = Command::new("sh");
  c.arg("-c").arg(script).current_dir(dir).env("NV", NV).env("NV_HTTP_TIMEOUT", "20");
  c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null");
  for (key, value) in
    [("AUTHOR_NAME", "A"), ("AUTHOR_EMAIL", "a@example.com"), ("COMMITTER_NAME", "C"), ("COMMITTER_EMAIL", "c@example.com")]
  {
    c.env(format!("GIT_{key}"), value);
  }
  common::bounded(&mut c)
}

fn text(out: &Output) -> String {
  format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn ok(dir: &Path, script: &str) -> String {
  let out = sh(dir, script);
  assert!(out.status.success(), "`{script}` failed:\n{}", text(&out));
  String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn code_of(dir: &Path, script: &str) -> (i32, String) {
  let out = sh(dir, script);
  (out.status.code().unwrap_or(-1), text(&out))
}

/// A directory with a seed repository, and a function making a workspace of it on `url`.
fn setup(name: &str) -> Option<(PathBuf, String)> {
  let Ok(real) = std::env::var("NV_SERVER") else {
    eprintln!("NV_SERVER is not set: the lying server needs a real one behind it");
    return None;
  };
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("lying-{name}"));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  ok(&dir, "git init -q -b main seed && cd seed && echo base > README && git add . && git commit -qm seed");
  let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  Some((dir, format!("{real}/t/lying-{name}-{stamp}")))
}

fn workspace(dir: &Path, name: &str, url: &str) {
  ok(
    dir,
    &format!(
      "$NV import seed st-{name} >/dev/null && $NV checkout st-{name} HEAD {name} >/dev/null && $NV remote {name} {url} main && $NV sync {name} >/dev/null"
    ),
  );
}

fn tip(url: &str) -> (String, u64) {
  let text = ureq::get(&format!("{url}/refs/main")).call().unwrap().into_string().unwrap();
  let answer: serde_json::Value = serde_json::from_str(&text).unwrap();
  (answer["tip"].as_str().unwrap().to_string(), answer["seq"].as_u64().unwrap())
}

fn honest() -> Lie {
  Box::new(|_, _, _, _| None)
}

#[test]
fn an_answer_to_a_push_that_moves_the_ref_back_is_not_taken() {
  let Some((dir, real)) = setup("push-back") else { return };
  let lie = Arc::new(Mutex::new(honest()));
  let through = lying(real.split("/t/").next().unwrap().to_string(), lie.clone());
  let url = format!("{through}/t/{}", real.split("/t/").nth(1).unwrap());
  workspace(&dir, "a", &real);
  ok(&dir, "echo 1 > a/k1 && $NV commit a k1 >/dev/null && $NV push a >/dev/null");
  let (first, _) = tip(&real);
  ok(&dir, "echo 2 > a/k2 && $NV commit a k2 >/dev/null && $NV push a >/dev/null");
  workspace(&dir, "b", &url);
  assert_eq!(ok(&dir, "cat b/k2"), "2");
  ok(&dir, "echo mine > b/mine && $NV commit b mine >/dev/null");
  // The push lands; the answer says the ref is where it was two commits ago.
  *lie.lock().unwrap() = Box::new(move |method, path, status, bytes| {
    if method != "POST" || !path.contains("/push/") || status != 200 {
      return None;
    }
    let mut answer: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    answer["commit"] = serde_json::json!(first);
    answer["commits"] = serde_json::json!([first]);
    answer["seq"] = serde_json::json!(1);
    answer["since"] = serde_json::json!([]);
    Some((200, serde_json::to_vec(&answer).unwrap()))
  });
  let (code, said) = code_of(&dir, "$NV push b 2>&1");
  assert_ne!(code, 0, "{said}");
  assert!(said.contains("only goes forward"), "{said}");
  assert_eq!(ok(&dir, "cat b/k2"), "2", "the other writer's file is where it was");
  assert_eq!(ok(&dir, "cat b/mine"), "mine");
  // Told the truth again, the workspace goes on.
  *lie.lock().unwrap() = honest();
  ok(&dir, "$NV sync b >/dev/null");
  assert_eq!(ok(&dir, "cat b/k2; cat b/mine"), "2\nmine");
}

#[test]
fn a_tree_too_deep_is_refused_at_every_sync_and_the_workspace_goes_on() {
  let Some((dir, real)) = setup("deep") else { return };
  let lie = Arc::new(Mutex::new(honest()));
  let base = real.split("/t/").next().unwrap().to_string();
  let through = lying(base, lie.clone());
  let url = format!("{through}/t/{}", real.split("/t/").nth(1).unwrap());
  workspace(&dir, "a", &real);
  ok(&dir, "echo 1 > a/k1 && $NV commit a k1 >/dev/null && $NV push a >/dev/null");
  workspace(&dir, "b", &url);
  // A commit on the ref's tip setting `deep` to a chain of 20,000 directories.
  let (at, seq) = tip(&real);
  let leaf: Id = object_id(Kind::Blob, b"x\n");
  let mut trees: Vec<Vec<u8>> = Vec::new();
  let mut below = newvcs::manifest::encode(&Dir::from([(b"f".to_vec(), Entry { mode: 0o100644, id: leaf })]));
  for _ in 0..20_000 {
    let id = object_id(Kind::Tree, &below);
    trees.push(below);
    below = newvcs::manifest::encode(&Dir::from([(b"d".to_vec(), Entry { mode: MODE_DIR, id })]));
  }
  let top = object_id(Kind::Tree, &below);
  trees.push(below);
  let parent: Id = newvcs::unhex(&at).unwrap().try_into().unwrap();
  let commit = Commit {
    parents: vec![parent],
    rest: b"author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\ndeep\n".to_vec(),
    changes: vec![(b"deep".to_vec(), Some(Entry { mode: MODE_DIR, id: top }))],
  };
  let bytes = commit.encode();
  let id = hex(&object_id(Kind::Commit, &bytes));
  let framed: Vec<u8> = trees.iter().flat_map(|t| [(t.len() as u32).to_le_bytes().to_vec(), t.clone()].concat()).collect();
  let (id2, framed) = (id.clone(), Arc::new(framed));
  *lie.lock().unwrap() = Box::new(move |method, path, _, _| {
    if method == "GET" && path.contains("/since/") {
      let step = serde_json::json!({ "seq": seq + 1, "id": id2, "bytes": hex(&bytes) });
      return Some((200, serde_json::to_vec(&serde_json::json!({ "tip": id2, "seq": seq + 1, "since": [step] })).unwrap()));
    }
    if method == "GET" && path.ends_with("/refs/main") {
      return Some((200, serde_json::to_vec(&serde_json::json!({ "tip": id2, "seq": seq + 1 })).unwrap()));
    }
    if method == "POST" && path.contains("/fetch/trees") {
      return Some((200, framed.to_vec()));
    }
    None
  });
  for round in 0..2 {
    let (code, said) = code_of(&dir, "$NV sync b 2>&1");
    assert_eq!(code, 2, "round {round}: {said}");
    assert!(said.contains("longer than"), "round {round}: {said}");
  }
  // Nothing was begun that has to be finished: the workspace records and commits as before.
  ok(&dir, "echo more >> b/README && $NV commit b more >/dev/null");
  assert!(!dir.join("b/deep").exists());
}

#[test]
fn a_path_too_long_for_where_the_workspace_is_is_refused_before_anything_is_written() {
  let Some((dir, real)) = setup("long") else { return };
  workspace(&dir, "a", &real);
  ok(&dir, "echo 1 > a/k1 && $NV commit a k1 >/dev/null && $NV push a >/dev/null");
  workspace(&dir, "b", &real);
  // A path the server takes (under 1,024 bytes) that this workspace cannot hold, being where it is.
  let root = std::fs::canonicalize(dir.join("b")).unwrap();
  let length = 1030 - root.as_os_str().len().min(500);
  let mut path = String::new();
  while path.len() + 101 < length {
    path.push_str(&"p".repeat(100));
    path.push('/');
  }
  path.push_str(&"f".repeat(length - path.len()));
  let content = b"long\n";
  let (at, _) = tip(&real);
  let commit = Commit {
    parents: vec![newvcs::unhex(&at).unwrap().try_into().unwrap()],
    rest: b"author t <t@example.com> 1 +0000\ncommitter t <t@example.com> 1 +0000\n\nlong\n".to_vec(),
    changes: vec![(path.clone().into_bytes(), Some(Entry { mode: 0o100644, id: object_id(Kind::Blob, content) }))],
  };
  let mut body = Vec::new();
  for part in [commit.encode(), content.to_vec()] {
    body.extend_from_slice(&(part.len() as u32).to_le_bytes());
    body.extend_from_slice(&part);
  }
  assert_eq!(ureq::post(&format!("{real}/push/main?commits=1")).send_bytes(&body).map(|r| r.status()).unwrap_or(0), 200);
  for round in 0..2 {
    let (code, said) = code_of(&dir, "$NV sync b 2>&1");
    assert_eq!(code, 2, "round {round}: {said}");
    assert!(said.contains("longer than"), "round {round}: {said}");
  }
  ok(&dir, "echo more >> b/README && $NV commit b more >/dev/null");
}

#[test]
fn an_answer_larger_than_the_limit_is_not_read_to_its_end() {
  let Some((dir, real)) = setup("large") else { return };
  let lie = Arc::new(Mutex::new(honest()));
  let through = lying(real.split("/t/").next().unwrap().to_string(), lie.clone());
  let url = format!("{through}/t/{}", real.split("/t/").nth(1).unwrap());
  workspace(&dir, "a", &real);
  ok(&dir, "echo 1 > a/k1 && $NV commit a k1 >/dev/null && $NV push a >/dev/null");
  workspace(&dir, "b", &url);
  *lie.lock().unwrap() = Box::new(|method, path, _, _| (method == "GET" && path.contains("/since/")).then(|| (200, vec![b' '; 3 << 20])));
  let (code, said) = code_of(&dir, "NV_ANSWER_MB=1 $NV sync b 2>&1");
  assert_eq!(code, 2, "{said}");
  assert!(said.contains("larger than 1 MB"), "{said}");
}
