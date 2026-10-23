#![allow(clippy::unwrap_used)]

#[path = "../../zerodb-tape/benches/support/mod.rs"]
mod support;

use std::fs::File;
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Barrier, Mutex};
use std::time::{Duration, Instant};
use support::{encoded_batch, Bare, BenchDir};
use zerodb_core::{turn::encode, Engine, EngineOptions, NewTurn, Role, Scope, WindowQuery};
use zerodb_tape::{Durability, Kind, RecordInput, StdIo, SystemClock, Tape, TapeOptions};

const READERS: usize = 2;
const WINDOW: usize = 16;
const SESSIONS: usize = 64;
const SIZES: [usize; 5] = [256, 512, 1024, 4096, 8192];

fn scope(writer: usize, session: usize) -> Scope {
    Scope {
        tenant: format!("tenant-{writer:02}"),
        agent: format!("agent-{writer:02}"),
        session: format!("session-{session:03}"),
    }
}
fn turns(iteration: usize, batch: usize) -> Vec<NewTurn> {
    (0..batch)
        .map(|i| NewTurn {
            role: Role::User,
            meta: vec![0x42; 16],
            content: vec![0x5a; SIZES[(iteration * batch + i) % SIZES.len()]],
        })
        .collect()
}
fn records(scope: &Scope, iteration: usize, batch: usize) -> Vec<RecordInput> {
    turns(iteration, batch)
        .iter()
        .map(|turn| RecordInput {
            kind: Kind::Turn,
            tenant_id: 1,
            content: encode(&scope.agent, &scope.session, 1, turn, None).unwrap(),
        })
        .collect()
}
fn quantile(samples: &[Duration], numerator: usize, denominator: usize) -> f64 {
    samples[(samples.len() * numerator).div_ceil(denominator) - 1].as_secs_f64() * 1e6
}
fn latency(label: &str, samples: &mut [Duration]) -> [f64; 3] {
    assert!(!samples.is_empty());
    samples.sort_unstable();
    let p99 = quantile(samples, 99, 100);
    println!(
        "{label}: samples={} p50_us={:.3} p99_us={p99:.3} p99.9_us={:.3}",
        samples.len(),
        quantile(samples, 50, 100),
        quantile(samples, 999, 1000)
    );
    [quantile(samples, 50, 100), p99, quantile(samples, 999, 1000)]
}
#[derive(Clone, Copy)]
struct Stats {
    ack: [f64; 3],
    throughput: f64,
    reopen: Option<Reopen>,
}
#[derive(Clone, Copy)]
struct Reopen {
    ms: f64,
    records: u64,
    tail_bytes: u64,
}

fn index_report(label: &str, stats: zerodb_tape::LsnIndexStats, turns: usize) {
    println!(
        "{label}/lsn_index: resident_bytes={} budget_bytes={} page_evictions={} dirty_page_evictions={} page_reloads={}",
        stats.resident_bytes, stats.budget_bytes, stats.page_evictions,
        stats.dirty_page_evictions, stats.page_reloads,
    );
    assert!(stats.resident_bytes <= stats.budget_bytes);
    if turns >= 1_000_000 && env_number("G1_LSN_INDEX_MIB", 0) == 0 {
        assert!(stats.page_evictions > 0, "1M G1 must exercise index eviction");
    }
}

fn report(label: &str, elapsed: Duration, n: usize, mut samples: Vec<Duration>) -> Stats {
    println!(
        "{label}: turns={n} seconds={:.3} turns_per_sec={:.1} batches_per_sec={:.1}",
        elapsed.as_secs_f64(),
        n as f64 / elapsed.as_secs_f64(),
        samples.len() as f64 / elapsed.as_secs_f64()
    );
    Stats {
        ack: latency(&format!("{label}/durable_ack"), &mut samples),
        throughput: n as f64 / elapsed.as_secs_f64(),
        reopen: None,
    }
}

