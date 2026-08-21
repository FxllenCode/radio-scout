# Leaving the machine as you found it

Two things this repo's own ritual produces, that nothing in it cleans up: **background shells that
outlive what they were watching**, and **build trees that grow without bound**. Both were found on
2026-08-21 with one stale `cargo mutants`, one orphaned polling loop, and **55 GB** in `target/` —
on a laptop, from one repository.

Neither is a tooling bug. Both are the direct consequence of the loop
[CLAUDE.md](../../CLAUDE.md#enforcement) prescribes per ticket: run the suite repeatedly, measure
coverage, sweep mutants. This file is the other half of that loop.

## Shells

**The completion notification *is* the wait.** A command started with `run_in_background` re-invokes
the agent when it exits. A second shell that sits in `until …; do sleep 20; done` waiting for the
same thing is not a safety net — it is a process nobody will ever stop.

Three rules, and the third is the one that actually bit:

- **Never background a `sleep`/poll loop to wait on another background command.** Wait for the
  notification. If there is genuinely external state to watch — a container coming up, a remote job
  — use `Monitor`, which takes a `timeout_ms` and therefore *ends*. An unbounded `until` in Bash
  does not.
- **One watcher per thing watched, at most.** Polling the same job again on the next turn starts a
  *second* immortal loop; five turns of "are we there yet" leaves five.
- **Killing a job orphans its watchers.** A loop waiting for `mutants.out/outcomes.json` to gain an
  `end_time` waits *forever* once the run producing it has been killed and its output deleted — the
  condition it is waiting for can no longer occur. **Restarting a long job means stopping its
  watchers first**, and `pkill`ing the job is not enough.

Before reporting a ticket finished, check:

```bash
ps -eo pid,etime,command | grep shell-snapshots | grep -v grep   # agent-launched shells
pgrep -fl 'cargo|rustc|mutants'                                  # builds nobody is waiting for
```

Anything still running that you are not actively reading output from is a leak.

## Disk

`cargo` never garbage-collects `target/`. Nothing ages out, so every rebuild of a test binary adds
another hashed copy beside the last one — the 2026-08-21 tree held **45 copies** of the same
integration-test binary. Three separate things pile up, and the ritual feeds all three:

| What | Why it grows | Was |
| --- | --- | --- |
| `target/debug/deps` | one artifact per build of each of ~20 test binaries, at `debuginfo=2`, never pruned | 24 GB |
| `target/llvm-cov-target` | `cargo llvm-cov` builds a **second full tree**, instrumented, so coverage cannot poison the normal build | 16 GB |
| `target/debug/incremental` | incremental-compilation cache, per crate per profile | 14 GB |
| `$TMPDIR/cargo-mutants-*.tmp` | one copied worktree + `target/` per run; removed on a clean exit, **left behind when the run is killed** | 31 MB each |

**After a ticket's gates pass**, reclaim it:

```bash
cargo clean -p radio-scout          # our crate's artifacts only — the stale copies
rm -rf target/llvm-cov-target target/debug/incremental target/doc
rm -rf mutants.out mutants.out.old "${TMPDIR}"cargo-mutants-*.tmp
```

**`cargo clean -p`, never a bare `cargo clean`.** The package-scoped form took the same tree from
55 GB to 2.5 GB while leaving every third-party dependency compiled, so the next build was 21
seconds. A bare `cargo clean` reclaims a little more and costs a full cold rebuild of `aws-lc-sys`,
`ring`, `symphonia` and the rest — minutes, for no benefit.

`target/llvm-cov-target` and `target/debug/incremental` are both rebuilt on demand: deleting them
costs one slower run of the thing that needs them, once.

## Docker

**The largest single leak found, by two orders of magnitude: 373 GB in 50 orphaned volumes.**

`docker rm -f <container>` removes the container and **keeps its anonymous volumes**. The `postgres`
image declares a `VOLUME` for its data directory, so every `docker run` of it creates one — and the
teardown line this repo documented for years (`docker rm -f rs-pg`) left it behind. Fifty
dual-dialect sessions since July 2026 meant fifty abandoned Postgres data directories, none attached
to any container, none named, invisible to `docker ps -a`.

Each was ~7.5 GB rather than the ~50 MB a fresh one would be, because the suite creates a database
per test (`rs_test_<uuid>`, one per each of ~2 200 tests) and [deliberately never drops
them](dual-dialect.md): the server is a throwaway, so the databases do not matter. They stop not
mattering the moment the volume outlives the server.

**The fix is one letter, and it is now in every documented teardown**: `docker rm -fv`. To check the
damage on any machine:

```bash
docker system df                       # "Local Volumes … RECLAIMABLE" is the number
docker volume ls -qf dangling=true     # every one of these is unattached
docker volume prune -af                # and this is the cure
docker builder prune -af               # build cache, separately
```

Docker Desktop TRIMs its VM disk after a prune, so the host really does get the space back — the
image went 363 GB → 20 GB immediately. Images were left alone: 7 GB of `postgres:17-alpine` and
friends is cheap to keep and tedious to re-pull.

## Why this is not a hook

Both halves are cheap to do and easy to forget, which is an argument for automating them — and the
reason not to is that each one has a case where doing it automatically would be wrong. A `target/`
sweep between two runs of the suite would turn a 20-second loop into a 4-minute one; a shell reaper
cannot tell a leaked poller from a deliberate long-running watch. So they live here, at the end of
the ticket loop, next to the commit-push-close that is already a ritual.
