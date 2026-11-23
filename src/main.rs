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
  nv snapshot <dir> [--recorded | --tracked]
        Record what is on disk as a commit nobody else sees. With --recorded,
        look only at the paths given to `nv record`; with --tracked, only at
        the files already known, as `git commit -a` does (new files are not found).
        Either way it is this command's look that is narrowed: `nv commit`
        publishes everything recorded so far, by whichever command recorded it
  nv record <dir> <path>...
        Say that these paths were written (for a tool that knows its writes)
  nv commit <dir> [<message>] [--recorded | --tracked] [--no-checks]
        Publish the snapshots since the last commit as one commit; the rest
        of the tree is not needed. Where nv.toml declares conditions, the
        state must pass them first (--no-checks makes it anyway); with no
        message, the one its [message] hook drafted or drafts now
  nv checkpoint <dir>
        Record what is on disk and run the conditions nv.toml declares on
        it, in order: the state is then `checkpoint-<name>` for each that
        passed. Exits 1 where one did not, or the files changed meanwhile.
        With `[commit] each = true` in nv.toml a state that passed them all
        is committed, there and then, under its drafted message or a
        provisional one, to be made again with `nv rebuild` before a push
  nv publish <dir> [--message <line>]
        Wait as `nv pending --wait` does, make the commits not pushed into
        one at each `nv note`, of everything since the one before (what
        came after the last note under --message, or with it), and push
        them. Where the ref took changes to the same files, catch up and
        push again; where that leaves files with both versions, stop.
        With a `[body]` hook in nv.toml, each commit's body is what it
        prints, given the change on stdin and the note in NV_SUBJECT
  nv note <dir> <line>
        Say in a line what was just done: the next commit the conditions
        pass on (`[commit] each`), of these files or later ones, is made
        under it. Where that commit was made already, it is renamed
  nv pending <dir> [--wait]
        The commits made here and not pushed, oldest first: each with its
        number, its Git id and nv's, message, the conditions its files
        passed, and its paths. With --wait, first wait until nothing more is
        coming: the watcher has recorded every write, no run of the
        conditions is under way, and what is on disk is committed or did not
        pass; the last line says which
  nv rebuild <dir> <commit> <message> [<commit> <message>]...
        Make the commits not pushed again, as fewer: one new commit for each
        pair, of what the commits up to and including <commit> changed since
        the one before, under <message>. <commit> is its number in
        `nv pending`, or the start of its Git id or nv's. Commits after the
        last one named are made again as they were. Where each <commit>
        passed the conditions, each new commit does; it is said where one
        did not. Each new commit keeps the author of the first commit it is
        made of, and when that was written, as a squash in Git does; its
        committer is whoever runs this, now. The files are not touched
  nv watch <dir> [--start | --stop | --status]
        Stay, and snapshot whatever is written there by whatever writes it:
        the operating system reports the writes, so nothing has to. With
        --start it stays after this command returns, and is started again
        by a hook or a command with --recorded should it ever be found gone
  nv log <dir>
        The workspace's operations
  nv restore <dir> <state> [--recorded]
        Make the files what they were at a recorded state: a name given with
        `nv mark`, `base` (the last commit), `~N` (N recorded states ago), the
        start of an id, `before-sync` (which undoes the last `nv sync`, files
        and all), or `pushed` (the ref as last seen: commits made here and not
        pushed are set aside, under an id it prints). What is on disk is
        recorded first, so nothing is lost by going
  nv mark <dir> <name> [--recorded]
        Record what is on disk and name that state, e.g. `green`. A name
        given before moves to the new state
  nv since <dir> <state> [--recorded]
        The paths that differ between a recorded state and the files now.
        With --recorded these three look only at the paths given to
        `nv record` (and at what a restore would overwrite), not at the tree
  nv hook claude
        Read a Claude Code PostToolUse event on stdin and record what the
        tool wrote. `nv agent-init claude` prints the settings that run it
  nv remote <dir> <url-of-repository> <ref>
        Point the workspace at a server: http://host/<owner>/<repo> and a ref,
        e.g. http://localhost:8799/me/project main
  nv push <dir>
        Send the published commits the ref has not seen, then catch up with it
  nv sync <dir>
        Bring the workspace to the ref's tip, keeping its own work on top. A
        file both changed is merged as text; where the same lines changed it is
        left with both versions between markers, and the exit code is 1
  nv fork <dir> <new-dir>
        Clone a workspace copy-on-write, build output included
  nv export <dir> <git-repo> <ref>
        Write the workspace's commit into a Git repository
  nv adopt <dir> [--undo]
        Make an nv workspace of a Git repository's directory, in place. Git is
        left as it is and free to do anything; nv follows it. With --undo, take
        nv out again
  nv check <dir>
        Say whether Git and nv are in step there, and if not, who moved
  nv reconcile <dir>
        Take in where Git went, now (any command that needs to does it anyway)
  nv shell-init
        A shell function that sends `git` to `nv git` inside a workspace
  nv name <store> /<owner>/<repo> <rev>
        Give a commit a repository's name in the store, or move the name
  nv names <store>
        Every repository in the store, and one hash for the state of them all
  nv link <dir> <path> /<owner>/<repo> [dir-within-it]
        Put a directory of another repository at <path>, by its id: nothing
        is copied, and where it came from is written in the tracked `nv.links`
  nv links <dir> [--update]
        How each link stands; with --update, follow the repositories that moved
  nv unlink <dir> <path>
        Stop following: the directory stays, as ordinary files
  nv stats <store>
  nv help [command], nv --version

A <dir> may be the top of a workspace or any directory inside one. After
`--` nothing is an option: nv commit <dir> -- '-a message that begins so'
  nv git <command> [<args>]
        The everyday Git commands, on an nv store (see tools/git-shim)

Exit codes: 0 done; 1 done, and something is left to a person (a sync or a
push that left files with both versions, conditions that did not pass for
`nv commit` or `nv checkpoint`); 2 refused or failed, as the message says (a
push the ref refused because it took changes to the same files: `nv sync`,
then push again); 125 `nv git` does not speak what it was given.
";

const AUTHOR: &str = "nv <nv@localhost>";

