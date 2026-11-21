use newvcs::store::Store;
use newvcs::workspace::{self, Look, Workspace};
use newvcs::{Result, git, hex, remote};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
nv: an experiment in keeping Git's object ids without Git's storage

  nv import <git-repo> <store> [rev=HEAD] [max-commits]
        Import history, checking that every commit maps back to its Git id
  nv verify <store>
        Recompute every imported commit's Git id from the store alone
  nv checkout <store> <rev> <dir> [prefix...]
        Write the files under the prefixes (all, if none) as a workspace
  nv status <dir>
  nv snapshot <dir> [--recorded]
        Record what is on disk as a commit nobody else sees. With --recorded,
        look only at the paths given to `nv record`
  nv record <dir> <path>...
        Say that these paths were written (for a tool that knows its writes)
  nv commit <dir> <message> [--recorded]
        Publish the snapshots since the last commit as one commit; the rest
        of the tree is not needed
  nv watch <dir> [--start | --stop | --status]
        Stay, and snapshot whatever is written there by whatever writes it:
        the operating system reports the writes, so nothing has to. With
        --start it stays after this command returns, and is started again
        by a hook or a command with --recorded should it ever be found gone
  nv log <dir>
        The workspace's operations
  nv restore <dir> <state> [--recorded]
        Make the files what they were at a recorded state: a name given with
        `nv mark`, `base`, `~N` (N snapshots or commits ago), or the start of
        an id. What is on disk is recorded first, so nothing is lost by going
  nv mark <dir> <name> [--recorded]
        Record what is on disk and name that state, e.g. `green`
  nv since <dir> <state> [--recorded]
        The paths that differ between a recorded state and the files now.
        With --recorded these three look only at the paths given to
        `nv record` (and at what a restore would overwrite), not at the tree
  nv hook claude
        Read a Claude Code PostToolUse event on stdin and record what the
        tool wrote. `nv agent-init claude` prints the settings that run it
  nv remote <dir> <url-of-repository> <ref>
        Point the workspace at a server, e.g. http://localhost:8799/myrepo main
  nv push <dir>
        Send the published commits the ref has not seen, then catch up with it
  nv sync <dir>
        Bring the workspace to the ref's tip, keeping its own work on top
  nv fork <dir> <new-dir>
        Clone a workspace copy-on-write, build output included
  nv export <dir> <git-repo> <ref>
        Write the workspace's commit into a Git repository
  nv adopt <dir>
        Make an nv workspace of a Git repository, in place: both then answer
        for the same files, and Git is stopped from moving on its own
  nv check <dir>
        Say whether Git and nv are in step there, and if not, who moved
  nv reconcile <dir>
        Put them back in step, whichever moved
  nv shell-init
        A shell function that sends `git` to nv inside a workspace
  nv stats <store>
  nv git <command> [<args>]
        The everyday Git commands, on an nv store (see tools/git-shim)
";

const AUTHOR: &str = "nv <nv@localhost>";

/// What to put in `.claude/settings.json` so that every write a tool makes is recorded.
const CLAUDE_HOOKS: &str = r#"{
  "hooks": {
    "PostToolUse": [
      { "matcher": "Edit|Write|MultiEdit|NotebookEdit", "hooks": [{ "type": "command", "command": "nv hook claude" }] },
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "nv hook claude" }] }
    ]
  }
}"#;

