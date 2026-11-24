# Working on nv on Linux

2026-11-21. A 4-core Linux VM (Firecracker), ext4, Git 2.43, Rust 1.97, Node 22. nv had been
developed and tested on macOS.

## The problem

Working through `TODO.md` means many small fix-and-test cycles. Two things made each cycle
slow, and one made its result hard to read:

- The release profile is built with fat LTO and one codegen unit. A clean build took 1 min
  59 s; touching one file and building again, 1 min 2 s. `pkf run ci` runs the tests on that
  profile.
- 12 tests failed before anything was changed, on a clean checkout. A failure introduced by a
  change could hide behind one of them, and a test that fails early skips everything after
  the line that failed.

## What was done

**A `fast` profile** (`Cargo.toml`): release, without LTO, 16 codegen units, incremental. A
change rebuilds in about a second. The tests pass on it as on release. Every number in the
README is still measured on release; benchmarks used release builds.

**A runner for the whole suite.** It builds the tests once with `--no-run` and runs every test
binary at the same time, as an unprivileged user (`setpriv --reuid=65534`), against the local
server when it is up. It prints only the failures that are not on a list of known ones. The
suite takes about 14 s this way. The runner was kept in the session's scratch directory, not
in the repository.

**Why the 12 failed**, and what became of each:

| Cause | Tests | Done |
| --- | ---: | --- |
| `chmod 000` does not stop root, and the tests ran as root | 3 | Run as another user; the README says so |
| macOS-only shell: `stat -f`, `wc` and `od` spacing, `sed -i ''`, `cp -c` | 5 | Made portable |
| A global Git identity assumed (`user.name` from the developer's config) | 1 | Set in the test |
| No copy-on-write clones on ext4: a copy had every file read again | 3 | Fixed in nv (see the tuning report): copies keep file times |
| Git 2.43 takes `log -n abc`; newer Git refuses it (case 27) | 2 | Left: the case is right for current Git |
| A stopped hook's process can still be seen for a moment, until it is reaped | 1 | Left: flaky here, depends on PID 1 |

By the end 3 failures were left, all of them environmental: case 27, the test that runs the
Git cases (and so fails with it), and the hook check now and then.

**The server.** `server/` runs locally with `pnpm dev -- --port 8799` (`cf dev`, the
Cloudflare Vite plugin's local runtime; nothing deployed), and the remote and lying-server tests run against it. They had been
skipping silently: without `NV_SERVER` they pass.

## What was learned

- The dev server keeps a CPU busy even when idle (workerd). Benchmarks were run with it
  stopped. Timing tests (kill loops with fixed sleeps, `sleep 1.2` for file times) failed now
  and then while a model checker or the whole suite ran in parallel, and passed run alone.
  They are not wrong, but they are a reason to read a failure twice before believing it.
- Comparing against the code before a change needs that code built. A `git worktree` of the
  base commit, built with its own `CARGO_TARGET_DIR`, took one incremental build and was used
  throughout to tell new failures from old ones and to measure before and after.
- `/tmp/claude-0` was recreated with mode 700 once during the session, and the unprivileged
  test user lost access to scripts there. Scripts for it are best kept in a directory whose
  mode is set by the script itself.