/// The state the conditions passed on, when `nv commit` ran them before taking the workspace.
static GATED: std::sync::OnceLock<newvcs::Id> = std::sync::OnceLock::new();
/// What `nv pending --wait` found once it had waited, before taking the workspace.
static SETTLED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

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
  // The write is noted before anything is waited for: the note is an append, and a snapshot that
  // cannot be taken now (another command holds the workspace past the limit) is taken by the next.
  if let Some(relative) = written.as_ref().and_then(|path| path.strip_prefix(&root).ok()) {
    Workspace::open(&root).ok()?.record(relative.as_os_str().as_encoded_bytes()).ok()?;
  }
  let _lock = workspace::lock(&root).ok()?;
  let mut ws = Workspace::open(&root).ok()?;
  let mut store = Store::open(&ws.store).ok()?;
  workspace::unfold(&mut store, &mut ws).ok()?;
  // Where Git moved on its own, that first: what is recorded is recorded on where the files are.
  let _ = newvcs::colocated::follow(&mut store, &mut ws);
  let look = match written.as_ref().and_then(|path| path.strip_prefix(&root).ok()) {
    Some(_) => Look::Recorded,
    None => Look::Everywhere,
  };
  workspace::snapshot(&mut store, &mut ws, AUTHOR, look).ok()?;
  // Where the repository declares conditions to run behind writes, they are started here.
  newvcs::checks::start_behind(&root);
  Some(())
}

/// A workspace and its store, for a command that builds on where the workspace
/// stands: if Git shares the directory and has moved, that is taken in first.
fn opened(dir: &str) -> Result<(Workspace, Store)> {
  let mut ws = Workspace::open(Path::new(dir))?;
  let mut store = Store::open(&ws.store)?;
  // Snapshots a look in Git's terms set aside are this workspace's again.
  workspace::unfold(&mut store, &mut ws)?;
  if let Some(done) = newvcs::colocated::follow(&mut store, &mut ws)? {
    eprintln!("nv: {done}");
  }
  Ok((ws, store))
}

/// A commit's short Git id, and the conditions its files passed.
fn described(store: &mut Store, root: &Path, id: &newvcs::Id) -> Result<(String, Vec<String>)> {
  let git = newvcs::git::git_id_of(store, id).map(|oid| hex(&oid)[..7].to_string()).unwrap_or_else(|_| "-".repeat(7));
  let passed = match newvcs::commit::root_of(store, id)? {
    Some(tree) => newvcs::checks::passed_on_tree(root, &tree),
    None => Vec::new(),
  };
  Ok((git, passed))
}

/// A commit not pushed, by its number in `nv pending` or the start of its id.
fn commit_named(store: &mut Store, ws: &Workspace, name: &str) -> Result<newvcs::Id> {
  let chain = workspace::pending(store, ws)?;
  if let Ok(n) = name.parse::<usize>()
    && name.len() < 6
  {
    return chain
      .get(n.wrapping_sub(1))
      .map(|(id, _)| *id)
      .ok_or_else(|| format!("there is no commit {n} not pushed: `nv pending` lists them"));
  }
  let wanted = name.to_ascii_lowercase();
  let mut found: Vec<&newvcs::Id> = chain.iter().map(|(id, _)| id).filter(|id| hex(*id).starts_with(&wanted)).collect();
  // Or by its Git id, as `nv git log` and `nv pending` show it.
  for (id, _) in &chain {
    if newvcs::git::git_id_of(store, id).is_ok_and(|oid| hex(&oid).starts_with(&wanted)) && !found.contains(&id) {
      found.push(id);
    }
  }
  match found[..] {
    [one] if name.len() >= 6 => Ok(*one),
    [] => Err(format!("`{name}` is not a commit here that is not pushed: `nv pending` lists them")),
    _ => Err(format!("`{name}` names more than one commit, or is too short: give six characters or more")),
  }
}

fn signature(ws: &Workspace, role: &str) -> Result<String> {
  workspace::signature(ws, role)
}

/// A path for a line of output: as it is, or, where it holds a newline, a quote or bytes
/// that are not text, in double quotes with escapes, as Git prints such a path.
fn shown(path: &[u8]) -> String {
  String::from_utf8_lossy(&newvcs::gitcli::quoted(path, false)).into_owned()
}

/// Say, of a workspace that is a view of part of the tree, what was written outside that part.
fn warn_outside(ws: &Workspace) {
  let outside = workspace::outside_view(ws);
  if !outside.is_empty() {
    eprintln!(
      "nv: outside this workspace's view, and so not recorded: {}\nnv: the view is {}; a workspace of those paths, or of everything, would record them",
      outside.join(", "),
      ws.prefixes.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect::<Vec<_>>().join(", ")
    );
  }
}

