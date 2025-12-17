use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use harmonic::utils::hash::BuzHash;
use rollsum::{Bup, Engine};
use adler::Adler32;

fn buzhash_compute(data: &[u8], window_size: usize) -> u64 {
    let mut hasher = BuzHash::new(window_size);
    hasher.compute(data)
}

fn buzhash_rolling(data: &[u8], window_size: usize) -> u64 {
    let mut hasher = BuzHash::new(window_size);
    hasher.compute(&data[0..window_size]);

    for &byte in data[window_size..].iter() {
        hasher.roll(byte);
    }

    hasher.hash
}

fn adler32_compute(data: &[u8]) -> u32 {
    let mut hasher = Adler32::new();
    hasher.write_slice(data);
    hasher.checksum()
}

fn rollsum_rolling_64(data: &[u8]) -> u32 {
    // can only compute 64 window size
    let mut hasher = Bup::new();

    for &byte in data[0..64].iter() {
        hasher.roll_byte(byte);
    }

    for i in 64..data.len() {
        hasher.roll_byte(data[i]);
        hasher.digest();
    }

    hasher.digest()
}

fn benchmark_hash_compute(c: &mut Criterion) {
    let sizes = vec![64, 256, 1024, 4096, 16384];

    let mut group = c.benchmark_group("hash_compute");

    for size in sizes {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("buzhash", size), &data, |b, data| {
            b.iter(|| buzhash_compute(black_box(data), black_box(64)));
        });

        group.bench_with_input(BenchmarkId::new("adler32", size), &data, |b, data| {
            b.iter(|| adler32_compute(black_box(data)));
        });
    }

    group.finish();
}

fn benchmark_rolling_hash(c: &mut Criterion) {
    let sizes = vec![1024, 4096, 16384, 65536];
    let window_size = 64;

    let mut group = c.benchmark_group("rolling_hash");

    for size in sizes {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("buzhash_rolling", size), &data, |b, data| {
            b.iter(|| buzhash_rolling(black_box(data), black_box(window_size)));
        });

        group.bench_with_input(BenchmarkId::new("rollsum_rolling", size), &data, |b, data| {
            b.iter(|| rollsum_rolling_64(black_box(data)));
        });
    }

    group.finish();
}

fn benchmark_window_sizes(c: &mut Criterion) {
    let window_sizes = vec![8, 64, 128, 1024, 8096];
    let data_size = 51200;
    let data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();

    let mut group = c.benchmark_group("window_size_comparison");

    for window_size in window_sizes {
        group.bench_with_input(
            BenchmarkId::new("buzhash", window_size),
            &window_size,
            |b, &ws| {
                b.iter(|| buzhash_compute(black_box(&data[0..ws]), black_box(ws)));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, benchmark_hash_compute, benchmark_rolling_hash, benchmark_window_sizes);
criterion_main!(benches);
