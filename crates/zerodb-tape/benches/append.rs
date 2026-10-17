#![allow(clippy::unwrap_used)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use zerodb_tape::frame::{self, Footer, Frame, Header, Kind, VERSION};
use zerodb_tape::io::{StdIo, SystemClock};
use zerodb_tape::segment::DEFAULT_SEGMENT_SIZE;
use zerodb_tape::{Durability, Options, RecordInput, Tape};

#[derive(Debug)]
struct BenchDir(PathBuf);

impl BenchDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::current_dir().unwrap().join(format!(
            "target/append-bench-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for BenchDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn record(size: usize) -> RecordInput {
    RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![0x5a; size] }
}

// The baseline writes the same content, salt space, record header, and transaction footer.
fn encoded(size: usize) -> Vec<u8> {
    let input = record(size);
    let header = Header {
        kind: input.kind,
        version: VERSION,
        flags: 0,
        txn: 1,
        lsn: 1,
        commit_time_ns: 0,
        tenant_id: input.tenant_id,
        tenant_prev_chain: [0; 32],
        content_digest: frame::content_digest(&[0; 16], &input.content),
    };
    let mut payload = vec![0; 16];
    payload.extend(input.content);
    let mut bytes = frame::encode(&Frame { header: header.clone(), payload }).unwrap();
    let footer = Footer {
        txn: 1,
        record_count: 1,
        batch_digest: frame::batch_digest(&[header.content_digest]),
    };
    bytes.extend(
        frame::encode(&Frame {
            header: Header { kind: Kind::Footer, tenant_id: 0, content_digest: [0; 32], ..header },
            payload: footer.encode().to_vec(),
        })
        .unwrap(),
    );
    bytes
}

struct Bare {
    file: File,
    dir: PathBuf,
    segment: u64,
    offset: u64,
    bytes: Vec<u8>,
}

impl Bare {
    fn new(dir: &Path, size: usize) -> Self {
        let file = Self::segment(dir, 0);
        Self { file, dir: dir.to_owned(), segment: 0, offset: 0, bytes: encoded(size) }
    }

    fn segment(dir: &Path, id: u64) -> File {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dir.join(format!("bare-{id}")))
            .unwrap();
        file.set_len(DEFAULT_SEGMENT_SIZE).unwrap();
        file.sync_data().unwrap();
        File::open(dir).unwrap().sync_all().unwrap();
        file
    }

    fn append(&mut self) {
        if self.offset + self.bytes.len() as u64 > DEFAULT_SEGMENT_SIZE {
            self.segment += 1;
            self.file = Self::segment(&self.dir, self.segment);
            self.offset = 0;
        }
        self.file.write_all(&self.bytes).unwrap();
        self.file.sync_data().unwrap();
        self.offset += self.bytes.len() as u64;
    }
}

fn percentiles(label: &str, mut samples: Vec<Duration>) {
    samples.sort_unstable();
    let quantile = |percent: usize| samples[(samples.len() * percent).div_ceil(100) - 1];
    println!(
        "latency {label}: n={} p50={:.3} us p99={:.3} us",
        samples.len(),
        quantile(50).as_secs_f64() * 1e6,
        quantile(99).as_secs_f64() * 1e6
    );
}

fn run_tape(
    tape: &Tape,
    size: usize,
    writers: usize,
    n: u64,
    sample: bool,
) -> (Duration, Vec<Duration>) {
    let barrier = Barrier::new(writers + 1);
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..writers)
            .map(|_| {
                let barrier = &barrier;
                scope.spawn(move || {
                    let input = record(size);
                    let mut samples = Vec::new();
                    barrier.wait();
                    for _ in 0..n {
                        let started = sample.then(Instant::now);
                        let ack = tape.append(vec![input.clone()], Durability::Durable).unwrap();
                        std::hint::black_box(ack);
                        if let Some(started) = started {
                            samples.push(started.elapsed());
                        }
                    }
                    samples
                })
            })
            .collect();
        let started = Instant::now();
        barrier.wait();
        let samples = threads.into_iter().flat_map(|thread| thread.join().unwrap()).collect();
        (started.elapsed(), samples)
    })
}

fn append(c: &mut Criterion) {
    for size in [256, 4096] {
        let mut group = c.benchmark_group(format!("append/{size}"));
        group.throughput(Throughput::Elements(1));
        {
            let dir = BenchDir::new();
            let mut bare = Bare::new(&dir.0, size);
            let samples = (0..1024)
                .map(|_| {
                    let started = Instant::now();
                    bare.append();
                    started.elapsed()
                })
                .collect();
            percentiles(&format!("bare/{size}/1"), samples);
            group.bench_function("bare/1", |b| b.iter(|| bare.append()));
        }
        for writers in [1, 16] {
            let dir = BenchDir::new();
            let (tape, _) = Tape::open(
                Arc::new(StdIo::new()),
                Arc::new(SystemClock),
                &dir.0,
                Options::default(),
            )
            .unwrap();
            let (_, samples) = run_tape(&tape, size, writers, 1024 / writers as u64, true);
            percentiles(&format!("tape/{size}/{writers}"), samples);
            group.throughput(Throughput::Elements(writers as u64));
            group.bench_with_input(BenchmarkId::new("tape", writers), &writers, |b, &writers| {
                b.iter_custom(|n| run_tape(&tape, size, writers, n, false).0);
            });
        }
        group.finish();
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = append
}
criterion_main!(benches);
