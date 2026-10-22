#![allow(clippy::unwrap_used)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use zerodb_tape::{
    Durability, Kind, RecordInput, StdIo, SystemClock, Tape, TapeOptions as Options,
};
mod support;
use support::{encoded_batch, Bare, BenchDir};

fn record(size: usize) -> RecordInput {
    RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![0x5a; size] }
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
            let mut bare = Bare::new(&dir.0, 0);
            let bytes = encoded_batch(&[record(size)]);
            let samples = (0..1024)
                .map(|_| {
                    let started = Instant::now();
                    bare.write(&bytes);
                    bare.sync();
                    started.elapsed()
                })
                .collect();
            percentiles(&format!("bare/{size}/1"), samples);
            group.bench_function("bare/1", |b| {
                b.iter(|| {
                    bare.write(&bytes);
                    bare.sync();
                })
            });
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
