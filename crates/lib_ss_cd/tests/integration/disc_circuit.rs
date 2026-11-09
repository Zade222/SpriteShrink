//! Runs synthetic discs through the whole library: CUE analysis, sector
//! mapping, storing the data and audio streams in an archive file, then
//! verifying, writing and randomly reading the disc back.
//!
//! The discs are generated here so nothing large is committed.

use std::{
    collections::{HashMap, hash_map::Entry},
    convert::Infallible,
    fs::File,
    io::{BufWriter, Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha512};
use sprite_shrink::{ChunkLocation, Hashable};
use sprite_shrink_cd::{
    ContentBlock, CueSheet, DataChunkLayout, DiscManifest, DiscReconstructor,
    ExceptionRegistry, FileSizeProvider, SSMDIndices, SectorDataProvider,
    SectorMap, SectorRegionStream, SectorType, SubheaderRegistry,
    UserDataStream, analyze_and_map_disc, analyze_cue_sheets,
    compress_audio_block, disc_files, generate_cue_string,
    rebuild_sector_simd, resolve_file_sector_counts, rle_encode_map,
    verify_disc_integrity, write_disc,
};
use xxhash_rust::xxh3::xxh3_64;

const SECTOR: usize = 2352;
const BIN_NAME: &str = "disc.bin";

/// The sector a disc's first sector is numbered as in its headers.
const LEAD_IN: u32 = 150;

/// Sectors per stored audio block, as the CLI uses.
const AUDIO_BLOCK_SECTORS: usize = 16;

/// Shorter than a sector's user data and not a divisor of it, so chunk
/// boundaries land inside sectors and a sector can need several chunks.
const DATA_CHUNK_LEN: usize = 1500;

const FORM1_SUBHEADER: [u8; 8] = [1, 0, 0x08, 0, 1, 0, 0x08, 0];
const FORM2_SUBHEADER: [u8; 8] = [1, 0, 0x20, 0, 1, 0, 0x20, 0];

/// `len` bytes that differ between seeds and don't repeat every sector.
fn payload(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn number(index: usize) -> u32 {
    u32::try_from(index).unwrap() + LEAD_IN
}

fn data_sector(
    index: usize,
    sector_type: SectorType,
    metadata: [u8; 8],
) -> [u8; SECTOR] {
    let user_data = payload(usize::from(sector_type.data_size()), number(index));
    rebuild_sector_simd(&user_data, &metadata, sector_type, number(index), None)
        .unwrap()
}

fn form1(index: usize) -> [u8; SECTOR] {
    data_sector(index, SectorType::Mode2Form1, FORM1_SUBHEADER)
}

fn form2(index: usize) -> [u8; SECTOR] {
    data_sector(index, SectorType::Mode2Form2, FORM2_SUBHEADER)
}

fn mode1(index: usize, reserved: [u8; 8]) -> [u8; SECTOR] {
    data_sector(index, SectorType::Mode1, reserved)
}

fn audio(index: usize) -> [u8; SECTOR] {
    payload(SECTOR, number(index)).try_into().unwrap()
}

/// Breaks a sector's EDC, as a libcrypt-protected or damaged sector has.
/// `edc_at` is where the EDC starts for the sector's mode.
fn with_bad_edc(mut sector: [u8; SECTOR], edc_at: usize) -> [u8; SECTOR] {
    sector[edc_at] ^= 0xFF;
    sector
}

/// Joins `sectors(index)` for each sector of `len`.
fn image(len: usize, sector: impl Fn(usize) -> [u8; SECTOR]) -> Vec<u8> {
    (0..len).flat_map(sector).collect()
}

// Compression

struct OneFile(u64);

impl FileSizeProvider for OneFile {
    fn get_file_size_bytes(&self, _: &str) -> Result<u64, std::io::Error> {
        Ok(self.0)
    }
}

struct Sectors<'a>(&'a [u8]);

impl SectorDataProvider for Sectors<'_> {
    fn read_sector(&mut self, sector_index: u32) -> std::io::Result<[u8; SECTOR]> {
        let start = sector_index as usize * SECTOR;
        let mut sector = [0; SECTOR];
        sector.copy_from_slice(&self.0[start..start + SECTOR]);
        Ok(sector)
    }
}

