# Tenants per VM

This bench prices a hosted offer. It compares 2 layouts on 1 machine:

- **per-process**: 1 `atticdb serve` and 1 data dir per tenant.
- **shared**: 1 `atticdb serve` with N tenants in 1 config and 1 data dir.

Design 0009 section 0c explains why. The verdict comes from the Azure run in Task 4. This file holds the runbook and a local shape check.

## Runbook

You need Python 3 (stdlib only) and a release build.

```sh
cargo build --release -p atticdb
export ATTICDB_BIN=$PWD/target/release/atticdb
# Optional. Run dirs go here. Default is bench/.tb. Keep it short, see "Socket paths".
export TENANTS_BASE=/mnt/nvme/tb
# Linux at N=1,000: raise the fd and thread limits first.
ulimit -n 65536
ulimit -u 65536   # counts threads. Keep it well above N * 10.
```

1 run, 1 TSV row:

```sh
python3 bench/tenants.py --layout shared --tenants 10 --active-pct 20 \
    --duration 20 --think-ms 50 --out bench/tenants-smoke.tsv
```

The sweep used for the local shape check:

```sh
for layout in shared per-process; do for n in 1 10 100; do for pct in 1 5 20; do
  python3 bench/tenants.py --layout $layout --tenants $n --active-pct $pct \
      --duration 60 --think-ms 50 --out bench/tenants-local-mac.tsv
done; done; done
```

The script appends 1 row per run. It writes the header when the file is new. A failed run writes no row and exits non-zero.

## What 1 run does

1. Each tenant gets a random 32-hex token. Each config is written mode 0600 with `segment_mib: 16`.
2. per-process starts N `atticdb serve --listen none --unix-socket <dir>/s`. shared starts 1.
3. It waits until every server answers `GET /v1/facts/get?key=x` with 200.
4. It waits 5 s, then takes the idle sample.
5. `round(N * active_pct / 100)` tenants go active, at least 1. The pick is seeded, so it repeats.
6. Each active tenant gets 1 client process with 1 request in flight.
7. Before each op the client sleeps a seeded uniform 0 to 2x `--think-ms`.
8. Ops alternate `POST /v1/append` and `POST /v1/facts/put`. Each carries 256 bytes and an idem key.
9. Fact puts pick 1 of 16 keys.
10. 2 s before the end of the load it takes the load sample. Clients are still connected then.
11. per-process only: it stops tenant 0's `serve`, spawns it again and times the first 200.
12. Teardown stops every `serve` it started, by pid. Then it removes the run dir.

SIGTERM and Ctrl-C still run the teardown. A SIGKILL can leave servers and `bench/.tb` behind.

## Columns

| Column | Meaning |
|---|---|
| `layout` | `per-process` or `shared` |
| `tenants` | N |
| `active_pct` | share of tenants with a client |
| `ops` | ops completed by all clients |
| `p50_ms`, `p99_ms` | nearest-rank percentile over every op of every active tenant |
| `rss_mib_idle` | RSS summed over every `serve` pid, after 5 s idle |
| `rss_mib_load` | RSS summed over every `serve` pid, 2 s before the load ends |
| `fds` | open fds summed over every `serve` pid, in the load sample |
| `threads` | threads summed over every `serve` pid, in the load sample |
| `disk_mib_per_tenant` | `du -sk` of all data dirs, divided by N |
| `cold_start_ms` | per-process only: spawn to first 200 on an existing data dir. `-` for shared |

Latency is the client round trip on the same host. The timer starts before the send and stops after the full reply. Think time is outside it. Design 0009 sets its bar on server-side p99. The round trip is an upper bound on that.

RSS comes from `ps -o rss=` on both systems. fds and threads differ:

- **Linux**: `/proc/<pid>/fd` entries and the `Threads:` line of `/proc/<pid>/status`.
- **macOS**: `lsof -nP -F f -p <pids>`, counting only numbered fds. cwd, txt and mem rows are skipped. Threads come from `ps -M -p <pids>`, 1 line per thread after the header.

Every sample checks that each `serve` is still alive. A dead server fails the run.

## Socket paths

macOS limits a Unix socket path to 104 bytes. Linux allows 108. The socket is `$TENANTS_BASE/<8 hex>/t<i>/s`. The script refuses a path over 103 bytes. On the Mac the default base gave about 60 bytes at N=100.

## Limits

The script raises its soft `RLIMIT_NOFILE` to min(hard, 65536) when it's lower. Children inherit it. On the Mac these limits applied, and none was hit:

| Limit | Value |
|---|---|
| `ulimit -n` (soft) | 1048575 |
| `kern.maxfilesperproc` | 61440 |
| `kern.maxprocperuid` | 2666 |
| `kern.num_taskthreads` | 4096 |