fn run(args: &[String]) -> Result<()> {
  let arg = |i: usize| args.get(i).map(String::as_str).ok_or_else(|| USAGE.to_string());
  let started = Instant::now();
  let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
  // `--recorded` after the arguments: the writer reported every write, so the tree need not be walked.
  // `--tracked`: only the files already known, as `git commit -a` looks; what is new is not found.
  let look = |at: usize| {
    let given = |option: &str| args.iter().skip(at.min(2)).any(|a| a == option);
    if given("--recorded") {
      Look::Recorded
    } else if given("--tracked") {
      Look::Tracked
    } else {
      Look::Everywhere
    }
  };
  match arg(0)? {
    "import" => {
      let mut store = Store::create(Path::new(arg(2)?))?;
      let limit = args.get(4).and_then(|n| n.parse().ok());
      let rev = args.get(3).map_or("HEAD", String::as_str);
      let done = git::import(&mut store, Path::new(arg(1)?), rev, limit)?;
      // What was imported can be asked for by the name it was asked for by: `nv checkout <store> HEAD <dir>`.
      let plain = rev.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)) && !rev.contains("..") && !rev.starts_with('/');
      if limit.is_none() && plain {
        let tip = std::process::Command::new("git")
          .arg("-C")
          .arg(arg(1)?)
          .args(["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")])
          .output()
          .ok();
        let tip = tip.map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string()).and_then(|oid| store.resolve(&oid));
        if let Some(tip) = tip {
          store.set_ref(rev, tip)?;
        }
      }
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
      // First that every object is what its name says and that the segment ends where a record does.
      let (objects, damaged) = store.check()?;
      if !damaged.is_empty() {
        return Err(format!("{} of {objects} objects are damaged, or the store is cut short:\n{}", damaged.len(), damaged.join("\n")));
      }
      // Then the ready copies a checkout clones without reading.
      let copies = workspace::check_copies(&mut store)?;
      if !copies.is_empty() {
        return Err(format!(
          "{}\nThe store itself is whole; a workspace made from one of these copies may hold the same bytes.",
          copies.join("\n")
        ));
      }
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
      let mut ws = Workspace::open(Path::new(arg(1)?))?;
      // Snapshots a look in Git's terms set aside are what status lists against, as ever.
      if ws.root.join(".nv").join("folded").exists() {
        let mut store = Store::open(&ws.store)?;
        workspace::unfold(&mut store, &mut ws)?;
      }
      let (found, looked) = workspace::status(&ws, Look::Everywhere)?;
      for (path, new, old) in found {
        let mark = match (new.is_some(), old.is_some()) {
          (true, true) => 'M',
          (true, false) => 'A',
          _ => 'D',
        };
        println!("{mark} {}", shown(&path));
      }
      eprintln!("looked at {} files, read {}, in {:.1} ms", looked.stats, looked.reads, ms(started));
      warn_outside(&ws);
      // What this lists is what no snapshot has yet. Two other things a reader would want to
      // know are said, not listed: looking changes nothing, so neither is acted on here.
      if matches!(newvcs::colocated::drift(&ws), Some(newvcs::colocated::Drift::GitMoved(_) | newvcs::colocated::Drift::BothMoved(_))) {
        eprintln!(
          "nv: Git has moved since nv last looked, so this is against the commit nv knows. `nv reconcile <dir>` takes Git's move in; so does anything that records or publishes."
        );
      }
      if ws.head != ws.base {
        let mut store = Store::open(&ws.store)?;
        let mut root = |id: Option<newvcs::Id>| id.map_or(Ok(None), |id| newvcs::commit::root_of(&mut store, &id));
        let (from, to) = (root(ws.base)?, root(ws.head)?);
        let unpublished = newvcs::manifest::diff(&mut store, from, to)?.len();
        if unpublished > 0 {
          // Where each state that passes is committed, that is what commits these, not a command.
          let each = newvcs::checks::load(&ws.root).ok().flatten().is_some_and(|c| c.commit_each && !c.checks.is_empty());
          let then = if each {
            "they are committed as the conditions pass on them (`nv pending <dir> --wait` says when)"
          } else {
            "`nv commit` publishes them"
          };
          eprintln!("nv: {unpublished} paths are recorded and not yet published: `nv since <dir> base` lists them, {then}");
        }
      }
    }
    "record" => {
      let ws = Workspace::open(Path::new(arg(1)?))?;
      // Told from the directory that was given, which may be below the workspace's top.
      let whole = |dir: &Path| std::fs::canonicalize(dir).map_err(|e| e.to_string());
      let below = whole(Path::new(arg(1)?))?.strip_prefix(whole(&ws.root)?).map(Path::to_path_buf).unwrap_or_default();
      let paths: Vec<PathBuf> = args[2.min(args.len())..]
        .iter()
        .map(|path| if Path::new(path).is_absolute() { PathBuf::from(path) } else { below.join(path) })
        .collect();
      ws.record_all(&paths.iter().map(|path| path.as_os_str().as_encoded_bytes()).collect::<Vec<_>>())?;
    }
    "snapshot" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      let look = look(2);
      match workspace::snapshot(&mut store, &mut ws, AUTHOR, look)? {
        Some(id) => println!("{} in {:.1} ms", hex(&id), ms(started)),
        None => println!("nothing changed ({:.1} ms)", ms(started)),
      }
      warn_outside(&ws);
    }
    "commit" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      warn_outside(&ws);
      let unsettled = workspace::unsettled(&ws);
      if !unsettled.is_empty() {
        return Err(format!(
          "these still hold both versions between <<<<<<< and >>>>>>> markers, from the last sync: {}\nEdit them to what they should be, then commit.",
          unsettled.join(", ")
        ));
      }
      // The message given, or else the one drafted for the state this commit is of.
      let given = args.get(2).filter(|a| !["--recorded", "--tracked", "--no-checks"].contains(&a.as_str()));
      let config = newvcs::checks::load(&ws.root)?;
      // Refused before anything is recorded: a refused command changes nothing.
      if given.is_none() && config.as_ref().is_none_or(|c| c.message.is_none()) {
        return Err("a commit needs a message: give one, or declare a [message] hook in nv.toml to draft one".into());
      }
      let message = if given.is_some() && GATED.get().is_none() {
        given.cloned().unwrap_or_default()
      } else {
        workspace::snapshot(&mut store, &mut ws, AUTHOR, look(3))?;
        // The files, not the state's id: the runner behind writes may have committed the state
        // the conditions passed on, which moves the head and leaves the files as they were.
        if let (Some(passed), Some(now)) = (GATED.get(), ws.head)
          && *passed != now
          && newvcs::commit::root_of(&mut store, passed)? != newvcs::commit::root_of(&mut store, &now)?
        {
          return Err("the files changed after the conditions passed on them: run `nv commit` again".into());
        }
        match given {
          Some(given) => given.clone(),
          None => {
            let state = ws.head.ok_or("nothing to commit")?;
            let hook = config.as_ref().and_then(|c| c.message.as_ref());
            newvcs::checks::draft(&ws.root, &state)
              .or_else(|| hook.and_then(|hook| newvcs::checks::write_draft(&ws.root, hook, &state).ok()))
              .ok_or("a commit needs a message: give one, or declare a [message] hook in nv.toml to draft one")?
          }
        }
      };
      let message = message.as_str();
      if message.trim().is_empty() {
        return Err("a commit needs a message that says something".into());
      }
      // A file holding both versions between a sync's markers is not published: refused by the
      // commit once it has recorded what is on disk, from what it recorded.
      // Beside a Git repository it is Git that makes the commit, by its own rules.
      if newvcs::colocated::shared(&ws.root) {
        match newvcs::colocated::commit(&mut store, &mut ws, AUTHOR, message, look(3))? {
          Some(oid) => println!("{oid} in {:.0} ms", ms(started)),
          None => println!("nothing to commit"),
        }
        return Ok(());
      }
      // The committer is the author unless the environment or the configuration names another, as with Git.
      let author = signature(&ws, "AUTHOR")?;
      let committer = signature(&ws, "COMMITTER")?;
      let rest = format!("author {author}\ncommitter {committer}\n\n{}\n", message.trim_end()).into_bytes();
      let based = ws.base;
      match workspace::commit_saying(&mut store, &mut ws, AUTHOR, rest, look(3))? {
        Some(id) => {
          workspace::take_branch_along(&mut store, &ws, based)?;
          // Everything recorded is in the commit: what `nv git add` had set apart for one is spent.
          let _ = std::fs::remove_file(ws.root.join(".nv").join("git-staged"));
          newvcs::colocated::published(&mut store, &ws)?;
          println!("{} in {:.0} ms", hex(&id), ms(started));
        }
        // Committed already, by the runner behind writes (`[commit] each`): say so, and how to
        // give that commit the message this one was given.
        None if GATED.get().is_some() && based.is_some_and(|b| newvcs::checks::committed_by_runner(&ws.root, &b)) => {
          println!(
            "nothing to commit: these files were committed already, as {}, behind the writes (`[commit] each`); `nv rebuild <dir> <n> <message>` gives that commit this message",
            &hex(&based.unwrap_or_default())[..12]
          )
        }
        None => println!("nothing to commit"),
      }
    }
    "checkpoint" => {
      let root = workspace::top_of(Path::new(arg(1)?)).ok_or("not in a workspace")?;
      let config = newvcs::checks::load(&root)?
        .filter(|c| !c.checks.is_empty())
        .ok_or(format!("{} declares no conditions: [checks.<name>] with `run = \"<command>\"`", newvcs::checks::FILE))?;
      // Behind a write: run until a run finds the files as it began, quietly.
      if args.iter().any(|a| a == "--until-still") {
        newvcs::checks::run_until_still(&root, &config, AUTHOR)?;
        return Ok(());
      }
      let run = newvcs::checks::run(&root, &config, AUTHOR)?;
      eprint!("{}", run.said());
      if !run.passed() {
        std::process::exit(1);
      }
      if let Some(hook) = &config.message
        && newvcs::checks::draft(&root, &run.state).is_none()
        && let Err(why) = newvcs::checks::write_draft(&root, hook, &run.state)
      {
        eprintln!("nv: {why}");
      }
      if config.commit_each {
        match newvcs::checks::commit_checkpoint(&root, &config, &run.state, AUTHOR)? {
          Some(id) => println!("{} passed every condition, and is committed as {}, in {:.1} ms", hex(&run.state), hex(&id), ms(started)),
          None => println!("{} passed every condition, and is what is committed already, in {:.1} ms", hex(&run.state), ms(started)),
        }
        return Ok(());
      }
      println!("{} passed every condition, in {:.1} ms", hex(&run.state), ms(started));
    }
    "publish" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      // `--message <line>`: the line comes after the directory, as the arguments are laid out.
      let last_message = match (args.iter().any(|a| a == "--message"), args.get(2).filter(|a| !a.starts_with("--"))) {
        (true, Some(line)) => Some(line.clone()),
        (true, None) => return Err("`nv publish --message` takes the line to give the last commit".to_string()),
        (false, Some(_)) => return Err(USAGE.to_string()),
        (false, None) => None,
      };
      if let Some(settled) = SETTLED.get() {
        println!("{settled}");
      }
      let chain = workspace::pending(&mut store, &ws)?;
      if chain.is_empty() {
        println!("nothing to publish: every commit made here is pushed");
        return Ok(());
      }
      // A commit at each note (a message that is not one the runner made up), of everything
      // since the one before; what came after the last note under `--message`, or with it.
      let message_of = |commit: &newvcs::commit::Commit| {
        let said = String::from_utf8_lossy(&commit.rest).into_owned();
        said.split_once("\n\n").map(|(_, m)| m.trim_end().to_string()).unwrap_or_default()
      };
      let author_of = |commit: &newvcs::commit::Commit| {
        String::from_utf8_lossy(&commit.rest).lines().find_map(|l| l.strip_prefix("author ").map(str::to_string))
      };
      let committer = signature(&ws, "COMMITTER")?;
      let mut plan: Vec<(newvcs::Id, Vec<u8>)> = Vec::new();
      let mut first: Option<String> = None;
      for (id, commit) in &chain {
        first = first.or_else(|| author_of(commit));
        let message = message_of(commit);
        if !message.starts_with(newvcs::checks::PROVISIONAL) {
          let author = first.take().map_or_else(|| signature(&ws, "AUTHOR"), Ok)?;
          plan.push((*id, format!("author {author}\ncommitter {committer}\n\n{message}\n").into_bytes()));
        }
      }
      let (last, last_commit) = chain.last().expect("not empty");
      if plan.last().map(|(id, _)| id) != Some(last) {
        match (last_message, plan.pop()) {
          (Some(message), noted) => {
            plan.extend(noted);
            let author = first.take().map_or_else(|| signature(&ws, "AUTHOR"), Ok)?;
            plan.push((*last, format!("author {author}\ncommitter {committer}\n\n{}\n", message.trim_end()).into_bytes()));
          }
          // What came after the last note goes with it.
          (None, Some((_, rest))) => plan.push((*last, rest)),
          (None, None) => {
            return Err(format!(
              "nothing says what these {} commits are: no `nv note` was made, and no `--message` given. Nothing was changed",
              chain.len()
            ));
          }
        }
        let _ = last_commit;
      }
      let mut made = workspace::rebuild(&mut store, &mut ws, &plan, AUTHOR)?;
      // A body for each from the `[body]` hook, given its summary line and its change; where the
      // hook gives none, the commit goes without one, and that is said.
      if let Some(hook) = newvcs::checks::load(&ws.root)?.and_then(|c| c.body) {
        let mut again = Vec::new();
        let mut parent = workspace::pending(&mut store, &ws)?.first().and_then(|(_, c)| c.parents.first().copied());
        for id in &made {
          let commit = newvcs::commit::Commit::load(&mut store, id)?;
          let said = String::from_utf8_lossy(&commit.rest).into_owned();
          let (head, message) = said.split_once("\n\n").unwrap_or((said.as_str(), ""));
          // The summary line is the note; what follows it is written afresh each time, so that a
          // publish run again (after a clash) does not give a commit its body twice.
          let subject = message.lines().next().unwrap_or_default().to_string();
          let change = newvcs::checks::change_between(&mut store, parent, id)?;
          let message = match newvcs::checks::write_body(&ws.root, &hook, id, &subject, &change) {
            Ok(body) => format!("{subject}\n\n{body}"),
            Err(why) => {
              eprintln!("nv: \"{subject}\" goes without a body: {why}");
              message.trim_end().to_string()
            }
          };
          again.push((*id, format!("{head}\n\n{message}\n").into_bytes()));
          parent = Some(*id);
        }
        made = workspace::rebuild(&mut store, &mut ws, &again, AUTHOR)?;
      }
      // Pushed, and where the ref took changes to the same files meanwhile, caught up with and
      // pushed again, as long as catching up leaves nothing for a person.
      let mut tries = 0;
      let done = loop {
        tries += 1;
        match remote::push(&mut store, &mut ws, AUTHOR) {
          Ok(done) => break done,
          Err(why) if why.contains("has changes to the same paths") && tries < 3 => {
            let subjects: Vec<String> = made
              .iter()
              .map(|id| newvcs::commit::Commit::load(&mut store, id).map(|c| message_of(&c).lines().next().unwrap_or_default().to_string()))
              .collect::<Result<_>>()?;
            let synced = remote::sync(&mut store, &mut ws, AUTHOR)?;
            if !synced.conflicts.is_empty() {
              // A commit that held nothing but files the sync left unsettled is gone from the
              // tip: its line waits for the settling, as a note, not to be lost with it.
              let left: Vec<String> = workspace::pending(&mut store, &ws)?
                .iter()
                .map(|(_, c)| message_of(c).lines().next().unwrap_or_default().to_string())
                .collect();
              let lost: Vec<&String> = subjects.iter().filter(|s| !left.contains(s)).collect();
              if !lost.is_empty() {
                newvcs::checks::note(&ws.root, &lost.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; "), AUTHOR)?;
              }
              println!("{}", describe(&synced, &ws.root));
              eprintln!("nv: the commits are made, and not pushed: settle those files, then `nv publish` again");
              std::process::exit(1);
            }
          }
          Err(why) => return Err(why),
        }
      };
      let _ = remote::fetch_for_export(&mut store, &ws);
      let git = |store: &mut Store, id: &newvcs::Id| {
        newvcs::git::git_id_of(store, id).map_or_else(|_| hex(id)[..12].to_string(), |oid| hex(&oid)[..7].to_string())
      };
      println!("published {} as {}:", counted(chain.len(), "commit"), counted(made.len(), "commit"));
      for id in done.landed.iter() {
        let subject = message_of(&newvcs::commit::Commit::load(&mut store, id)?).lines().next().unwrap_or_default().to_string();
        println!("  {}  {subject}", git(&mut store, id));
      }
      if !done.merged_on_server.is_empty() {
        println!("merged on the server (merge=union): {}", done.merged_on_server.join(", "));
      }
      let tip = ws.pushed.map(|id| git(&mut store, &id)).unwrap_or_default();
      println!(
        "`{}` on {} is now at {tip}, in {:.0} ms",
        ws.remote_ref.as_deref().unwrap_or("?"),
        ws.remote.as_deref().unwrap_or("?"),
        ms(started)
      );
      newvcs::checks::start_behind(&ws.root);
    }
    "note" => {
      let root = workspace::top_of(Path::new(arg(1)?)).ok_or("not in a workspace")?;
      println!("{}", newvcs::checks::note(&root, arg(2)?, AUTHOR)?);
      newvcs::checks::start_behind(&root);
    }
    "pending" => {
      let (ws, mut store) = opened(arg(1)?)?;
      let chain = workspace::pending(&mut store, &ws)?;
      if chain.is_empty() {
        println!("nothing committed here that is not pushed");
      }
      if ws.pushed.or(ws.start).is_none() && !chain.is_empty() {
        println!("(this workspace has no ref it pushes to, nor a commit it was made at: all of its history is listed)");
      }
      for (n, (id, commit)) in chain.iter().enumerate() {
        let said = String::from_utf8_lossy(&commit.rest).into_owned();
        let message = said.split_once("\n\n").map_or("", |(_, m)| m).trim();
        let first = message.lines().next().unwrap_or_default();
        let (git, passed) = described(&mut store, &ws.root, id)?;
        let passed = if passed.is_empty() { "conditions not run on it".to_string() } else { format!("passed {}", passed.join(", ")) };
        println!("{:>3}  git {git}  nv {}  {first}  ({passed})", n + 1, &hex(id)[..12]);
        let before = match commit.parents.first() {
          Some(parent) => newvcs::commit::root_of(&mut store, parent)?,
          None => None,
        };
        for (path, entry) in &commit.changes {
          let was = match before {
            Some(root) => newvcs::manifest::lookup(&mut store, root, path)?.is_some(),
            None => false,
          };
          let what = match (entry.is_some(), was) {
            (true, false) => "A",
            (true, true) => "M",
            (false, _) => "D",
          };
          println!("       {what} {}", String::from_utf8_lossy(path));
        }
      }
      let unsettled = workspace::unsettled(&ws);
      if !unsettled.is_empty() {
        println!(
          "still holding both versions, and so in none of the commits above: {}. Once they are settled and the conditions pass, they go into the last commit above, which they complete",
          unsettled.join(", ")
        );
      }
      if let Some(settled) = SETTLED.get() {
        println!("{settled}");
      }
    }
    "rebuild" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      let rest = &args[2..];
      if !rest.len().is_multiple_of(2) {
        return Err("`nv rebuild` takes pairs: a commit, and the message of the commit that ends with it".to_string());
      }
      let mut plan = Vec::new();
      // Each new commit is by whoever wrote the first of the commits it is made of, as a squash
      // in Git keeps it; who made it again is its committer.
      let chain = workspace::pending(&mut store, &ws)?;
      let committer = signature(&ws, "COMMITTER")?;
      let mut from = 0;
      for pair in rest.chunks(2) {
        let id = commit_named(&mut store, &ws, &pair[0])?;
        if pair[1].trim().is_empty() {
          return Err(format!("the commit ending with {} needs a message that says something", pair[0]));
        }
        let first = chain.get(from).map(|(_, commit)| String::from_utf8_lossy(&commit.rest).into_owned());
        let author = match first.as_deref().and_then(|rest| rest.lines().find_map(|line| line.strip_prefix("author "))) {
          Some(author) => author.to_string(),
          None => signature(&ws, "AUTHOR")?,
        };
        plan.push((id, format!("author {author}\ncommitter {committer}\n\n{}\n", pair[1].trim_end()).into_bytes()));
        from = chain.iter().position(|(c, _)| *c == id).map_or(from, |at| at + 1);
      }
      let before = workspace::pending(&mut store, &ws)?.len();
      let made = workspace::rebuild(&mut store, &mut ws, &plan, AUTHOR)?;
      let declared = newvcs::checks::load(&ws.root).ok().flatten().is_some_and(|c| !c.checks.is_empty());
      for (n, id) in made.iter().enumerate() {
        let (git, passed) = described(&mut store, &ws.root, id)?;
        let first = String::from_utf8_lossy(&newvcs::commit::Commit::load(&mut store, id)?.rest)
          .split_once("\n\n")
          .map_or(String::new(), |(_, m)| m.lines().next().unwrap_or_default().to_string());
        let said = if passed.is_empty() { "conditions not run on it".to_string() } else { format!("passed {}", passed.join(", ")) };
        println!("{:>3}  git {git}  nv {}  {first}  ({said})", n + 1, &hex(id)[..12]);
        if declared && passed.is_empty() {
          eprintln!("nv: commit {} ({git}) is of files the conditions have not passed on", n + 1);
        }
      }
      println!("{} not pushed made again as {}, in {:.0} ms; `nv pending` lists them", counted(before, "commit"), made.len(), ms(started));
    }
    "restore" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      let target = workspace::state_named(&mut store, &ws, arg(2)?)?;
      let done = match arg(2)? {
        "before-sync" => workspace::restore_before_sync(&mut store, &mut ws, AUTHOR, look(3))?,
        "pushed" => {
          let done = workspace::set_aside(&mut store, &mut ws, AUTHOR, look(3))?;
          if let Some(id) = done.kept {
            println!(
              "what this workspace had and the ref does not is set aside as {}: `nv since <dir> {}` lists it, `nv checkout {} {} <another-dir>` writes its files out",
              hex(&id),
              &hex(&id)[..12],
              ws.store.display(),
              hex(&id)
            );
          }
          done
        }
        _ => workspace::restore_to(&mut store, &mut ws, target, AUTHOR, look(3))?,
      };
      // Beside Git, Git goes with nv at once: a commit made with Git before the next command of
      // nv's would otherwise be made on where the workspace was, and undo what it went back over.
      if let Some(done) = newvcs::colocated::follow(&mut store, &mut ws)? {
        eprintln!("nv: {done}");
      }
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
      let (mut ws, mut store) = opened(arg(1)?)?;
      let was = workspace::state_named(&mut store, &ws, arg(2)?).ok();
      let id = workspace::mark(&mut store, &mut ws, arg(2)?, AUTHOR, look(3))?;
      match was.filter(|was| *was != id) {
        Some(was) => println!("{} is {} (it was {})", arg(2)?, hex(&id), &hex(&was)[..12]),
        None => println!("{} is {}", arg(2)?, hex(&id)),
      }
    }
    "since" => {
      let (ws, mut store) = opened(arg(1)?)?;
      let state = workspace::state_named(&mut store, &ws, arg(2)?)?;
      for (path, what) in workspace::since(&mut store, &ws, state, look(3))? {
        println!("{what} {}", shown(&path));
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
      let url = arg(2)?.trim_end_matches('/');
      let named = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .map(|rest| rest.split('/').filter(|part| !part.is_empty()).count());
      if named != Some(3) {
        return Err(format!("`{url}` is not a repository on a server: http://host/<owner>/<repo>"));
      }
      let name = arg(3)?;
      let parts_ok = name.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
      if !parts_ok || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)) {
        return Err(format!("`{name}` is not a name for a ref: letters, digits, `-`, `_`, `.` and `/`"));
      }
      Workspace::open(Path::new(arg(1)?))?.set_remote(url, name)?;
    }
    "push" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      newvcs::colocated::locked(&ws.root)?;
      let done = remote::push(&mut store, &mut ws, AUTHOR)?;
      if let Some(done) = newvcs::colocated::follow(&mut store, &mut ws)? {
        eprintln!("nv: {done}");
      }
      let landed = done.updated + done.rebased;
      if landed > 0 {
        // Git's id of the tip needs the content of every version of every file since the last
        // id known, those the server merged and nobody fetched among them: fetched as `nv git` does.
        let _ = remote::fetch_for_export(&mut store, &ws);
        let tip = ws.pushed.map(|id| match newvcs::git::git_id_of(&mut store, &id) {
          Ok(oid) => hex(&oid)[..7].to_string(),
          Err(_) => format!("{} (nv's id)", &hex(&id)[..12]),
        });
        // Where the server put them on a newer tip, they have ids of its making: named.
        let yours = if done.rebased > 0 {
          let ids: Vec<String> = done
            .landed
            .iter()
            .map(|id| match newvcs::git::git_id_of(&mut store, id) {
              Ok(oid) => hex(&oid)[..7].to_string(),
              Err(_) => hex(id)[..12].to_string(),
            })
            .collect();
          format!(" (yours: {})", ids.join(", "))
        } else {
          String::new()
        };
        println!(
          "pushed {landed} {}: {} as they were, {} put on a newer tip{yours}; `{}` on {} is now at {}",
          if landed == 1 { "commit" } else { "commits" },
          done.updated,
          done.rebased,
          ws.remote_ref.as_deref().unwrap_or("?"),
          ws.remote.as_deref().unwrap_or("?"),
          tip.unwrap_or_default()
        );
      }
      if !done.merged_on_server.is_empty() {
        println!("merged on the server (merge=union): {}", done.merged_on_server.join(", "));
      }
      // Said as what came of them: merged with, or sent in case and not needed.
      let for_merge = match (done.sent_for_merge, done.merged_on_server.is_empty()) {
        (0, _) => String::new(),
        (n, false) => format!(" (and {n} the server merged {} with)", done.merged_on_server.join(", ")),
        (n, true) => format!(" (and {n} in case the server had to merge {}: it did not)", done.stated_paths.join(", ")),
      };
      println!(
        "{} sent{for_merge}, {} of linked directories not sent; {} in {:.0} ms",
        counted(done.contents_sent, "file content"),
        done.adopted,
        describe(&done.synced, &ws.root),
        ms(started)
      );
      // And on what the push left: the ref's tip, where the server put the commits on it.
      newvcs::checks::start_behind(&ws.root);
      if done.rebased > 0 && newvcs::checks::load(&ws.root).ok().flatten().is_some_and(|c| c.auto && !c.checks.is_empty()) {
        println!("the conditions are run on what landed: `nv pending <dir> --wait` says how they end");
      }
      if !done.synced.conflicts.is_empty() {
        std::process::exit(1);
      }
    }
    "sync" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      newvcs::colocated::locked(&ws.root)?;
      let done = remote::sync(&mut store, &mut ws, AUTHOR)?;
      if let Some(done) = newvcs::colocated::follow(&mut store, &mut ws)? {
        eprintln!("nv: {done}");
      }
      println!("{} in {:.0} ms", describe(&done, &ws.root), ms(started));
      // Where the conditions run behind writes, they are run on what the sync made as well.
      newvcs::checks::start_behind(&ws.root);
      // As `git pull` does: a sync that left something for a person does not look like one that did not.
      if !done.conflicts.is_empty() {
        std::process::exit(1);
      }
    }
    "fork" => {
      workspace::fork(Path::new(arg(1)?), Path::new(arg(2)?))?;
      println!("forked in {:.0} ms", ms(started));
    }
    "export" => {
      let (ws, mut store) = opened(arg(1)?)?;
      let base = ws.base.ok_or("the workspace has no commit")?;
      let fetched = remote::fetch_for_export(&mut store, &ws)?;
      if fetched > 0 {
        println!("fetched {fetched} file contents the history needs and this store did not have");
      }
      // A name by itself is a branch's, as it is to `git push`.
      let name = if arg(3)?.starts_with("refs/") { arg(3)?.to_string() } else { format!("refs/heads/{}", arg(3)?) };
      let (oid, written) = git::export(&mut store, &base, Path::new(arg(2)?), &name)?;
      println!("{} at {} ({written} git objects written) in {:.0} ms", arg(3)?, hex(&oid), ms(started));
    }
    "adopt" if args.get(2).is_some_and(|a| a == "--undo") => println!("{}", newvcs::colocated::unadopt(Path::new(arg(1)?))?),
    "adopt" => println!("{}", newvcs::colocated::adopt(Path::new(arg(1)?))?),
    "shell-init" => print!("{}", newvcs::colocated::SHELL_INIT),
    "name" => {
      let mut store = Store::open(Path::new(arg(1)?))?;
      let commit = store.resolve(arg(3)?).ok_or_else(|| format!("unknown revision `{}`", args[3]))?;
      newvcs::links::name(&store, arg(2)?, commit)?;
    }
    "names" => {
      let store = Store::open(Path::new(arg(1)?))?;
      let names = newvcs::links::names(&store);
      for (name, id) in &names {
        println!("{name} {}", hex(id));
      }
      println!("everything {}", hex(&newvcs::links::everything(&names)));
    }
    "link" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      let sub = args.get(4).map_or("", String::as_str);
      let paths = newvcs::links::link(&mut store, &mut ws, arg(2)?, arg(3)?, sub, AUTHOR)?;
      println!("linked {} to {}: {paths} paths, in {:.1} ms", arg(2)?, arg(3)?, ms(started));
    }
    "unlink" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      newvcs::links::unlink(&mut store, &mut ws, arg(2)?, AUTHOR)?;
      println!("{} is no longer linked: its files stay, as files of this repository", arg(2)?);
    }
    "links" => {
      let (mut ws, mut store) = opened(arg(1)?)?;
      if args.get(2).is_some_and(|a| a == "--update") {
        for (path, paths) in newvcs::links::update(&mut store, &mut ws, AUTHOR)? {
          println!("{path}: {paths} paths");
        }
      } else {
        for s in newvcs::links::links(&mut store, &ws)? {
          let how = match (s.known, s.as_linked, s.up_to_date) {
            (false, _, _) => "that repository is not in this store: nothing to compare with".to_string(),
            (_, here, there) => {
              format!("{}, {}", if here { "as linked" } else { "changed here" }, if there { "up to date" } else { "behind" })
            }
          };
          println!("{}\t{}@{} {}\t{how}", s.link.path, s.link.name, &hex(&s.link.commit)[..12], s.link.sub);
        }
      }
    }
    "stats" => print_stats(&Store::open(Path::new(arg(1)?))?),
    _ => return Err(USAGE.to_string()),
  }
  Ok(())
}