/// A Claude Code `PostToolUse` event: record what the tool wrote. A tool
/// that names the file it wrote costs a look at that file; one that does not
/// (a shell command) costs a walk. Whatever goes wrong, the tool is not
/// failed for it: this is called for every tool call there is.
fn claude_hook() -> Option<()> {
  let mut input = String::new();
  std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).ok()?;
  let event: serde_json::Value = serde_json::from_str(&input).ok()?;
  let text = |value: &serde_json::Value| value.as_str().map(PathBuf::from);
  let written = text(&event["tool_input"]["file_path"]).or_else(|| text(&event["tool_input"]["notebook_path"]));
  let cwd = text(&event["cwd"]).or_else(|| std::env::current_dir().ok())?;
  let written = written.map(|path| if path.is_absolute() { path } else { cwd.join(path) });
  // The workspace the file is in, or the one the tool ran in.
  let from = written.as_ref().and_then(|path| path.parent().map(Path::to_path_buf)).unwrap_or(cwd);
  let root = from.ancestors().find(|dir| dir.join(".nv").join("workspace").is_file())?.to_path_buf();
  // A workspace that is meant to be watched and is not gets its watcher back here: this runs on every tool call.
  newvcs::watch::ensure(&root);
  let _lock = workspace::lock(&root).ok()?;
  let mut ws = Workspace::open(&root).ok()?;
  let mut store = Store::open(&ws.store).ok()?;
  let look = match written.as_ref().and_then(|path| path.strip_prefix(&root).ok()) {
    Some(relative) => {
      ws.record(relative.as_os_str().as_encoded_bytes()).ok()?;
      Look::Recorded
    }
    None => Look::Everywhere,
  };
  workspace::snapshot(&mut store, &mut ws, AUTHOR, look).ok()?;
  Some(())
}