fn writers(
    iterations: usize,
    readers: usize,
    append: impl Fn(usize, usize) + Sync,
    read: impl Fn(usize, usize) -> Option<u64> + Sync,
) -> (Duration, Vec<Duration>, Vec<Duration>, Queries) {
    let barrier = Barrier::new(writer_count() + readers + 1);
    let done = AtomicBool::new(false);
    std::thread::scope(|threads| {
        let writers = (0..writer_count())
            .map(|writer| {
                let (barrier, append) = (&barrier, &append);
                threads.spawn(move || {
                    let mut samples = Vec::with_capacity(iterations);
                    barrier.wait();
                    for i in 0..iterations {
                        let start = Instant::now();
                        append(writer, i);
                        samples.push(start.elapsed());
                    }
                    samples
                })
            })
            .collect::<Vec<_>>();
        let readers = (0..readers)
            .map(|reader| {
                let (barrier, done, read) = (&barrier, &done, &read);
                threads.spawn(move || {
                    let mut samples = Vec::new();
                    let mut count = 0;
                    let mut records = 0;
                    barrier.wait();
                    while !done.load(Ordering::Acquire) {
                        let start = Instant::now();
                        // A reader that finds nothing durable yet has not run a query.
                        let Some(read) = read(reader, count) else {
                            std::thread::yield_now();
                            continue;
                        };
                        records += read;
                        // Bound sample memory during the 1M-turn trace. Count every completed query.
                        if count.is_multiple_of(64) {
                            samples.push(start.elapsed());
                        }
                        count += 1;
                    }
                    (Queries { queries: count as u64, records }, samples)
                })
            })
            .collect::<Vec<_>>();
        let start = Instant::now();
        barrier.wait();
        let samples = writers.into_iter().flat_map(|t| t.join().unwrap()).collect();
        let elapsed = start.elapsed();
        done.store(true, Ordering::Release);
        let mut read_samples = Vec::new();
        let mut queries = Queries::default();
        for reader in readers {
            let (n, samples) = reader.join().unwrap();
            queries.queries += n.queries;
            queries.records += n.records;
            read_samples.extend(samples);
        }
        (elapsed, samples, read_samples, queries)
    })
}

struct Request {
    bytes: Arc<Vec<u8>>,
    reply: mpsc::SyncSender<()>,
}
#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset).unwrap();
}
#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) {
    while !buf.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_read(file, buf, offset).unwrap();
        assert!(n > 0);
        buf = &mut buf[n..];
        offset += n as u64;
    }
}

fn reader_count(symmetric: bool) -> usize {
    if symmetric {
        READERS
    } else {
        0
    }
}

#[derive(Clone, Copy, Default)]
struct Queries {
    queries: u64,
    records: u64,
}

fn window_report(label: &str, queries: Queries, mut reads: Vec<Duration>) {
    println!(
        "{label}/window: queries={} records_per_query={:.3} sample_stride=64",
        queries.queries,
        queries.records as f64 / queries.queries.max(1) as f64
    );
    // Short runs can end before any read finds durable data.
    if !reads.is_empty() {
        latency(&format!("{label}/window"), &mut reads);
    }
}