/// `1 commit`, `2 commits`.
fn counted(n: usize, what: &str) -> String {
  if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

fn describe(s: &remote::Synced, root: &Path) -> String {
  let nothing = s.commits == 0 && s.written == 0 && s.removed == 0 && s.contents_fetched == 0 && s.outside_view == 0 && s.merged == 0;
  if nothing && s.conflicts.is_empty() && s.cancelled == 0 && s.superseded == 0 && s.folded <= 1 && s.before.is_none() {
    return "nothing else came from the ref".to_string();
  }
  let mut text = format!(
    "caught up {}: {} files written, {} removed, {} contents fetched, {} changes outside the view left alone",
    counted(s.commits, "commit"),
    s.written,
    s.removed,
    s.contents_fetched,
    s.outside_view
  );
  if s.merged > 0 {
    text.push_str(&format!("; {} files both sides changed were merged", s.merged));
  }
  if !s.conflicts.is_empty() {
    text.push_str(&format!(
      "\nboth sides changed the same lines of: {}\nEach holds both versions between <<<<<<< and >>>>>>> markers, and is left out of the commit put on the ref's tip. {}",
      s.conflicts.join(", "),
      if newvcs::checks::load(root).ok().flatten().is_some_and(|c| c.commit_each) {
        "Edit them: once the conditions pass on them, they go into the commit of yours put on the tip, which they complete (where you have none, they are a commit of their own). Then `nv push`."
      } else {
        "Edit them, then `nv commit` and `nv push`."
      }
    ));
  }
  if s.cancelled > 0 {
    text.push_str(&format!(
      "\n{} commits made here and not yet pushed undid each other: there was nothing of them to put on the ref, and they are not on it",
      s.cancelled
    ));
  }
  if s.superseded > 0 {
    text.push_str(&format!(
      "\n{} commits made here and not yet pushed are dropped: the ref already has the changes they made, under commits of its own",
      s.superseded
    ));
  }
  if s.folded > 1 {
    text.push_str(&format!(
      "\n{} commits made here and not yet pushed are now one commit on top of the ref's, under the message of the last",
      s.folded
    ));
  }
  if s.before.is_some() {
    text.push_str("\nthe workspace as it was before is kept: `nv restore <dir> before-sync` undoes this catching-up, files and all");
  }
  text
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

/// Every command but `git` and `hook`: how many arguments it takes besides options, and its options.
const COMMANDS: &[(&str, usize, usize, &[&str])] = &[
  ("import", 2, 4, &[]),
  ("verify", 1, 1, &[]),
  ("checkout", 3, usize::MAX, &[]),
  ("status", 1, 1, &[]),
  ("record", 2, usize::MAX, &[]),
  ("snapshot", 1, 1, &["--recorded", "--tracked"]),
  ("commit", 1, 2, &["--recorded", "--tracked", "--no-checks"]),
  ("checkpoint", 1, 1, &["--until-still"]),
  ("note", 2, 2, &[]),
  ("publish", 1, 2, &["--message"]),
  ("pending", 1, 1, &["--wait"]),
  ("rebuild", 3, usize::MAX, &[]),
  ("watch", 1, 1, &["--start", "--stop", "--status"]),
  ("log", 1, 1, &[]),
  ("restore", 2, 2, &["--recorded", "--tracked"]),
  ("mark", 2, 2, &["--recorded", "--tracked"]),
  ("since", 2, 2, &["--recorded", "--tracked"]),
  ("agent-init", 1, 1, &[]),
  ("remote", 3, 3, &[]),
  ("push", 1, 1, &[]),
  ("sync", 1, 1, &[]),
  ("fork", 2, 2, &[]),
  ("export", 3, 3, &[]),
  ("adopt", 1, 1, &["--undo"]),
  ("check", 1, 1, &[]),
  ("reconcile", 1, 1, &[]),
  ("shell-init", 0, 0, &[]),
  ("name", 3, 3, &[]),
  ("names", 1, 1, &[]),
  ("link", 3, 4, &[]),
  ("unlink", 2, 2, &[]),
  ("links", 1, 1, &["--update"]),
  ("stats", 1, 1, &[]),
];

/// The lines of the usage text that are about one command.
fn usage_of(command: &str) -> String {
  let mut lines = USAGE.lines().skip_while(|line| {
    !line.starts_with(&format!("  nv {command}")) || line[5 + command.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '-')
  });
  let first = lines.next().unwrap_or_default();
  std::iter::once(first).chain(lines.take_while(|line| !line.starts_with("  nv "))).collect::<Vec<_>>().join("\n")
}

/// The arguments as the commands expect them (options last), or what is wrong with them.
/// `Ok(None)`: the question was answered here, by printing help.
fn shaped(args: &[String]) -> std::result::Result<Option<Vec<String>>, String> {
  let Some(command) = args.first().map(String::as_str) else { return Err(USAGE.to_string()) };
  match command {
    "help" | "--help" | "-h" => {
      match args.get(1).map(String::as_str) {
        Some(about) if about == "hook" || about == "git" || COMMANDS.iter().any(|(name, ..)| *name == about) => {
          println!("{}", usage_of(about))
        }
        Some(about) => return Err(format!("nv: `{about}` is not a command. `nv help` lists them.")),
        None => println!("{USAGE}"),
      }
      return Ok(None);
    }
    "version" | "--version" | "-V" => {
      println!("nv {}", env!("CARGO_PKG_VERSION"));
      return Ok(None);
    }
    _ => {}
  }
  let Some((_, least, most, options)) = COMMANDS.iter().find(|(name, ..)| *name == command) else {
    return Err(format!("nv: `{command}` is not a command. `nv help` lists them."));
  };
  let (mut plain, mut given, mut literal) = (vec![command.to_string()], Vec::new(), false);
  for arg in &args[1..] {
    if literal || !arg.starts_with('-') || arg == "-" {
      plain.push(arg.clone());
    } else if arg == "--" {
      literal = true;
    } else if arg == "--help" || arg == "-h" {
      println!("{}", usage_of(command));
      return Ok(None);
    } else if options.contains(&arg.as_str()) {
      given.push(arg.clone());
    } else {
      // Refused, not ignored: `--recoded` for `--recorded` would otherwise quietly do the slow thing.
      let known = if options.is_empty() { "it takes none".to_string() } else { format!("it takes {}", options.join(", ")) };
      return Err(format!("nv {command}: unknown option `{arg}` ({known})\n{}", usage_of(command)));
    }
  }
  let count = plain.len() - 1;
  if count < *least || count > *most {
    let wanted = if count < *least { "too few arguments" } else { "too many arguments" };
    return Err(format!("nv {command}: {wanted}\n{}", usage_of(command)));
  }
  if given.iter().filter(|option| *option == "--recorded" || *option == "--tracked").count() > 1 {
    return Err(format!("nv {command}: --recorded and --tracked are two ways of looking; give one\n{}", usage_of(command)));
  }
  plain.extend(given);
  Ok(Some(plain))
}

fn main() -> ExitCode {
  // Output cut short by `| head` is the reader's choice, not a failure to report.
  // SAFETY: called once, before any thread or output exists.
  unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
  let args: Vec<String> = std::env::args().skip(1).collect();
  // Every command has a limit to how long it runs. A watcher is no command but a process that
  // stays; the step that makes a commit beside Git has its own limit, on Git.
  let stays = args.first().is_some_and(|a| a == "watch") && args.len() == 2;
  if !stays && args.first().is_none_or(|a| a != "publish-step") {
    newvcs::limits::watch_the_clock(&args.iter().take(2).cloned().collect::<Vec<_>>().join(" "));
  }
  // `nv git …` answers as Git would, exit code and all.
  if args.first().is_some_and(|a| a == "git") {
    return ExitCode::from(newvcs::gitcli::run(&args[1..]) as u8);
  }
  // Not a command for a person: the part of publishing beside Git that runs as its own process.
  if args.first().is_some_and(|a| a == "publish-step") {
    return ExitCode::from(newvcs::colocated::publish_step(&args[1..]) as u8);
  }
  // Started by a hook of a commit nv is publishing, on the workspace it is publishing: that
  // publishing holds the workspace, and waiting for it would be waiting for oneself.
  if let Some(publishing) = std::env::var_os("NV_PUBLISHING")
    && let Some(top) = args
      .get(1)
      .and_then(|dir| workspace::top_of(Path::new(dir)))
      .or_else(|| args.first().filter(|a| *a == "hook" || *a == "git").and_then(|_| workspace::top_of(Path::new("."))))
    && std::fs::canonicalize(&top).is_ok_and(|top| top.as_os_str() == publishing)
  {
    eprintln!(
      "nv: this workspace is in the middle of `nv commit`, which started the Git hook this command runs in; nv cannot be used on it from inside that commit"
    );
    return ExitCode::from(2);
  }
  if args.first().is_some_and(|a| a == "hook") {
    if args.get(1).is_some_and(|a| a == "claude") {
      claude_hook();
      return ExitCode::SUCCESS;
    }
    eprintln!("nv hook: the hook of which tool? `nv hook claude` is the one there is\n{}", usage_of("hook"));
    return ExitCode::from(2);
  }
  let args = match shaped(&args) {
    Ok(Some(args)) => args,
    Ok(None) => return ExitCode::SUCCESS,
    Err(why) => {
      eprintln!("{why}");
      return ExitCode::from(2);
    }
  };
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
  // A watcher that cannot say it has is not trusted to have: the command then looks at every file itself.
  let mut args = args;
  if let Some(top) = args.get(1).filter(|_| args.iter().any(|a| a == "--recorded")).and_then(|dir| workspace::top_of(Path::new(dir)))
    && !newvcs::watch::settle(&top)
  {
    args.retain(|a| a != "--recorded");
  }
  // A commit where conditions are declared is of a state that passed them. They run before the
  // workspace is held: they can take minutes, and each run holds it only to record and to name.
  if args[0] == "commit"
    && !args.iter().any(|a| a == "--no-checks")
    && let Some(top) = workspace::top_of(Path::new(&args[1]))
  {
    match newvcs::checks::load(&top) {
      Err(why) => {
        eprintln!("{why}");
        return ExitCode::from(2);
      }
      Ok(Some(config)) if !config.checks.is_empty() => match newvcs::checks::run(&top, &config, AUTHOR) {
        Ok(run) if run.passed() => {
          let _ = GATED.set(run.state);
        }
        Ok(run) => {
          eprint!("{}", run.said());
          eprintln!(
            "nv: not committed: the state does not pass the conditions {} declares (`--no-checks` commits it all the same)",
            newvcs::checks::FILE
          );
          return ExitCode::from(1);
        }
        Err(why) => {
          eprintln!("{why}");
          return ExitCode::from(2);
        }
      },
      Ok(_) => {}
    }
  }
  // Waiting for the watcher and the conditions is done before the workspace is held: both
  // take it to record and to commit.
  if (args[0] == "publish" || args[0] == "pending" && args.iter().any(|a| a == "--wait"))
    && let Some(top) = workspace::top_of(Path::new(&args[1]))
  {
    match newvcs::checks::wait_settled(&top) {
      Ok(line) => {
        let _ = SETTLED.set(line);
      }
      Err(why) => {
        eprintln!("{why}");
        return ExitCode::from(2);
      }
    }
  }
  // One writer at a time in a workspace. A watcher takes the lock for each snapshot instead, not for as long as it runs.
  let _lock = match (args.first().map(String::as_str), args.get(1)) {
    (Some(command), Some(dir)) if command != "watch" && command != "fork" && command != "checkpoint" => {
      match workspace::top_of(Path::new(dir)).map(|top| workspace::lock(&top)) {
        Some(Ok(held)) => Some(held),
        // Another command holds the workspace and did not let go in time: this one does not go
        // ahead without it, which would be two writers at once.
        Some(Err(why)) if why.contains("gave up waiting") => {
          eprintln!("{why}");
          return ExitCode::from(2);
        }
        // A lock file that cannot be made (a read-only `.nv`): the command says what it can and cannot do itself.
        _ => None,
      }
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