fn run(args: &[String]) -> Result<()> {
  let arg = |i: usize| args.get(i).map(String::as_str).ok_or_else(|| USAGE.to_string());
  let started = Instant::now();
  let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
  // `--recorded` after the arguments: the writer reported every write, so the tree need not be walked.
  let look = |at: usize| if args.get(at).is_some_and(|a| a == "--recorded") { Look::Recorded } else { Look::Everywhere };
  match arg(0)? {
    "import" => {
      let mut store = Store::open(Path::new(arg(2)?))?;
      let limit = args.get(4).and_then(|n| n.parse().ok());
      let done = git::import(&mut store, Path::new(arg(1)?), args.get(3).map_or("HEAD", String::as_str), limit)?;
      println!(
        "imported {} commits and {} blobs ({} commits were already there) in {:.0} ms",
        done.commits,
        done.blobs,
        done.already,
        ms(started)
      );
      print_stats(&store);
      println!("git ids reproduced: {} of {}", done.commits - done.mismatches.len(), done.commits);
      for m in done.mismatches.iter().take(10) {
        println!("  MISMATCH {m}");
      }
      if !done.mismatches.is_empty() {
        return Err(format!("{} commits did not map back to their git ids", done.mismatches.len()));
      }
    }
    "verify" => {
      let mut store = Store::open(Path::new(arg(1)?))?;
      let (checked, wrong) = git::verify(&mut store)?;
      println!("{checked} commits recomputed from the store in {:.0} ms, {} with a different git id", ms(started), wrong.len());
      if !wrong.is_empty() {
        return Err(format!("first mismatch: {}", wrong[0]));
      }
    }
    "checkout" => {
      let mut store = Store::open(Path::new(arg(1)?))?;
      let commit = store.resolve(arg(2)?).ok_or_else(|| format!("unknown revision `{}`", args[2]))?;
      let prefixes = args[4.min(args.len())..].iter().map(|p| p.trim_matches('/').as_bytes().to_vec()).collect();
      let done = workspace::checkout(&mut store, commit, Path::new(arg(3)?), prefixes)?;
      println!(
        "{} files, {:.1} MB, {} blobs read from the store, in {:.0} ms",
        done.files,
        done.bytes as f64 / 1e6,
        done.materialised,
        ms(started)
      );
    }
    "status" => {
      let ws = Workspace::open(Path::new(arg(1)?))?;
      let (found, looked) = workspace::status(&ws, Look::Everywhere)?;
      for (path, new, old) in found {
        let mark = match (new.is_some(), old.is_some()) {
          (true, true) => 'M',
          (true, false) => 'A',
          _ => 'D',
        };
        println!("{mark} {}", String::from_utf8_lossy(&path));
      }
      eprintln!("looked at {} files, read {}, in {:.1} ms", looked.stats, looked.reads, ms(started));
    }
    "record" => {
      let ws = Workspace::open(Path::new(arg(1)?))?;
      for path in &args[2.min(args.len())..] {
        ws.record(path.as_bytes())?;
      }
    }
    "snapshot" => {
      let mut ws = Workspace::open(Path::new(arg(1)?))?;
      let mut store = Store::open(&ws.store)?;
      let look = if args.get(2).is_some_and(|a| a == "--recorded") { Look::Recorded } else { Look::Everywhere };
      match workspace::snapshot(&mut store, &mut ws, AUTHOR, look)? {
        Some(id) => println!("{} in {:.1} ms", hex(&id), ms(started)),
        None => println!("nothing changed ({:.1} ms)", ms(started)),
      }
    }
    "commit" => {
      let mut ws = Workspace::open(Path::new(arg(1)?))?;
      let mut store = Store::open(&ws.store)?;
      newvcs::colocated::guard(&ws)?;
      match workspace::commit(&mut store, &mut ws, AUTHOR, arg(2)?, look(3))? {
        Some(id) => {
          newvcs::colocated::published(&mut store, &ws)?;
          println!("{} in {:.0} ms", hex(&id), ms(started));
        }
        None => println!("nothing to commit"),
      }
    }
    "restore" => {
      let mut ws = Workspace::open(Path::new(arg(1)?))?;
      let mut store = Store::open(&ws.store)?;
      let target = workspace::state_named(&ws, arg(2)?)?;
      let done = workspace::restore_to(&mut store, &mut ws, target, AUTHOR, look(3))?;
      let kept = done.kept.map(|id| format!("; what was on disk is kept as {}", hex(&id))).unwrap_or_default();
      println!(
        "restored to {}: {} files written, {} removed, in {:.1} ms{kept}",
        &hex(&target)[..12],
        done.written,
        done.removed,
        ms(started)
      );
    }
    "mark" => {
      let mut ws = Workspace::open(Path::new(arg(1)?))?;
      let mut store = Store::open(&ws.store)?;
      let id = workspace::mark(&mut store, &mut ws, arg(2)?, AUTHOR, look(3))?;
      println!("{} is {}", arg(2)?, hex(&id));
    }
    "since" => {
      let ws = Workspace::open(Path::new(arg(1)?))?;
      let mut store = Store::open(&ws.store)?;
      let state = workspace::state_named(&ws, arg(2)?)?;
      for (path, what) in workspace::since(&mut store, &ws, state, look(3))? {
        println!("{what} {}", String::from_utf8_lossy(&path));
      }
    }
    "agent-init" if arg(1)? == "claude" => println!("{CLAUDE_HOOKS}"),
    "watch" => match args.get(2).map(String::as_str) {
      None => newvcs::watch::watch(Path::new(arg(1)?), AUTHOR)?,
      Some("--start") => println!("{}", newvcs::watch::start(Path::new(arg(1)?))?),
      Some("--stop") => println!("{}", newvcs::watch::stop(Path::new(arg(1)?))?),
      Some("--status") => println!("{}", newvcs::watch::status(Path::new(arg(1)?))),
      Some(_) => return Err(USAGE.to_string()),
    },
    "log" => {
      for (at, what, id) in Workspace::open(Path::new(arg(1)?))?.operations() {
        println!("{at} {what:<8} {id}");
      }
    }
    "remote" => {
      Workspace::open(Path::new(arg(1)?))?.set_remote(arg(2)?, arg(3)?)?;
    }
    "push" => {
      let mut ws = Workspace::open(Path::new(arg(1)?))?;
      let mut store = Store::open(&ws.store)?;
      let done = remote::push(&mut store, &mut ws, AUTHOR)?;
      println!(
        "{} commits taken as they were, {} moved onto a newer tip, {} file contents sent; {} in {:.0} ms",
        done.updated,
        done.rebased,
        done.contents_sent,
        describe(&done.synced),
        ms(started)
      );
    }
    "sync" => {
      let mut ws = Workspace::open(Path::new(arg(1)?))?;
      let mut store = Store::open(&ws.store)?;
      let done = remote::sync(&mut store, &mut ws, AUTHOR)?;
      println!("{} in {:.0} ms", describe(&done), ms(started));
    }
    "fork" => {
      workspace::fork(Path::new(arg(1)?), Path::new(arg(2)?))?;
      println!("forked in {:.0} ms", ms(started));
    }
    "export" => {
      let ws = Workspace::open(Path::new(arg(1)?))?;
      let mut store = Store::open(&ws.store)?;
      let base = ws.base.ok_or("the workspace has no commit")?;
      let (oid, written) = git::export(&mut store, &base, Path::new(arg(2)?), arg(3)?)?;
      println!("{} at {} ({written} git objects written) in {:.0} ms", arg(3)?, hex(&oid), ms(started));
    }
    "adopt" => println!("{}", newvcs::colocated::adopt(Path::new(arg(1)?))?),
    "shell-init" => print!("{}", newvcs::colocated::SHELL_INIT),
    "stats" => print_stats(&Store::open(Path::new(arg(1)?))?),
    _ => return Err(USAGE.to_string()),
  }
  Ok(())
}

