//! Sector rebuild throughput, mainly to catch regressions in the ECC kernel.
//!
//! Only the public API is used, so the same file also builds against older
//! commits and can be compared with a saved Criterion baseline:
//!
//! ```text
//! cargo bench -p sprite_shrink_cd --bench sector_rebuild -- --save-baseline before
//! cargo bench -p sprite_shrink_cd --bench sector_rebuild -- --baseline before
//! ```
//!
//! `single` rebuilds the same sector every iteration, so its input stays in
//! L1. `shuffled` walks 1024 sectors (2 MiB of user data) in a fixed random
//! order, which is closer to extracting a disc.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use sprite_shrink_cd::{SectorType, rebuild_sector_bitwise, rebuild_sector_simd};

const USER_DATA_SIZE: usize = 2048;
const SECTOR_COUNT: usize = 1024;

type RebuildFn = fn(
    &[u8],
    &[u8; 8],
    SectorType,
    u32,
    Option<&[u8]>,
) -> Result<[u8; 2352], sprite_shrink_cd::ReconstructionError>;

/// Fixed-seed xorshift, so every run and every commit sees the same data.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// User data for `SECTOR_COUNT` sectors, in a shuffled visiting order.
fn shuffled_user_data() -> Vec<[u8; USER_DATA_SIZE]> {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);

    let mut sectors: Vec<[u8; USER_DATA_SIZE]> = (0..SECTOR_COUNT)
        .map(|_| {
            let mut data = [0_u8; USER_DATA_SIZE];
            for chunk in data.as_chunks_mut::<8>().0 {
                *chunk = rng.next().to_le_bytes();
            }
            data
        })
        .collect();

    // Fisher-Yates, so consecutive iterations don't touch neighbouring
    // memory the way a sequential walk would.
    for i in (1..sectors.len()).rev() {
        let bound = u64::try_from(i + 1).unwrap_or(u64::MAX);
        let j = usize::try_from(rng.next() % bound).unwrap_or(0);
        sectors.swap(i, j);
    }

    sectors
}

fn bench_rebuild(c: &mut Criterion) {
    let sectors = shuffled_user_data();
    let mode1_header = [0_u8; 8];
    // Mode 2 Form 1 subheader: data sector, submode 0x08, repeated.
    let form1_subheader = [0, 0, 0x08, 0, 0, 0, 0x08, 0];

    let cases: [(&str, RebuildFn, SectorType, [u8; 8]); 3] = [
        ("simd/mode1", rebuild_sector_simd, SectorType::Mode1, mode1_header),
        ("simd/mode2form1", rebuild_sector_simd, SectorType::Mode2Form1, form1_subheader),
        ("bitwise/mode2form1", rebuild_sector_bitwise, SectorType::Mode2Form1, form1_subheader),
    ];

    let mut group = c.benchmark_group("rebuild_sector");
    group.throughput(Throughput::Bytes(2352));

    for (name, rebuild, sector_type, metadata) in cases {
        group.bench_function(BenchmarkId::new(name, "single"), |b| {
            let data = sectors.first().map_or(&[0_u8; USER_DATA_SIZE], |s| s);
            b.iter(|| {
                rebuild(
                    black_box(data),
                    black_box(&metadata),
                    sector_type,
                    black_box(150),
                    None,
                )
            });
        });

        group.bench_function(BenchmarkId::new(name, "shuffled"), |b| {
            let mut next = sectors.iter().cycle();
            b.iter(|| {
                next.next().map(|data| {
                    rebuild(
                        black_box(data),
                        black_box(&metadata),
                        sector_type,
                        black_box(150),
                        None,
                    )
                })
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_rebuild);
criterion_main!(benches);
