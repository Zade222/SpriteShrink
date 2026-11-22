//! Data sector analysis throughput: how fast compression classifies a
//! disc's data sectors as plain or exceptions, through the public
//! `analyze_and_map_disc` the CLI calls.
//!
//! Compare against a saved Criterion baseline:
//!
//! ```text
//! cargo bench -p sprite_shrink_cd --bench sector_analysis -- --save-baseline before
//! cargo bench -p sprite_shrink_cd --bench sector_analysis -- --baseline before
//! ```
//!
//! Each iteration maps a one-track disc of 1024 different sectors.

use std::{hint::black_box, path::PathBuf};

use criterion::{
    BenchmarkId, Criterion, Throughput,
    criterion_group, criterion_main
};
use sprite_shrink_cd::{
    CueSheet, ExceptionRegistry, SectorDataProvider, SectorType,
    SubheaderRegistry,
    analyze_and_map_disc, analyze_cue_sheets, rebuild_sector_simd,
};

const SECTOR_COUNT: u32 = 1024;

/// The frame number of the first sector, after the two-second lead-in.
const FIRST_SECTOR: u32 = 150;

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

struct Sectors<'a>(&'a [[u8; 2352]]);

impl SectorDataProvider for Sectors<'_> {
    fn read_sector(&mut self, sector_index: u32) -> std::io::Result<[u8; 2352]> {
        usize::try_from(sector_index).ok()
            .and_then(|index| self.0.get(index))
            .copied()
            .ok_or_else(|| std::io::Error::other("no such sector"))
    }
}

/// `SECTOR_COUNT` valid sectors of `sector_type` with random user data,
/// numbered from `FIRST_SECTOR`.
fn sectors(sector_type: SectorType, metadata: [u8; 8]) -> Vec<[u8; 2352]> {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let len = usize::from(sector_type.data_size());

    (FIRST_SECTOR..FIRST_SECTOR + SECTOR_COUNT)
        .map(|number| {
            let data: Vec<u8> = (0..len.div_ceil(8))
                .flat_map(|_| rng.next().to_le_bytes())
                .take(len)
                .collect();
            rebuild_sector_simd(&data, &metadata, sector_type, number, None)
                .unwrap_or([0; 2352])
        })
        .collect()
}

/// A one-track disc of `SECTOR_COUNT` sectors in a `track_type` track.
fn sheet(track_type: &str) -> Option<CueSheet> {
    let cue = format!(
        "FILE \"disc.bin\" BINARY\n  TRACK 01 {track_type}\n    INDEX 01 00:00:00\n"
    );
    let len = u64::from(SECTOR_COUNT) * 2352;
    analyze_cue_sheets(&[(PathBuf::from("disc.cue"), cue)], |_| Ok(len))
        .ok()
        .and_then(|analysis| analysis.cue_sheets.into_iter().next())
}

fn bench_analysis(c: &mut Criterion) {
    let form1_subheader = [0, 0, 0x08, 0, 0, 0, 0x08, 0];
    let form2_subheader = [0, 0, 0x20, 0, 0, 0, 0x20, 0];

    let mut bad_edc = sectors(SectorType::Mode2Form1, form1_subheader);
    for sector in &mut bad_edc {
        sector[2072] ^= 0xFF;
    }

    let (Some(mode1), Some(mode2)) = (sheet("MODE1/2352"), sheet("MODE2/2352"))
    else {
        eprintln!("The benchmark's CUE sheets don't parse.");
        return;
    };
    let cases = [
        ("mode1", sectors(SectorType::Mode1, [0; 8]), &mode1),
        ("mode2form1", sectors(SectorType::Mode2Form1, form1_subheader), &mode2),
        ("mode2form2", sectors(SectorType::Mode2Form2, form2_subheader), &mode2),
        ("mode2form1_bad_edc", bad_edc, &mode2),
    ];

    let mut group = c.benchmark_group("analyze_disc");
    group.throughput(Throughput::Bytes(u64::from(SECTOR_COUNT) * 2352));

    for (name, sectors, sheet) in &cases {
        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| {
                analyze_and_map_disc(
                    sheet,
                    &[SECTOR_COUNT],
                    &mut Sectors(black_box(sectors)),
                    &SubheaderRegistry::default(),
                    &ExceptionRegistry::default(),
                )
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_analysis);
criterion_main!(benches);