For N=1,000 on Linux, check `ulimit -u` first. It counts threads, not processes. per-process at N=1,000 needs about 7,000 `serve` threads plus clients. Also check free disk. per-process uses about 34 MiB per tenant, so 34 GiB at N=1,000.

## Local results

**Local shape check, not the verdict.** The Mac is not the target VM. Latency includes macOS fsync on an Apple SSD.

Machine: MacBook Air (Mac16,12), Apple M4, 10 cores (4 performance, 6 efficiency), 16 GB RAM. macOS 27.2 (build 26B123). Internal APFS SSD, 494 GB. Python 3.14.6. `atticdb 0.1.0+dev` release build at commit 243140f.

Command: the sweep above with `ATTICDB_BIN=target/release/atticdb` and the default base. 18 runs of 60 s took 1,189 s in total. Raw rows: `bench/tenants-local-mac.tsv`.

| layout | N | active % | ops | p50 ms | p99 ms | RSS idle MiB | RSS load MiB | fds | threads | disk MiB/tenant | cold start ms |
|---|---|---|---|---|---|---|---|---|---|---|---|
| shared | 1 | 1 | 1020 | 4.440 | 6.220 | 3.6 | 5.5 | 11 | 8 | 34.11 | - |
| shared | 1 | 5 | 1021 | 4.427 | 6.162 | 3.6 | 5.5 | 11 | 8 | 34.11 | - |
| shared | 1 | 20 | 1020 | 4.408 | 5.818 | 3.6 | 5.4 | 11 | 8 | 34.11 | - |
| shared | 10 | 1 | 1014 | 4.410 | 6.464 | 3.6 | 5.3 | 11 | 8 | 3.41 | - |
| shared | 10 | 5 | 974 | 4.491 | 6.414 | 3.6 | 5.4 | 11 | 8 | 3.41 | - |
| shared | 10 | 20 | 1954 | 4.354 | 8.235 | 3.6 | 6.5 | 13 | 9 | 3.42 | - |
| shared | 100 | 1 | 990 | 4.454 | 6.095 | 3.7 | 5.5 | 11 | 8 | 0.34 | - |
| shared | 100 | 5 | 4909 | 4.354 | 8.480 | 3.7 | 8.8 | 19 | 12 | 0.34 | - |
| shared | 100 | 20 | 19512 | 5.073 | 8.947 | 3.7 | 19.7 | 49 | 27 | 0.36 | - |
| per-process | 1 | 1 | 973 | 4.474 | 5.948 | 3.6 | 5.3 | 11 | 8 | 34.11 | 53.0 |
| per-process | 1 | 5 | 971 | 4.444 | 6.026 | 3.6 | 5.5 | 11 | 8 | 34.11 | 63.4 |
| per-process | 1 | 20 | 977 | 4.418 | 6.515 | 3.6 | 4.7 | 11 | 8 | 34.11 | 59.2 |
| per-process | 10 | 1 | 986 | 4.551 | 6.454 | 36.0 | 37.9 | 83 | 71 | 34.02 | 55.8 |
| per-process | 10 | 5 | 987 | 4.545 | 6.254 | 37.3 | 39.1 | 83 | 71 | 34.02 | 50.7 |
| per-process | 10 | 20 | 1974 | 4.445 | 9.111 | 37.0 | 40.3 | 86 | 72 | 34.03 | 58.6 |
| per-process | 100 | 1 | 1049 | 4.453 | 6.355 | 286.4 | 291.2 | 803 | 701 | 33.37 | 50.5 |
| per-process | 100 | 5 | 5190 | 4.332 | 11.209 | 366.7 | 375.8 | 815 | 705 | 33.70 | 59.3 |
| per-process | 100 | 20 | 19015 | 10.007 | 18.346 | 367.4 | 402.2 | 860 | 720 | 33.39 | 53.6 |

What the shape shows:

- Every cell held p99 under 20 ms. The 50 ms bar from design 0009 was never close.
- An idle `serve` costs about 3 to 4 MiB RSS, 8 fds and 7 threads.
- per-process scales that cost by N. At N=100 it held 286 to 367 MiB idle against 3.7 MiB shared.
- shared adds about 2 fds and 1 thread per open connection. That matches 1 thread per connection.
- per-process disk is 34 MiB per tenant. shared spreads 1 tape, so it's 0.34 MiB at N=100.
- At N=100 and 20% active, per-process p50 doubled to 10 ms. shared stayed at 5 ms.
- Cold start on an existing dir took 50 to 63 ms.

The p50 jump is a hypothesis, not a finding. 20 servers fsync on their own, while 1 shared server may batch them. The Linux run should show whether it holds on NVMe.

A first sweep sampled fds and threads after the clients closed. That missed the connection threads, so it was discarded and rerun. Its shared N=1 cells also showed p99 spikes of 130 to 320 ms. They did not repeat in the rerun.
