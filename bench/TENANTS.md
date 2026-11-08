# Tenants per VM

This bench prices a hosted offer. It compares 2 layouts on 1 machine:

- **per-process**: 1 `atticdb serve` and 1 data dir per tenant.
- **shared**: 1 `atticdb serve` with N tenants in 1 config and 1 data dir.

Design 0009 section 0c explains why. This file holds the runbook, the Azure run with the verdict, and a local shape check.

## Runbook

You need Python 3 (stdlib only) and a release build.

```sh
cargo build --release -p atticdb
export ATTICDB_BIN=$PWD/target/release/atticdb
# Run dirs go here. Default is bench/.tb. Keep it short, see "Socket paths".
# Required on a VM for N=1,000: point it at the data disk, not the OS disk.
export TENANTS_BASE=/mnt/tb
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

The script appends 1 row per run. It writes the header when the file is new. It refuses a file whose header has other columns. A failed run writes no row and exits non-zero.

Before it starts any server, it checks free space under `TENANTS_BASE`. per-process needs about N x 35 MiB. shared needs 256 MiB. A short disk fails the run with a message that names `TENANTS_BASE`.

## What 1 run does

1. Each tenant gets a random 32-hex token. Each config is written mode 0600 with `segment_mib: 16`.
2. per-process starts N `atticdb serve --listen none --unix-socket <dir>/s`. shared starts 1.
3. It waits until every server answers `GET /v1/facts/get?key=x` with 200.
4. It waits 5 s, then takes the idle sample.
5. `round(N * active_pct / 100)` tenants go active, at least 1. The pick is seeded, so it repeats.
6. Each active tenant gets 1 client process with 1 request in flight.
7. Before each op the client sleeps a seeded uniform 0 to 2x `--think-ms`.
8. Ops alternate `POST /v1/append` and `POST /v1/facts/put`. Each carries 256 bytes and an idem key. Each sample is tagged with its op.
9. Fact puts pick 1 of 16 keys.
10. 2 s before the end of the load it takes the load sample. Clients are still connected then.
11. per-process only: it stops tenant 0's `serve`, spawns it again and times the first 200. It does this 5 times and keeps the median.
12. Teardown stops every `serve` it started, by pid. Then it removes the run dir.

SIGTERM and Ctrl-C still run the teardown. The first signal makes the script ignore later ones, so a second signal can't cut the teardown short. A SIGKILL can leave servers and `bench/.tb` behind.

## Columns

| Column | Meaning |
|---|---|
| `layout` | `per-process` or `shared` |
| `tenants` | N |
| `active_pct` | share of tenants with a client |
| `ops` | ops completed by all clients |
| `p50_ms`, `p99_ms` | nearest-rank percentile over every op of every active tenant |
| `p50_append_ms`, `p99_append_ms` | the same, over `POST /v1/append` ops only |
| `p50_fact_ms`, `p99_fact_ms` | the same, over `POST /v1/facts/put` ops only. `-` if there were none |
| `rss_mib_idle` | RSS summed over every `serve` pid, after 5 s idle |
| `rss_mib_load` | RSS summed over every `serve` pid, 2 s before the load ends |
| `fds` | open fds summed over every `serve` pid, in the load sample |
| `threads` | threads summed over every `serve` pid, in the load sample |
| `disk_mib_per_tenant` | `du -sk` of all data dirs, divided by N |
| `cold_start_ms` | per-process only: median of 5 spawns to first 200 on an existing data dir. `-` for shared |

Latency is the client round trip on the same host. The timer starts before the send and stops after the full reply. Think time is outside it. Design 0009 sets its bar on server-side p99. The round trip is an upper bound on that.

RSS comes from `ps -o rss=` on both systems. fds and threads differ:

- **Linux**: `/proc/<pid>/fd` entries and the `Threads:` line of `/proc/<pid>/status`.
- **macOS**: `lsof -nP -F f -p <pids>`, counting only numbered fds. cwd, txt and mem rows are skipped. Threads come from `ps -M -p <pids>`, 1 line per thread after the header.

Every sample checks that each `serve` is still alive. A dead server fails the run. If `du` can't size a data dir, the script warns on stderr. It also warns if it can't raise the fd limit.

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

For N=1,000 on Linux, check `ulimit -u` first. It counts threads, not processes. per-process at N=1,000 needs about 7,000 `serve` threads plus clients. Also check free disk. per-process uses about 34 MiB per tenant, so 34 GiB at N=1,000. Set `TENANTS_BASE` to the data disk. The default is under the repo, which is usually on the OS disk.

## Azure results (the verdict)

### Setup

Machine: zdb-g1, Standard_L8s_v3 in Central India. 8 vCPU (Intel Xeon Platinum 8370C, 2.8 GHz), 64 GiB RAM. Ubuntu 24.04.5 LTS, kernel 7.0.0-1017-azure. Python 3.12.3. `atticdb 0.1.0+dev` release build at commit 4c62992, built on the VM with cargo 1.99.1.

Disk: the local NVMe (`/dev/nvme0n1`, 1.7 TiB, "Microsoft NVMe Direct Disk"). Formatted ext4, mounted `noatime` at `/mnt/tb`. Clients and servers ran on the same VM.

Limits: `ulimit -n 65536` and `ulimit -u 200000` in the sweep shell. `kernel.pid_max` was 4194304 and `kernel.threads-max` 476943. No limit was hit.

```sh
sudo mkfs.ext4 -q -F /dev/nvme0n1
sudo mkdir -p /mnt/tb && sudo mount -o noatime /dev/nvme0n1 /mnt/tb
sudo chown azureuser:azureuser /mnt/tb
cargo build --release -p atticdb
ulimit -n 65536; ulimit -u 200000
export ATTICDB_BIN=$PWD/target/release/atticdb TENANTS_BASE=/mnt/tb
for layout in shared per-process; do for n in 1 10 100 1000; do for pct in 1 5 20; do
  timeout -k 30 1200 python3 bench/tenants.py --layout $layout --tenants $n --active-pct $pct \
      --duration 120 --think-ms 50 --out bench/tenants-l8s-v3.tsv
