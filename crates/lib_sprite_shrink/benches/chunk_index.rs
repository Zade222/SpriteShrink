//! Chunk index throughput: building the hash map an archive's chunk index
//! is parsed into, and looking chunks up in it as extraction does. Both
//! hash every chunk hash again, so they show what the map's hasher costs.
//!
//! Only the public API is used, so the same file builds against older
//! commits and can be compared with a saved Criterion baseline:
//!
//! ```text
//! cargo bench -p sprite_shrink --bench chunk_index -- --save-baseline before
//! cargo bench -p sprite_shrink --bench chunk_index -- --baseline before
//! ```

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use sprite_shrink::{ChunkLocation, Hashable, parse_file_chunk_index};

/// About the number of chunks in three CD images at the default 2 KiB
/// window.
const CHUNK_COUNT: u64 = 1 << 20;

/// Fixed-seed xorshift, so every run and every commit sees the same order.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// `CHUNK_COUNT` chunk hashes, made the way an archive makes them, and the
/// chunk index that stores them, encoded as an archive does.
fn index() -> (Vec<u64>, Vec<u8>) {
    let hashes: Vec<u64> = (0..CHUNK_COUNT)
        .map(|n| u64::from_bytes_with_seed(&n.to_le_bytes()))
        .collect();
    let entries: Vec<(u64, ChunkLocation)> = hashes
        .iter()
        .zip(0..)
        .map(|(&hash, n)| {
            (hash, ChunkLocation { offset: n * 1500, compressed_length: 1500 })
        })
        .collect();
    (hashes, bitcode::encode(&entries))
}

fn bench_chunk_index(c: &mut Criterion) {
    let (mut hashes, encoded) = index();

    let mut group = c.benchmark_group("chunk_index");
    group.throughput(Throughput::Elements(CHUNK_COUNT));

    group.bench_function("parse", |b| {
        b.iter(|| parse_file_chunk_index::<u64>(black_box(&encoded)));
    });

    let Ok(map) = parse_file_chunk_index::<u64>(&encoded) else {
        eprintln!("The benchmark's chunk index doesn't parse.");
        return;
    };

    // Fisher-Yates with a fixed seed: extraction looks chunks up in file
    // order, which isn't the order they were hashed in.
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for i in (1..hashes.len()).rev() {
        let bound = u64::try_from(i + 1).unwrap_or(u64::MAX);
        let j = usize::try_from(rng.next() % bound).unwrap_or(0);
        hashes.swap(i, j);
    }

    group.bench_function("lookup_all", |b| {
        b.iter(|| {
            hashes
                .iter()
                .filter_map(|hash| map.get(black_box(hash)))
                .map(|location| u64::from(location.compressed_length))
                .sum::<u64>()
        });
    });

    group.finish();
}

criterion_group!(benches, bench_chunk_index);
criterion_main!(benches);