fn parse(cue: &str, bin_len: u64) -> CueSheet {
    let sheets = [(PathBuf::from("disc.cue"), cue.to_string())];
    let analysis = analyze_cue_sheets(&sheets, |_: &Path| Ok(bin_len)).unwrap();
    analysis.cue_sheets.into_iter().next().unwrap()
}

/// A disc stored the way the CLI stores one, minus the archive's prefix.
struct Archived {
    sheet: CueSheet,
    bin: Vec<u8>,
    manifest: DiscManifest<u64>,
    indices: SSMDIndices<u64>,
    file: File,
    exception_blob: Vec<u8>,
    subheader_table: Vec<[u8; 8]>,
    data_chunks: HashMap<u64, Vec<u8>>,
    audio_blocks: HashMap<u64, Vec<u8>>,
}

fn is_audio(sector_type: SectorType) -> bool {
    matches!(
        sector_type,
        SectorType::Audio | SectorType::PregapAudio | SectorType::ZeroedAudio
    )
}

/// Appends `compressed` to the archive and returns where it went.
fn store(file: &mut File, compressed: &[u8]) -> ChunkLocation {
    let offset = file.stream_position().unwrap();
    file.write_all(compressed).unwrap();
    ChunkLocation {
        offset,
        compressed_length: u32::try_from(compressed.len()).unwrap(),
    }
}

fn archive(bin: Vec<u8>, cue: &str) -> Archived {
    let bin_len = bin.len() as u64;
    let sheet = parse(cue, bin_len);
    let sector_counts = resolve_file_sector_counts(&sheet, &OneFile(bin_len))
        .unwrap();
    let files = disc_files(&sheet, &sector_counts).unwrap();

    let subheaders = SubheaderRegistry::default();
    let exceptions = ExceptionRegistry::default();
    let mapped = analyze_and_map_disc(
        &sheet,
        &sector_counts,
        &mut Sectors(&bin),
        &subheaders,
        &exceptions,
    )
    .unwrap();
    let rle_sector_map = rle_encode_map(&mapped.sector_map);

    let mut file = tempfile::tempfile().unwrap();
    let mut source = Cursor::new(bin.clone());
    let mut indices = SSMDIndices {
        chunk_index: HashMap::new(),
        audio_block_index: HashMap::new(),
        exception_index: Vec::new(),
    };
    let mut data_chunks = HashMap::new();
    let mut audio_blocks = HashMap::new();
    let mut data_stream_layout = Vec::new();
    let mut audio_block_map = Vec::new();
    let mut run_start = 0u32;

    for &(run_len, sector_type) in &rle_sector_map.runs {
        let mut region = SectorRegionStream::new(&mut source, run_start, run_len);

        if is_audio(sector_type) {
            let mut raw = Vec::new();
            region.read_to_end(&mut raw).unwrap();
            for (i, block) in raw.chunks(AUDIO_BLOCK_SECTORS * SECTOR).enumerate() {
                let hash = u64::from_bytes_with_seed(block);
                if let Entry::Vacant(slot) = audio_blocks.entry(hash) {
                    let flac = compress_audio_block(block, false).unwrap();
                    indices.audio_block_index.insert(hash, store(&mut file, &flac));
                    slot.insert(block.to_vec());
                }
                audio_block_map.push(ContentBlock {
                    start_sector: run_start
                        + u32::try_from(i * AUDIO_BLOCK_SECTORS).unwrap(),
                    sector_count: u32::try_from(block.len() / SECTOR).unwrap(),
                    content_hash: hash,
                    sector_type,
                });
            }
        } else {
            let run_map = SectorMap {
                sectors: vec![sector_type; run_len as usize],
            };
            let mut user_data = Vec::new();
            UserDataStream::new(&mut region, &run_map)
                .read_to_end(&mut user_data)
                .unwrap();
            for chunk in user_data.chunks(DATA_CHUNK_LEN) {
                let hash = u64::from_bytes_with_seed(chunk);
                if let Entry::Vacant(slot) = data_chunks.entry(hash) {
                    let zstd = zstd::bulk::compress(chunk, 3).unwrap();
                    indices.chunk_index.insert(hash, store(&mut file, &zstd));
                    slot.insert(chunk.to_vec());
                }
                data_stream_layout.push(DataChunkLayout {
                    hash,
                    uncomp_len: u32::try_from(chunk.len()).unwrap(),
                });
            }
        }
        run_start += run_len;
    }
    file.flush().unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();

    let (exception_blob, exception_index) = exceptions.generate_blob_and_index();
    indices.exception_index = exception_index;

    let manifest = DiscManifest {
        files,
        lba_map: mapped.lba_map,
        rle_sector_map,
        audio_block_map,
        data_stream_layout,
        subheader_index: mapped.subheader_index,
        disc_exception_index: mapped.exception_index,
        integrity_hash: xxh3_64(&bin),
    };

    Archived {
        sheet,
        bin,
        manifest,
        indices,
        file,
        exception_blob,
        subheader_table: subheaders.generate_blob(),
        data_chunks,
        audio_blocks,
    }
}