fn bare(iterations: usize, batch: usize, n: usize, symmetric: bool) -> Stats {
    let dir = BenchDir::new();
    let templates = (0..SIZES.len())
        .map(|i| Arc::new(encoded_batch(&records(&scope(0, 0), i, batch))))
        .collect::<Vec<_>>();
    let mut bare = Bare::new(&dir.0);
    println!(
        "matched encoded batch bytes: {:?}",
        templates.iter().map(|b| b.len()).collect::<Vec<_>>()
    );
    // Engine Last(16) reads the 16 most recent turns of one session, one tape read each.
    // Short sessions return fewer, so compare records_per_query, not only window latency.
    // The bare reader does 16 positional reads ending at the synced offset. Each read is the
    // mean encoded record size with its footer share. Its newest bytes sit in page cache, so
    // it completes far more queries than Tape or Engine. That extra load may slow bare appends.
    let record_bytes = templates.iter().map(|b| b.len()).sum::<usize>() / (SIZES.len() * batch);
    assert!(zerodb_tape::sim::DEFAULT_SEGMENT_SIZE <= u64::from(u32::MAX));
    // Synced end as segment << 32 | offset, published after each sync.
    let durable = Arc::new(AtomicU64::new(0));
    let published = durable.clone();
    let files = (0..READERS).map(|_| Mutex::new(None::<(u64, File)>)).collect::<Vec<_>>();
    let (sender, receiver) = mpsc::channel::<Request>();
    let worker = std::thread::spawn(move || {
        let mut syncs = 0;
        let mut requests = 0;
        while let Ok(first) = receiver.recv() {
            let group: Vec<_> = std::iter::once(first).chain(receiver.try_iter()).collect();
            for request in &group {
                bare.write(&request.bytes);
            }
            bare.sync();
            if symmetric {
                let (segment, offset) = bare.position();
                published.store(segment << 32 | offset, Ordering::Release);
            }
            syncs += 1;
            requests += group.len();
            for request in group {
                request.reply.send(()).unwrap();
            }
        }
        (syncs, requests)
    });
    let (elapsed, samples, reads, queries) = writers(
        iterations,
        reader_count(symmetric),
        |_, i| {
            let (reply, receiver) = mpsc::sync_channel(1);
            sender.send(Request { bytes: templates[i % SIZES.len()].clone(), reply }).unwrap();
            receiver.recv().unwrap();
        },
        |reader, _| {
            let packed = durable.load(Ordering::Acquire);
            let (segment, mut offset) = (packed >> 32, packed & u64::from(u32::MAX));
            if offset == 0 {
                return None;
            }
            let mut file = files[reader].lock().unwrap();
            if file.as_ref().is_none_or(|(id, _)| *id != segment) {
                *file = Some((segment, File::open(Bare::path(&dir.0, segment)).unwrap()));
            }
            let (_, file) = file.as_ref().unwrap();
            let mut buf = vec![0; record_bytes];
            let mut records = 0;
            for _ in 0..WINDOW {
                let len = record_bytes.min(offset as usize);
                if len == 0 {
                    break;
                }
                offset -= len as u64;
                read_at(file, &mut buf[..len], offset);
                black_box(&buf);
                records += 1;
            }
            Some(records)
        },
    );
    drop(sender);
    let (syncs, requests) = worker.join().unwrap();
    println!(
        "bare: sync_groups={syncs} mean_batches_per_group={:.3}",
        requests as f64 / syncs as f64
    );
    if symmetric {
        window_report("bare", queries, reads);
    }
    report("bare", elapsed, n, samples)
}

fn tape(iterations: usize, batch: usize, n: usize, symmetric: bool) -> Stats {
    let dir = BenchDir::new();
    let (tape, _) =
        Tape::open(Arc::new(StdIo::new()), Arc::new(SystemClock), &dir.0, tape_options()).unwrap();
    let templates = (0..SIZES.len()).map(|i| records(&scope(0, 0), i, batch)).collect::<Vec<_>>();
    let (elapsed, samples, reads, queries) = writers(
        iterations,
        reader_count(symmetric),
        |_, i| {
            black_box(
                tape.append(templates[i % SIZES.len()].clone(), Durability::Durable).unwrap(),
            );
        },
        |_, _| {
            // The last 16 durable LSNs across all writers, through the read path an engine
            // window uses. Footers take no LSN. No session lookup is involved.
            let end = tape.durable_lsn();
            if end == 0 {
                return None;
            }
            let start = end.saturating_sub(WINDOW as u64 - 1).max(1);
            for lsn in start..=end {
                black_box(tape.read(lsn).unwrap());
            }
            Some(end - start + 1)
        },
    );
    let (groups, requests) = tape.group_stats();
    println!(
        "tape: writer_groups={groups} mean_requests_per_group={:.3} superblock_installs={}",
        requests as f64 / groups as f64,
        tape.superblock_installs()
    );
    index_report("tape", tape.index_stats().unwrap(), n);
    if symmetric {
        window_report("tape", queries, reads);
    }
    report("tape", elapsed, n, samples)
}