fn describe(s: &remote::Synced) -> String {
  format!(
    "caught up {} commits: {} files written, {} removed, {} contents fetched, {} changes outside the view left alone",
    s.commits, s.written, s.removed, s.contents_fetched, s.outside_view
  )
}

fn print_stats(store: &Store) {
  let s = store.stats;
  let on_disk = |name: &str| std::fs::metadata(PathBuf::from(&store.dir).join(name)).map_or(0, |m| m.len()) as f64 / 1e6;
  println!(
    "store: {} objects ({} as deltas), {:.1} MB raw, {:.1} MB in the segment, {:.1} MB of git ids, {:.1} MB of derived roots",
    s.objects,
    s.deltas,
    s.raw_bytes as f64 / 1e6,
    on_disk("objects.seg").max(s.stored_bytes as f64 / 1e6),
    on_disk("git.map"),
    on_disk("roots.map"),
  );
  for (name, [count, raw, stored]) in ["full blobs", "delta blobs", "trees", "commits"].iter().zip(s.by_class) {
    println!("  {name:<12} {count:>8} objects {:>9.1} MB raw {:>8.1} MB stored", raw as f64 / 1e6, stored as f64 / 1e6);
  }
}

fn main() -> ExitCode {
  // Output cut short by `| head` is the reader's choice, not a failure to report.
  // SAFETY: called once, before any thread or output exists.
  unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
  let args: Vec<String> = std::env::args().skip(1).collect();
  // `nv git …` answers as Git would, exit code and all.
  if args.first().is_some_and(|a| a == "git") {
    return ExitCode::from(newvcs::gitcli::run(&args[1..]) as u8);
  }
  if args.first().is_some_and(|a| a == "hook") && args.get(1).is_some_and(|a| a == "claude") {
    claude_hook();
    return ExitCode::SUCCESS;
  }
  // These answer with an exit code that says how things stand.
  let told = match args.first().map(String::as_str) {
    Some("check") => args.get(1).map(|dir| newvcs::colocated::check(Path::new(dir))),
    Some("reconcile") => args.get(1).map(|dir| newvcs::colocated::reconcile(Path::new(dir))),
    _ => None,
  };
  if let Some(Ok((code, text))) = &told {
    println!("{text}");
    // Being watched, or meant to be and not, is part of how a workspace stands.
    let dir = Path::new(&args[1]);
    let (watched, expected) = (newvcs::watch::alive(dir), newvcs::watch::expected(dir));
    if args[0] == "check" && (watched || expected) {
      println!("{}", newvcs::watch::status(dir));
    }
    return ExitCode::from(if args[0] == "check" && expected && !watched { 1 } else { *code as u8 });
  }
  if let Some(Err(why)) = told {
    eprintln!("{why}");
    return ExitCode::from(2);
  }
  // A command that trusts reported writes first lets a watcher report the last of them.
  if let Some(dir) = args.get(1).filter(|_| args.iter().any(|a| a == "--recorded")) {
    newvcs::watch::settle(Path::new(dir));
  }
  // One writer at a time in a workspace. A watcher takes the lock for each snapshot instead, not for as long as it runs.
  let _lock = match (args.first().map(String::as_str), args.get(1)) {
    (Some(command), Some(dir)) if command != "watch" && command != "fork" && Path::new(dir).join(".nv").join("workspace").is_file() => {
      workspace::lock(Path::new(dir)).ok()
    }
    _ => None,
  };
  match run(&args) {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      eprintln!("{e}");
      ExitCode::from(2)
    }
  }
}