// Extraction

fn verify(disc: &Archived) {
    let fetcher = |store: HashMap<u64, Vec<u8>>| {
        move |hashes: &[u64]| {
            Ok::<_, Infallible>(
                hashes.iter().map(|hash| store[hash].clone()).collect(),
            )
        }
    };
    let veri_hash: [u8; 64] = Sha512::digest(&disc.bin).into();

    verify_disc_integrity(
        &disc.manifest,
        &disc.indices.exception_index,
        &disc.exception_blob,
        &disc.subheader_table,
        &veri_hash,
        fetcher(disc.data_chunks.clone()),
        fetcher(disc.audio_blocks.clone()),
    )
    .unwrap();
}

fn write_out(disc: &Archived) -> Vec<u8> {
    let mut writer = BufWriter::new(Vec::new());
    write_disc(
        &disc.manifest,
        &disc.indices,
        &[],
        &disc.file,
        &disc.exception_blob,
        &disc.subheader_table,
        &mut writer,
    )
    .unwrap();
    writer.into_inner().unwrap()
}

/// Asserts that every way of reading `disc` back gives the original.
fn assert_reads_back(disc: &Archived) {
    verify(disc);
    assert!(write_out(disc) == disc.bin, "write_disc changed the disc");

    let reader = DiscReconstructor::new(
        &disc.manifest,
        &disc.indices,
        &[],
        &disc.file,
        &disc.exception_blob,
        &disc.subheader_table,
    )
    .unwrap();

    // Multiplying by a number coprime with the length visits every sector
    // once, out of order.
    let total = disc.bin.len() / SECTOR;
    let step = (2..total).find(|n| gcd(*n, total) == 1 && *n > total / 3)
        .unwrap_or(1);
    for visit in 0..total {
        let index = visit * step % total;
        let sector = reader
            .get_sector_data(u32::try_from(index).unwrap())
            .unwrap_or_else(|e| panic!("sector {index}: {e}"));
        assert!(
            sector[..] == disc.bin[index * SECTOR..(index + 1) * SECTOR],
            "sector {index} read back differently"
        );
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

const FULL_DISC_CUE: &str = r#"FILE "disc.bin" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
  TRACK 02 MODE2/2352
    INDEX 00 00:01:45
    INDEX 01 00:02:00
  TRACK 03 AUDIO
    INDEX 00 00:02:50
    INDEX 01 00:03:05
"#;

const FULL_DISC_SECTORS: usize = 300;

/// Form 1 data with some Form 2 sectors, a libcrypt-style sector and a blank
/// stretch; a data track with a pregap; an audio track with a pregap.
fn full_disc() -> Vec<u8> {
    image(FULL_DISC_SECTORS, |i| match i {
        40..=44 => form2(i),
        60 => with_bad_edc(form1(i), 2072),
        100..=103 => [0; SECTOR],
        0..=119 | 150..=199 => form1(i),
        // INDEX 00 of track 2: valid sectors with an all-zero subheader.
        120..=149 => data_sector(i, SectorType::Mode2Form1, [0; 8]),
        _ => audio(i),
    })
}

#[test]
fn the_disc_is_analyzed_as_built() {
    let disc = archive(full_disc(), FULL_DISC_CUE);
    let types: Vec<SectorType> = disc.manifest.rle_sector_map.runs.iter()
        .flat_map(|&(n, t)| std::iter::repeat_n(t, n as usize))
        .collect();

    assert_eq!(types.len(), FULL_DISC_SECTORS);
    assert_eq!(types[0], SectorType::Mode2Form1);
    assert_eq!(types[40], SectorType::Mode2Form2);
    assert_eq!(types[60], SectorType::Mode2Form1Exception);
    assert_eq!(types[100], SectorType::ZeroedMode2Data);
    assert_eq!(types[120], SectorType::PregapMode2);
    assert_eq!(types[150], SectorType::Mode2Form1);
    assert_eq!(types[200], SectorType::PregapAudio);
    assert_eq!(types[230], SectorType::Audio);
    assert_eq!(disc.manifest.disc_exception_index.len(), 1);
    assert_eq!(disc.manifest.lba_map, [(0, LEAD_IN)]);
}

#[test]
fn the_disc_reads_back_byte_for_byte() {
    assert_reads_back(&archive(full_disc(), FULL_DISC_CUE));
}

#[test]
fn the_generated_cue_has_the_same_layout() {
    let disc = archive(full_disc(), FULL_DISC_CUE);
    let generated = generate_cue_string(&disc.manifest, BIN_NAME).unwrap();
    let reparsed = parse(&generated, disc.bin.len() as u64);
    assert_eq!(reparsed.files, disc.sheet.files);
}

#[test]
fn a_wrong_integrity_hash_fails_write_disc() {
    let mut disc = archive(full_disc(), FULL_DISC_CUE);
    disc.manifest.integrity_hash ^= 1;

    let mut writer = BufWriter::new(Vec::new());
    write_disc(
        &disc.manifest,
        &disc.indices,
        &[],
        &disc.file,
        &disc.exception_blob,
        &disc.subheader_table,
        &mut writer,
    )
    .unwrap_err();
}

// Data pregaps that aren't blank

const MODE2_PREGAP_CUE: &str = r#"FILE "disc.bin" BINARY
  TRACK 01 MODE2/2352
    INDEX 00 00:00:00
    INDEX 01 00:00:20
"#;

const MODE1_PREGAP_CUE: &str = r#"FILE "disc.bin" BINARY
  TRACK 01 MODE1/2352
    INDEX 00 00:00:00
    INDEX 01 00:00:20
"#;

const PREGAP_DISC_SECTORS: usize = 40;

#[test]
fn a_mode2_pregap_with_a_subheader_reads_back() {
    let bin = image(PREGAP_DISC_SECTORS, form1);
    assert_reads_back(&archive(bin, MODE2_PREGAP_CUE));
}

#[test]
fn a_damaged_mode2_pregap_sector_reads_back() {
    let bin = image(PREGAP_DISC_SECTORS, |i| match i {
        5 => with_bad_edc(form1(i), 2072),
        _ => form1(i),
    });
    let disc = archive(bin, MODE2_PREGAP_CUE);
    assert_eq!(disc.manifest.disc_exception_index.len(), 1);
    assert_reads_back(&disc);
}

#[test]
fn a_mode1_pregap_with_data_reads_back() {
    let bin = image(PREGAP_DISC_SECTORS, |i| mode1(i, [0; 8]));
    assert_reads_back(&archive(bin, MODE1_PREGAP_CUE));
}

#[test]
fn a_damaged_mode1_pregap_sector_reads_back() {
    let bin = image(PREGAP_DISC_SECTORS, |i| match i {
        // Nonzero reserved bytes, which must come from 2068..2076.
        5 => with_bad_edc(mode1(i, [9; 8]), 2064),
        _ => mode1(i, [0; 8]),
    });
    let disc = archive(bin, MODE1_PREGAP_CUE);
    assert_eq!(disc.manifest.disc_exception_index.len(), 1);
    assert_reads_back(&disc);
}