fn engine(iterations: usize, batch: usize, n: usize, checkpoint_bytes: u64) -> Stats {
    let dir = BenchDir::new();
    let opts = EngineOptions { checkpoint_bytes, tape: tape_options(), ..EngineOptions::default() };
    let engine = Engine::open(&dir.0, opts.clone()).unwrap();
    // Register every tenant and session before timing, so timed bytes match the bare trace.
    std::thread::scope(|threads| {
        for writer in 0..writer_count() {
            let engine = &engine;
            threads.spawn(move || {
                for session in 0..SESSIONS {
                    engine.append(&scope(writer, session), turns(0, 1), None).unwrap();
                }
            });
        }
    });
    let templates = (0..SIZES.len()).map(|i| turns(i, batch)).collect::<Vec<_>>();
    let scopes = (0..writer_count())
        .map(|w| (0..SESSIONS).map(|s| scope(w, s)).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let warmup = engine.metrics().unwrap();
    let (elapsed, samples, reads, queries) = writers(
        iterations,
        READERS,
        |writer, i| {
            black_box(
                engine
                    .append(&scopes[writer][i % SESSIONS], templates[i % SIZES.len()].clone(), None)
                    .unwrap(),
            );
        },
        |reader, i| {
            let scope = &scopes[(i + reader) % writer_count()][i / writer_count() % SESSIONS];
            Some(black_box(engine.window(scope, WindowQuery::Last(WINDOW)).unwrap()).len() as u64)
        },
    );
    let metrics = engine.metrics().unwrap();
    // Each returned turn is one tape read. The metric is the source; the sum cross-checks it.
    let records = metrics.window_reads - warmup.window_reads;
    assert_eq!(records, queries.records);
    window_report("engine", Queries { records, ..queries }, reads);
    let mut stats = report("engine", elapsed, n, samples);
    index_report("engine", metrics.tape_index, n);
    println!(
        "engine: writer_groups={} mean_requests_per_group={:.3}",
        metrics.writer_groups - warmup.writer_groups,
        (metrics.writer_group_requests - warmup.writer_group_requests) as f64
            / (metrics.writer_groups - warmup.writer_groups) as f64
    );
    println!(
        "engine: checkpoint_watermark={} checkpoint_files={} checkpoint_saves={} snapshot_lock_max_us={:.3} state_lock_max_us={:.3} superblock_installs={} checkpoint_buffer_peak_bytes={}",
        metrics.checkpoint_watermark,
        metrics.checkpoint_files,
        metrics.checkpoint_saves,
        metrics.checkpoint_snapshot_lock_ns as f64 / 1e3,
        metrics.checkpoint_state_lock_ns as f64 / 1e3,
        metrics.superblock_installs - warmup.superblock_installs,
        metrics.checkpoint_buffer_peak_bytes
    );
    drop(engine);
    let start = Instant::now();
    let engine = Engine::open(&dir.0, opts).unwrap();
    let open_ms = start.elapsed().as_secs_f64() * 1e3;
    let recovered = engine.metrics().unwrap();
    assert_eq!(recovered.checkpoint_warnings, 0);
    assert!(recovered.replayed_records <= metrics.durable_lsn - metrics.checkpoint_watermark);
    println!("engine/reopen: checkpoint_mib={} total_ms={open_ms:.3} replayed_records={} tail_bytes_before_close={}", checkpoint_bytes / (1024 * 1024), recovered.replayed_records, metrics.checkpoint_pending_bytes);
    stats.reopen = Some(Reopen {
        ms: open_ms,
        records: recovered.replayed_records,
        tail_bytes: metrics.checkpoint_pending_bytes,
    });
    for scopes in &scopes {
        for (session, scope) in scopes.iter().enumerate() {
            let batches = iterations / SESSIONS + usize::from(session < iterations % SESSIONS);
            let tail = engine.window(scope, WindowQuery::Last(1)).unwrap();
            assert_eq!(tail.len(), 1);
            assert_eq!(tail[0].seq, (1 + batches * batch) as u64);
        }
    }
    println!(
        "engine/reopen: verified all {} session tails outside timing",
        writer_count() * SESSIONS
    );
    stats
}

fn history_windows() {
    let dir = BenchDir::new();
    let engine = Engine::open(&dir.0, EngineOptions::default()).unwrap();
    let scope = scope(0, 0);
    let mut length = 0;
    for target in [16, 1024, 65536] {
        while length < target {
            let count = (target - length).min(256);
            let turn = NewTurn { role: Role::User, meta: vec![], content: vec![0x5a; 256] };
            engine.append(&scope, vec![turn; count], None).unwrap();
            length += count;
        }
        for (name, query) in [
            ("last", WindowQuery::Last(16)),
            ("since", WindowQuery::Since { seq: (target / 2).max(1) as u64, limit: 16 }),
        ] {
            // The smallest history still returns 16 turns for the since query.
            let query = if name == "since" && target == 16 {
                WindowQuery::Since { seq: 1, limit: 16 }
            } else {
                query
            };
            for _ in 0..100 {
                black_box(engine.window(&scope, query).unwrap());
            }
            let before = engine.metrics().unwrap().window_reads;
            let mut samples = (0..2000)
                .map(|_| {
                    let start = Instant::now();
                    let turns = engine.window(&scope, query).unwrap();
                    assert_eq!(turns.len(), 16);
                    black_box(turns);
                    start.elapsed()
                })
                .collect::<Vec<_>>();
            let reads = engine.metrics().unwrap().window_reads - before;
            assert_eq!(reads, 2000 * 16);
            println!(
                "history={target} query={name} returned=16 tape_reads_per_query={}",
                reads / 2000
            );
            latency(&format!("window/{name}/{target}"), &mut samples);
        }
    }
}

/// `G1_LSN_INDEX_MIB` overrides the tape LSN index budget for eviction A/B runs.
fn tape_options() -> TapeOptions {
    let default = TapeOptions::default();
    let mib = env_number("G1_LSN_INDEX_MIB", 0);
    if mib == 0 {
        default
    } else {
        TapeOptions { lsn_index_bytes: mib * 1024 * 1024, ..default }
    }
}

/// `G1_WRITERS` closed-loop writers, 16 by default. Agents usually run 1 to 4.
fn writer_count() -> usize {
    static COUNT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *COUNT.get_or_init(|| env_number("G1_WRITERS", 16))
}

fn env_number(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |s| s.parse().unwrap())
}
fn spread(path: &str, metric: &str, mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let median = if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    };
    println!(
        "summary path={path} metric={metric} median={median:.3} min={:.3} max={:.3}",
        values[0],
        values[values.len() - 1]
    );
    median
}
fn replay_probe(iterations: usize, batch: usize, n: usize, reps: usize) {
    let mut results = [Vec::new(), Vec::new()];
    for rep in 1..=reps {
        for (i, mib) in [64, 128].into_iter().enumerate() {
            println!("replay_probe repetition={rep}/{reps} checkpoint_mib={mib}");
            let result = engine(iterations, batch, n, mib * 1024 * 1024);
            results[i].push(result.reopen.unwrap());
        }
    }
    for (mib, values) in [64, 128].into_iter().zip(results) {
        let path = format!("reopen/{mib}MiB");
        spread(&path, "total_ms", values.iter().map(|v| v.ms).collect());
        spread(&path, "replayed_records", values.iter().map(|v| v.records as f64).collect());
        spread(
            &path,
            "tail_bytes_before_close",
            values.iter().map(|v| v.tail_bytes as f64).collect(),
        );
    }
}
fn main() {
    let n = env_number("G1_TURNS", 1_000_000);
    let batch = env_number("G1_BATCH", 4);
    // Parse before the run so a bad value fails fast, not after a 1M-turn run.
    let nvme = env_number("G1_NVME", 0) != 0;
    let symmetric = env_number("G1_SYMMETRIC", 1);
    assert!(symmetric <= 1, "G1_SYMMETRIC must be 0 or 1");
    let symmetric = symmetric == 1;
    assert!((1..=256).contains(&batch), "G1_BATCH must be in 1..=256");
    assert!((1..=100).contains(&writer_count()), "G1_WRITERS must be in 1..=100");
    assert!(
        n >= writer_count() * batch && n.is_multiple_of(writer_count() * batch),
        "G1_TURNS must be a positive multiple of G1_WRITERS * G1_BATCH"
    );
    let iterations = n / writer_count() / batch;
    let reps = env_number("G1_REPS", 3);
    assert!(reps >= 3, "G1_REPS must be at least 3");
    if env_number("G1_REPLAY_PROBE", 0) != 0 {
        return replay_probe(iterations, batch, n, reps);
    }
    let checkpoint_bytes = env_number(
        "G1_CHECKPOINT_MIB",
        (EngineOptions::default().checkpoint_bytes / (1024 * 1024)) as usize,
    ) as u64
        * 1024
        * 1024;
    assert!(checkpoint_bytes > 0);
    let reader_paths = if symmetric { "bare,tape,engine" } else { "engine" };
    println!("G1: turns={n} batch={batch} writers={} readers={READERS} symmetric={} reader_paths={reader_paths} sessions={} sizes={SIZES:?}", writer_count(), u8::from(symmetric), writer_count() * SESSIONS);
    println!("Nearest-rank caller latency, measured per durable batch. macOS sync_data uses F_FULLFSYNC.");
    let mut results = Vec::new();
    for rep in 1..=reps {
        println!("repetition={rep}/{reps}");
        let bare = bare(iterations, batch, n, symmetric);
        let tape = tape(iterations, batch, n, symmetric);
        let engine = engine(iterations, batch, n, checkpoint_bytes);
        println!(
            "comparison repetition={rep} engine_p99_over_bare={:.3} tape_p99_over_bare={:.3}",
            engine.ack[1] / bare.ack[1],
            tape.ack[1] / bare.ack[1]
        );
        results.push([bare, tape, engine]);
    }
    for (i, path) in ["bare", "tape", "engine"].into_iter().enumerate() {
        for (j, metric) in ["p50_us", "p99_us", "p99.9_us"].into_iter().enumerate() {
            spread(path, metric, results.iter().map(|r| r[i].ack[j]).collect());
        }
        spread(path, "turns_per_sec", results.iter().map(|r| r[i].throughput).collect());
    }
    let ratio = spread(
        "engine/bare",
        "p99_ratio",
        results.iter().map(|r| r[2].ack[1] / r[0].ack[1]).collect(),
    );
    spread("tape/bare", "p99_ratio", results.iter().map(|r| r[1].ack[1] / r[0].ack[1]).collect());
    spread("engine/reopen", "total_ms", results.iter().map(|r| r[2].reopen.unwrap().ms).collect());
    spread(
        "engine/reopen",
        "replayed_records",
        results.iter().map(|r| r[2].reopen.unwrap().records as f64).collect(),
    );
    if env_number("G1_HISTORY", 1) != 0 {
        history_windows();
    }
    // Only a Linux NVMe run may print the release verdict. G1_NVME=1 marks that run.
    if std::env::consts::OS == "linux" && nvme {
        println!(
            "Linux NVMe 1.5x verdict: {} (median ratio={ratio:.3}). Nightly LazyFS remains pending. Append-only trace.",
            if ratio <= 1.5 { "PASS" } else { "FAIL" }
        );
    } else {
        let platform = match std::env::consts::OS {
            "macos" => "macOS",
            "linux" => "Linux",
            other => other,
        };
        println!(
            "{platform} local measurement: median ratio={ratio:.3} (1.5x target). Not the release verdict. Linux NVMe verdict pending: rerun on Linux NVMe with G1_NVME=1."
        );
    }
}
