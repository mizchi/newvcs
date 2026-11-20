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
  nv commit <dir> <message>
        Publish the snapshots since the last commit as one commit; the rest
        of the tree is not needed
  nv log <dir>
        The workspace's operations
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
  nv stats <store>
";

const AUTHOR: &str = "nv <nv@localhost>";

fn run(args: &[String]) -> Result<()> {
  let arg = |i: usize| args.get(i).map(String::as_str).ok_or_else(|| USAGE.to_string());
  let started = Instant::now();
  let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
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
      match workspace::commit(&mut store, &mut ws, AUTHOR, arg(2)?)? {
        Some(id) => println!("{} in {:.0} ms", hex(&id), ms(started)),
        None => println!("nothing to commit"),
      }
    }
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
  match run(&args) {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      eprintln!("{e}");
      ExitCode::from(2)
    }
  }
}