done; done; done
```

The sweep ran from 05:32 to 06:23 UTC. All 24 cells exited 0. Raw rows: `bench/tenants-l8s-v3.tsv`.

### Sweep results

Every cell passed. The worst per-op p99 was 1.058 ms, 47x under the 50 ms bar.

| layout | N | active % | ops | p50 ms | p99 ms | p50 append | p99 append | p50 fact | p99 fact | RSS idle MiB | RSS load MiB | fds | threads | disk MiB/tenant | cold start ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| shared | 1 | 1 | 2344 | 0.477 | 0.887 | 0.500 | 0.941 | 0.452 | 0.826 | 4.9 | 6.5 | 11 | 8 | 34.25 | - |
| shared | 1 | 5 | 2344 | 0.491 | 0.925 | 0.513 | 0.968 | 0.464 | 0.885 | 4.8 | 6.3 | 11 | 8 | 34.25 | - |
| shared | 1 | 20 | 2344 | 0.502 | 0.903 | 0.523 | 0.918 | 0.483 | 0.898 | 4.9 | 6.4 | 11 | 8 | 34.25 | - |
| shared | 10 | 1 | 2342 | 0.498 | 1.001 | 0.518 | 1.037 | 0.471 | 0.942 | 5.0 | 6.5 | 11 | 8 | 3.42 | - |
| shared | 10 | 5 | 2344 | 0.451 | 0.965 | 0.483 | 1.018 | 0.423 | 0.930 | 5.0 | 6.5 | 11 | 8 | 3.42 | - |
| shared | 10 | 20 | 4745 | 0.422 | 0.906 | 0.441 | 0.930 | 0.409 | 0.877 | 4.9 | 7.9 | 13 | 9 | 3.45 | - |
| shared | 100 | 1 | 2364 | 0.466 | 0.928 | 0.495 | 0.986 | 0.441 | 0.873 | 4.9 | 6.5 | 11 | 8 | 0.34 | - |
| shared | 100 | 5 | 11879 | 0.295 | 0.898 | 0.314 | 0.930 | 0.268 | 0.837 | 5.0 | 12.4 | 19 | 12 | 0.35 | - |
| shared | 100 | 20 | 47796 | 0.197 | 0.600 | 0.213 | 0.626 | 0.171 | 0.556 | 5.1 | 34.1 | 49 | 27 | 0.39 | - |
| shared | 1000 | 1 | 23697 | 0.219 | 0.837 | 0.235 | 0.859 | 0.191 | 0.807 | 5.6 | 20.1 | 29 | 17 | 0.04 | - |
| shared | 1000 | 5 | 119109 | 0.173 | 0.585 | 0.189 | 0.610 | 0.150 | 0.527 | 5.7 | 77.8 | 111 | 57 | 0.08 | - |
| shared | 1000 | 20 | 476378 | 0.170 | 0.580 | 0.187 | 0.616 | 0.150 | 0.538 | 5.8 | 251.4 | 417 | 207 | 0.20 | - |
| per-process | 1 | 1 | 2344 | 0.460 | 0.937 | 0.481 | 0.959 | 0.436 | 0.851 | 4.8 | 6.3 | 11 | 8 | 34.25 | 53.1 |
| per-process | 1 | 5 | 2344 | 0.481 | 1.035 | 0.508 | 1.058 | 0.455 | 1.023 | 4.9 | 6.4 | 11 | 8 | 34.25 | 53.2 |
| per-process | 1 | 20 | 2343 | 0.525 | 0.946 | 0.541 | 0.982 | 0.504 | 0.932 | 4.9 | 6.4 | 11 | 8 | 34.25 | 54.1 |
| per-process | 10 | 1 | 2348 | 0.345 | 0.873 | 0.364 | 0.877 | 0.312 | 0.859 | 49.1 | 50.8 | 83 | 71 | 30.84 | 51.1 |
| per-process | 10 | 5 | 2348 | 0.347 | 0.963 | 0.361 | 0.977 | 0.316 | 0.963 | 48.9 | 50.6 | 83 | 71 | 32.44 | 50.1 |
| per-process | 10 | 20 | 4747 | 0.324 | 0.949 | 0.344 | 0.972 | 0.295 | 0.891 | 48.9 | 52.2 | 86 | 72 | 34.06 | 50.4 |
| per-process | 100 | 1 | 2374 | 0.283 | 0.852 | 0.306 | 0.880 | 0.260 | 0.777 | 489.2 | 491.2 | 803 | 701 | 32.90 | 50.4 |
| per-process | 100 | 5 | 11912 | 0.246 | 0.619 | 0.268 | 0.646 | 0.223 | 0.581 | 488.1 | 496.3 | 815 | 705 | 32.43 | 48.9 |
| per-process | 100 | 20 | 47820 | 0.214 | 0.297 | 0.236 | 0.307 | 0.193 | 0.258 | 488.6 | 520.6 | 860 | 720 | 33.26 | 53.2 |
| per-process | 1000 | 1 | 23728 | 0.242 | 0.341 | 0.264 | 0.352 | 0.218 | 0.299 | 4887.4 | 4907.0 | 8030 | 7010 | 33.01 | 55.0 |
| per-process | 1000 | 5 | 119017 | 0.239 | 0.472 | 0.260 | 0.492 | 0.215 | 0.442 | 4894.8 | 4977.9 | 8150 | 7050 | 33.37 | 55.1 |
| per-process | 1000 | 20 | 475517 | 0.254 | 0.370 | 0.277 | 0.386 | 0.229 | 0.332 | 4889.9 | 5210.5 | 8600 | 7200 | 33.42 | 56.5 |

At N=1,000 both layouts passed at 1, 5 and 20% active. So the sweep alone can't separate them. Both cost at most $578.16 / 1,000 = $0.58 per tenant per month.

### Past N=1,000

The sweep left both layouts at the top of its range. So we ran more cells at 5% active, same flags. The budget allowed it. Raw rows: `bench/tenants-l8s-v3-extra.tsv`.

| layout | N | p99 append ms | p99 fact ms | RSS load MiB | threads | cold start ms | pass |
|---|---|---|---|---|---|---|---|
| shared | 2,000 | 0.631 | 0.551 | 138.0 | 107 | - | yes |
| shared | 4,000 | 0.637 | 0.566 | 257.3 | 207 | - | yes |
| shared | 5,000 | 0.702 | 0.620 | 301.6 | 257 | - | yes |
| shared | 8,000 | - | - | - | - | - | no: failed, see below |
| per-process | 2,000 | 0.441 | 0.398 | 9949.5 | 14100 | 61.1 | yes |
| per-process | 4,000 | 0.762 | 0.663 | 19888.3 | 28200 | 69.8 | yes |
| per-process | 6,000 | 4.313 | 3.785 | 29834.9 | 42300 | 92.7 | yes |
| per-process | 8,000 | 81.905 | 81.790 | 39584.2 | 56000 | 160.5 | no |
| per-process | 8,000 (repeat) | 65.244 | 58.767 | 39590.1 | 56000 | 159.6 | no |

**shared at N=8,000 failed on a constant, not on load.** `serve` accepts at most 256 open connections (`MAX_CONNECTIONS` in `serve.rs`). The 257th gets 503 `busy` and a closed socket. 5% of 8,000 is 400 clients, each holding 1 connection. A client hit `BrokenPipeError` and the run wrote no row. With this client model, shared stops at 256 / 5% = 5,120 tenants.

To see what shared does past that cap, we built a second binary on the VM only. It changes `MAX_CONNECTIONS` from 256 to 2048 and nothing else. It isn't committed. Raw rows: `bench/tenants-l8s-v3-cap2048.tsv`.

| layout | N | p99 append ms | p99 fact ms | RSS load MiB | fds | threads |
|---|---|---|---|---|---|---|
| shared, cap 2048 | 8,000 | 0.696 | 0.635 | 473.0 | 825 | 407 |
| shared, cap 2048 | 16,000 | 0.725 | 0.663 | 901.7 | 1642 | 807 |

**per-process at N=8,000 failed on CPU.** It reproduced: p99 82 ms, then 62 to 65 ms. `vmstat` showed 0% idle and a run queue of 1,000 to 3,900 during load. At N=6,000 it had about 10% idle. shared at 16,000 with 800 clients kept 46% idle. Clients run on the same 8 vCPU in both layouts. So the extra CPU comes from 8,000 `serve` processes and 56,000 threads. Which of their threads burn it is not measured.

### Verdict

Pass bar: p99 at most 50 ms for both append and fact. VM cost: $0.792/hr x 730 hr = $578.16 per month. Cost per tenant is that over N. Disk and egress are excluded.

| layout | largest passing N at 5% | cost per tenant per month | what stops it |
|---|---|---|---|
| per-process | 6,000 | $0.096 | CPU at 8,000 |
| shared, as shipped | 5,000 | $0.116 | 256-connection cap, 5,120 at 5% |
| shared, cap 2048 | 16,000 or more | $0.036 or less | not reached |

At 1% and 20% active, both layouts passed every N up to 1,000. Neither was run past 1,000 at those shares.

**The 2x rule switches us to shared.** per-process costs at least 2.67x shared per tenant, once shared's connection cap is raised. As shipped, shared stops at 5,120 tenants and per-process is cheaper. But that cap is 1 constant that bounds request memory. It isn't a limit of the layout. A hosted shared `serve` needs it raised, or a proxy that pools connections. Raising it to 2048 raises the worst-case body memory from 2 GiB to 16 GiB.

Both numbers are tiny next to any price. Neither layout hits RAM or disk first on this VM. per-process at 6,000 used 29 GiB RSS and about 200 GiB of NVMe.

Other findings:

- Linux NVMe latency is about 10x lower than the Mac's. Combined p50 sat between 0.17 and 0.53 ms in the sweep.
- The Mac's p50 jump at per-process N=100 and 20% active did not appear. p50 was 0.214 ms.
- An idle `serve` costs 4.9 MiB RSS on Linux. That's 4.9 GiB at N=1,000 per-process, against 5.7 MiB shared.
- shared adds about 1.1 MiB RSS, 2 fds and 1 thread per open connection under load.
- Cold start was 49 to 57 ms up to N=1,000. It rose to 93 ms at 6,000 and 160 ms at 8,000.
- Starting 8,000 per-process servers took about 2.5 minutes, mostly in system CPU and I/O wait.

Cost of the run: zdb-g1 ran from 05:29:09 to 06:59:21 UTC. That's 1.50 hours, or $1.19 at $0.792/hr. It was deallocated after.

## Local results

**Local shape check, not the verdict.** The Mac is not the target VM. Latency includes macOS fsync on an Apple SSD.

Machine: MacBook Air (Mac16,12), Apple M4, 10 cores (4 performance, 6 efficiency), 16 GB RAM. macOS 27.2 (build 26B123). Internal APFS SSD, 494 GB. Python 3.14.6. `atticdb 0.1.0+dev` release build at commit 243140f.

Command: the sweep above with `ATTICDB_BIN=target/release/atticdb` and the default base. 18 runs of 60 s took 1,191 s in total. The append and fact columns are in ms. Raw rows: `bench/tenants-local-mac.tsv`.

| layout | N | active % | ops | p50 ms | p99 ms | p50 append | p99 append | p50 fact | p99 fact | RSS idle MiB | RSS load MiB | fds | threads | disk MiB/tenant | cold start ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| shared | 1 | 1 | 1024 | 4.609 | 6.261 | 4.630 | 6.348 | 4.573 | 5.949 | 3.6 | 5.4 | 11 | 8 | 34.11 | - |
| shared | 1 | 5 | 1023 | 4.608 | 6.065 | 4.632 | 6.065 | 4.580 | 6.014 | 3.6 | 5.4 | 11 | 8 | 34.11 | - |
| shared | 1 | 20 | 1025 | 4.557 | 6.511 | 4.605 | 6.257 | 4.523 | 6.511 | 3.6 | 5.4 | 11 | 8 | 34.11 | - |
| shared | 10 | 1 | 1029 | 4.601 | 6.263 | 4.611 | 6.237 | 4.589 | 6.263 | 3.6 | 5.5 | 11 | 8 | 3.41 | - |
| shared | 10 | 5 | 997 | 4.600 | 6.063 | 4.612 | 6.271 | 4.585 | 6.063 | 3.6 | 5.4 | 11 | 8 | 3.41 | - |
| shared | 10 | 20 | 1975 | 4.499 | 8.224 | 4.517 | 8.568 | 4.486 | 7.704 | 3.6 | 6.3 | 13 | 9 | 3.42 | - |
| shared | 100 | 1 | 998 | 4.591 | 6.370 | 4.606 | 6.130 | 4.585 | 6.454 | 3.7 | 5.5 | 11 | 8 | 0.34 | - |
| shared | 100 | 5 | 4959 | 4.488 | 8.590 | 4.546 | 8.758 | 4.440 | 8.207 | 3.7 | 8.8 | 19 | 12 | 0.34 | - |
| shared | 100 | 20 | 19478 | 5.404 | 8.561 | 5.571 | 8.763 | 5.225 | 8.337 | 3.7 | 17.9 | 49 | 27 | 0.36 | - |
| per-process | 1 | 1 | 982 | 4.597 | 6.681 | 4.607 | 6.420 | 4.586 | 6.854 | 3.6 | 5.3 | 11 | 8 | 34.11 | 41.5 |
| per-process | 1 | 5 | 978 | 4.591 | 6.732 | 4.625 | 6.762 | 4.564 | 6.732 | 3.6 | 5.4 | 11 | 8 | 34.11 | 43.3 |
| per-process | 1 | 20 | 979 | 4.625 | 6.341 | 4.643 | 6.341 | 4.620 | 6.343 | 3.6 | 5.3 | 11 | 8 | 34.11 | 43.1 |
| per-process | 10 | 1 | 987 | 4.564 | 6.796 | 4.584 | 7.267 | 4.536 | 5.998 | 37.0 | 38.8 | 83 | 71 | 34.02 | 42.4 |
| per-process | 10 | 5 | 985 | 4.577 | 6.882 | 4.612 | 7.416 | 4.564 | 6.082 | 37.1 | 38.9 | 83 | 71 | 34.02 | 40.2 |
| per-process | 10 | 20 | 1976 | 4.451 | 8.875 | 4.437 | 9.035 | 4.480 | 8.849 | 36.1 | 39.5 | 86 | 72 | 34.03 | 41.0 |
| per-process | 100 | 1 | 1047 | 4.528 | 6.645 | 4.547 | 6.782 | 4.500 | 6.571 | 285.7 | 290.5 | 803 | 701 | 33.53 | 38.1 |
| per-process | 100 | 5 | 5159 | 4.494 | 11.119 | 4.503 | 11.474 | 4.484 | 10.819 | 366.9 | 375.9 | 815 | 705 | 32.58 | 37.1 |
| per-process | 100 | 20 | 19053 | 9.760 | 18.223 | 9.763 | 18.223 | 9.756 | 18.188 | 366.4 | 401.5 | 860 | 720 | 33.87 | 40.8 |

What the shape shows:

- Every cell held p99 under 20 ms, for both ops. The 50 ms bar from design 0009 was never close.
- append and fact p99 stayed within 1.3 ms of each other in every cell.
- An idle `serve` costs about 3 to 4 MiB RSS, 8 fds and 7 threads.
- per-process scales that cost by N. At N=100 it held 286 to 367 MiB idle against 3.7 MiB shared.
- shared adds about 2 fds and 1 thread per open connection. That matches 1 thread per connection.
- per-process disk is 34 MiB per tenant. shared spreads 1 tape, so it's 0.34 MiB at N=100.
- At N=100 and 20% active, per-process p50 doubled to 9.8 ms. shared stayed at 5.4 ms.
- Cold start on an existing dir took a median of 37 to 43 ms.

The p50 jump is a hypothesis, not a finding. 20 servers fsync on their own, while 1 shared server may batch them. The Linux run should show whether it holds on NVMe.

These rows come from the third sweep. The first sampled fds and threads after the clients closed, so it missed connection threads. The second had no per-op columns or repeated cold starts. Both were discarded. The first also showed shared N=1 p99 spikes of 130 to 320 ms. Neither later sweep repeated them.
