# Serve LOCK Watch and Test Sweep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `atticdb serve` exits when its `LOCK` or data folder is removed or replaced, and the Rust integration tests clean up servers that killed test runs left behind.

**Architecture:**
- Task 1 adds a watch thread to `serve`. It checks every 2 s that the path `<data_dir>/LOCK` still has the device and inode it had at start. If not, it stops the server the way the poison exit does, and `atticdb serve` exits 75.
- Task 2 adds a 1-time sweep to the test support's temp root.

**Tech Stack:** Rust, rustix.

**Spec:** GitHub issue Clawdlinux/atticdb#227.

## Global Constraints

- Unix only for Task 1. On Windows a held `LOCK` can't be deleted, so the watch is `#[cfg(unix)]`.
- Check interval: 2 s, a named constant `LOCK_CHECK` in serve.rs.
- Lock lost means either case below:
  - `std::fs::symlink_metadata(<data_dir>/LOCK)` fails with `NotFound`;
  - it succeeds with a different `(dev, ino)` than the held lock (see Ruling 1).

  Any other error is ignored for that tick.
- On lock lost: stop accepting, wait for requests in flight, remove the socket path only if it is still ours (`UnixListener` `remove`, as the poison exit does), then mark exited. Write 1 stderr line, exactly: `atticdb: <data_dir>/LOCK was removed or replaced, exiting so 2 servers can't share the data folder`. `atticdb serve` exits `EXIT_LOCKED` (75). Update that constant's doc to cover this.
- The watch runs whether or not `--exit-on-poison` is set.
- The poison exit keeps its own message and exit 70.
- Task 2 never signals a process unless its `LOCK` names it and `serves(pid, data)` matches, the same check `stop_servers` uses. It never touches a folder whose name doesn't parse as `<hex pid>-<n>`, or whose pid is still running, or that is this process's own.
- Prose (comments, docs): no em dashes, sentences under 20 words, numbers as digits.
- No new dependencies. Commits use `git commit -s`, no Co-authored-by trailer.

## Rulings

- Ruling (revised after the PR #228 review): the expected `(dev, ino)` comes from the held lock handle, not a path snapshot. `LockGuard` records it, and the tape and `Engine::lock_identity` expose it. Each check compares a path stat with it, once before serving and then every 2 s. Why: a snapshot taken at the start of `run` adopts a `LOCK` replaced before it, and then passes forever. Cost: 1 small accessor in each of 3 crates. A manual `LOCK` delete still leaves up to 2 s where 2 servers could overlap.
- Ruling: exit 75, not a new code. Why: `Launch` already treats 75 as "retry the spawn", which is the right reaction. Cost if wrong: a log reader confuses this exit with a lost lock race. The stderr line tells them apart.

## Review Focus

1. Deleting the whole data folder (not just `LOCK`) makes the server exit within about 2 s.
2. Replacing `LOCK` with a new file at the same path counts as lost.
3. A server whose `LOCK` stays put never exits from the watch. An existing long test must not flake.
4. The sweep never kills a process whose pid was reused by something else.
5. The sweep with `ATTICDB_TEST_DIR` set uses that root, not `/tmp`.

---

### Task 1: serve watches its LOCK

**Files:**
- Modify: `crates/atticdb/src/serve.rs` (`ServeConfig`, `Shared`, `run`, `try_start`, `EXIT_LOCKED` doc)
- Modify: every `ServeConfig { .. }` literal (find them with grep; tests included), adding the new field
- Modify: `crates/atticdb/README.md` (the `atticdb serve` exit codes and behavior), `docs/design/0002-decision-log.md` (D114)
- Test: `crates/atticdb/tests/it/` (the file that tests `atticdb serve` as a process; find it with grep for `EXIT_LOCKED` or `"serve"`)

**Interfaces:**
- Produces: `ServeConfig.lock_path: Option<PathBuf>`. `None` means no watch (tests that call `run` with a sim engine). `try_start` sets it to `<data_dir>/LOCK`.
- `Server::wait` returns why the server stopped. Use a small enum, `Exit { Poisoned, LockLost }`, or the exit code. `try_start` maps it to 70 or 75 and its message.

- [ ] **Step 1: Write failing tests**

Integration tests, each starting the real `atticdb serve` on a temp data folder and waiting for its "serving" line:
- Remove `LOCK`. The process exits 75 within 10 s, and its stderr holds the exact line.
- Remove the whole data folder. Same.
- Replace `LOCK`: remove it, write a new file at the path. Same.
- With `LOCK` untouched for 5 s, the process is still running and answers `/healthz`. Then stop it.

- [ ] **Step 2: Run, confirm they fail**

Run: `timeout 900 cargo test -p atticdb --test it serve`

- [ ] **Step 3: Implement** as in Global Constraints and Interfaces. The watch thread is named `atticdb-lockwatch`. It exits when `stopping` is set, so `shutdown` still joins every thread.

- [ ] **Step 4: Run all tests**

Run: `timeout 900 cargo test -p atticdb`, `cargo clippy -p atticdb --all-targets -- -D warnings`, `cargo fmt --check`.

- [ ] **Step 5: Docs and commit**

D114: `serve` exits 75 when its `LOCK` or data folder is removed or replaced, checked every 2 s on Unix. Why: a server with no `LOCK` can't be found, and a new one could start beside it on the same data (#227). Rejected: an `fstat` on the held handle, which lives in atticdb-tape behind `Io`.

`git commit -s -m "fix(serve): exit when LOCK or the data folder is removed (#227)"`

### Task 2: tests sweep leftovers from killed runs

**Files:**
- Modify: `crates/atticdb/tests/it/support.rs` (`tmp_root` or `Dir::new`, plus a new `sweep` function)
- Test: a unit-style test in `crates/atticdb/tests/it/` for the sweep

**Interfaces:**
- Consumes: `stop_servers(&[PathBuf])`, `lock_pid`, `serves`, `locked` in support.rs.
- Produces: `fn sweep(root: &Path)`, run once per test process through a `std::sync::OnceLock` the first time the root is resolved, whether it came from `tmp_root` or `ATTICDB_TEST_DIR`.

- [ ] **Step 1: Write failing tests**

- Under a fresh temp root, make `<hex of a dead pid>-0/x/data` with a running `atticdb serve` on it. Get a dead pid by spawning `true` and waiting for it. `sweep(root)` stops that server and removes the folder.
- `<hex of this process's pid>-0` is left alone.
- A folder named `notes` is left alone.
- `<hex of a running pid>-0` is left alone. Use a spawned `sleep 30`, then kill it.

- [ ] **Step 2: Run, confirm fail**

Run: `timeout 900 cargo test -p atticdb --test it sweep`

- [ ] **Step 3: Implement.** Walk at most 6 levels for `LOCK` files. Pass their parent data folders to `stop_servers`. Then `remove_dir_all` the top folder. Errors are ignored: the sweep is best effort and must never fail a test run.

- [ ] **Step 4: Run all tests**

Run: `timeout 900 cargo test -p atticdb`, clippy `-D warnings`, `cargo fmt --check`.

- [ ] **Step 5: Commit**

`git commit -s -m "test: sweep servers that killed test runs left behind (#227)"`
