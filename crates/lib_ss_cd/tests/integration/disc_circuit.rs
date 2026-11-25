//! Runs synthetic discs through the whole library: CUE analysis, sector
//! mapping, storing the data and audio streams in an archive file, then
//! verifying, writing and randomly reading the disc back.
//!
//! The discs are generated here so nothing large is committed.

use std::{
    collections::{HashMap, hash_map::Entry},
    convert::Infallible,
    io::{self, BufWriter, Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use sha2::{Digest, Sha512};
use sprite_shrink::{ChunkLocation, ChunkMap, Hashable, IsCancelled};
use sprite_shrink_cd::{
    ContentBlock, CueSheet, DataChunkLayout, DiscManifest, DiscReconstructor,
    ExceptionRegistry, ExceptionTable, FileSizeProvider, SSMDIndices,
    SectorDataProvider, SectorMap, SectorRegionStream, SectorType,
    SubheaderRegistry, SubheaderTable, UserDataStream,
    analyze_and_map_disc, analyze_cue_sheets,
    compress_audio_block, disc_files, generate_cue_string,
    rebuild_sector_simd, resolve_file_sector_counts, rle_encode_map,
    verify_disc_integrity, write_disc,
};
use xxhash_rust::xxh3::xxh3_64;

const SECTOR: usize = 2352;

/// Threads `write_disc` uses: several decoders per stream, so every disc
/// read back here also checks the decoders keep the order.
const WORKERS: usize = 4;
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
    numbered_sector(number(index), sector_type, metadata)
}

/// A data sector whose header holds `sector_number`.
fn numbered_sector(
    sector_number: u32,
    sector_type: SectorType,
    metadata: [u8; 8],
) -> [u8; SECTOR] {
    let user_data = payload(usize::from(sector_type.data_size()), sector_number);
    rebuild_sector_simd(&user_data, &metadata, sector_type, sector_number, None)
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

/// The size of each BIN file, by the name the CUE sheet gives it.
struct BinSizes(HashMap<String, u64>);

impl FileSizeProvider for BinSizes {
    fn get_file_size_bytes(
        &self,
        filename: &str,
    ) -> Result<u64, std::io::Error> {
        Ok(self.0[filename])
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
    archive: Vec<u8>,
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
fn store(archive: &mut Vec<u8>, compressed: &[u8]) -> ChunkLocation {
    let offset = archive.len() as u64;
    archive.extend_from_slice(compressed);
    ChunkLocation {
        offset,
        compressed_length: u32::try_from(compressed.len()).unwrap(),
    }
}

/// Reads stored data out of `archive`, as a host reads it from the archive
/// file. Data past the end is an error.
fn read_stored(
    archive: &[u8],
) -> impl Fn(ChunkLocation) -> io::Result<Vec<u8>> + Sync + '_ {
    move |location| {
        usize::try_from(location.offset).ok()
            .and_then(|start| {
                archive.get(start..start + location.compressed_length as usize)
            })
            .map(<[u8]>::to_vec)
            .ok_or_else(|| io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stored data runs past the end of the archive",
            ))
    }
}

fn archive(bin: Vec<u8>, cue: &str) -> Archived {
    archive_files(vec![(BIN_NAME, bin)], cue)
}

/// Sorts the shared tables and renumbers the disc against them, as archiving
/// does once every disc is analysed.
fn sort_tables(
    subheaders: &SubheaderRegistry,
    exceptions: &ExceptionRegistry,
    manifest: &mut DiscManifest<u64>,
) -> (SubheaderTable, ExceptionTable) {
    let subheader_table = subheaders.sorted_table().unwrap();
    let exception_table = exceptions.sorted_table().unwrap();
    subheader_table.renumber(&mut manifest.subheader_index).unwrap();
    exception_table.renumber(&mut manifest.disc_exception_index).unwrap();
    (subheader_table, exception_table)
}

/// Archives a disc stored as several BIN files, given in the CUE sheet's
/// order. The sectors are read from the files joined, as the CLI reads
/// them.
fn archive_files(bins: Vec<(&str, Vec<u8>)>, cue: &str) -> Archived {
    let sizes = BinSizes(
        bins.iter()
            .map(|(name, bin)| ((*name).to_string(), bin.len() as u64))
            .collect(),
    );
    let bin: Vec<u8> = bins.into_iter().flat_map(|(_, bin)| bin).collect();
    let sheet = parse(cue, bin.len() as u64);
    let sector_counts = resolve_file_sector_counts(&sheet, &sizes).unwrap();
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

    let mut stored = Vec::new();
    let mut source = Cursor::new(bin.clone());
    let mut indices = SSMDIndices {
        chunk_index: ChunkMap::default(),
        audio_block_index: ChunkMap::default(),
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
                    indices.audio_block_index.insert(hash, store(&mut stored, &flac));
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
                    indices.chunk_index.insert(hash, store(&mut stored, &zstd));
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

    let mut manifest = DiscManifest {
        files,
        lba_map: mapped.lba_map,
        rle_sector_map,
        audio_block_map,
        data_stream_layout,
        subheader_index: mapped.subheader_index,
        disc_exception_index: mapped.exception_index,
        integrity_hash: xxh3_64(&bin),
    };
    let (subheader_table, exception_table) =
        sort_tables(&subheaders, &exceptions, &mut manifest);
    indices.exception_index = exception_table.index;

    Archived {
        sheet,
        bin,
        manifest,
        indices,
        archive: stored,
        exception_blob: exception_table.blob,
        subheader_table: subheader_table.entries,
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
    let mut reported = 0;

    verify_disc_integrity(
        &disc.manifest,
        &disc.indices.exception_index,
        &disc.exception_blob,
        &disc.subheader_table,
        &veri_hash,
        fetcher(disc.data_chunks.clone()),
        fetcher(disc.audio_blocks.clone()),
        |bytes| reported += bytes,
    )
    .unwrap();
    assert_eq!(reported, disc.bin.len() as u64, "verification progress");
}

fn write_out(disc: &Archived) -> Vec<u8> {
    let mut writer = BufWriter::new(Vec::new());
    let mut reported = 0;
    write_disc(
        &disc.manifest,
        &disc.indices,
        &[],
        read_stored(&disc.archive),
        &disc.exception_blob,
        &disc.subheader_table,
        &mut writer,
        WORKERS,
        || false,
        |bytes| reported += bytes,
    )
    .unwrap();
    assert_eq!(reported, disc.bin.len() as u64, "write_disc progress");
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
        read_stored(&disc.archive),
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

/// Form 1 data with some Form 2 sectors (one with a broken EDC), a
/// libcrypt-style sector and a blank stretch; a data track with a pregap;
/// an audio track with a pregap.
fn full_disc() -> Vec<u8> {
    image(FULL_DISC_SECTORS, |i| match i {
        40..=44 => form2(i),
        45 => with_bad_edc(form2(i), 2348),
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
    assert_eq!(types[45], SectorType::Mode2Form2Exception);
    assert_eq!(types[60], SectorType::Mode2Form1Exception);
    assert_eq!(types[100], SectorType::ZeroedMode2Data);
    assert_eq!(types[120], SectorType::PregapMode2);
    assert_eq!(types[150], SectorType::Mode2Form1);
    assert_eq!(types[200], SectorType::PregapAudio);
    assert_eq!(types[230], SectorType::Audio);
    assert_eq!(disc.manifest.disc_exception_index.len(), 2);
    assert_eq!(disc.manifest.lba_map, [(0, LEAD_IN)]);
}

#[test]
fn the_disc_reads_back_byte_for_byte() {
    assert_reads_back(&archive(full_disc(), FULL_DISC_CUE));
}

/// Points the two lowest hashes in `index` at each other's stored data,
/// which still decodes, but to the wrong bytes for its hash.
fn swap_first_two(index: &mut ChunkMap<u64, ChunkLocation>) {
    let mut hashes: Vec<u64> = index.keys().copied().collect();
    hashes.sort_unstable();
    let (a, b) = (hashes[0], hashes[1]);
    let location_a = index[&a];
    let location_b = index.insert(b, location_a).unwrap();
    index.insert(a, location_b);
}

/// Reads every sector of `disc` with `get_sector_data`, asserts that none
/// comes back different from the original, and returns the errors.
fn failed_sector_reads(disc: &Archived) -> Vec<String> {
    let reader = DiscReconstructor::new(
        &disc.manifest,
        &disc.indices,
        &[],
        read_stored(&disc.archive),
        &disc.exception_blob,
        &disc.subheader_table,
    )
    .unwrap();

    let mut errors = Vec::new();
    for (index, original) in disc.bin.chunks(SECTOR).enumerate() {
        match reader.get_sector_data(u32::try_from(index).unwrap()) {
            Ok(sector) => {
                assert!(sector[..] == *original, "sector {index} read back wrong");
            }
            Err(e) => errors.push(e.to_string()),
        }
    }
    errors
}

#[test]
fn a_damaged_data_chunk_is_not_read_back() {
    let mut disc = archive(full_disc(), FULL_DISC_CUE);
    swap_first_two(&mut disc.indices.chunk_index);

    let errors = failed_sector_reads(&disc);
    assert!(!errors.is_empty(), "no sector read failed");
    for e in &errors {
        assert!(e.contains("Chunk failed its integrity check"), "{e}");
    }
}

#[test]
fn a_damaged_audio_block_is_not_read_back() {
    let mut disc = archive(full_disc(), FULL_DISC_CUE);
    swap_first_two(&mut disc.indices.audio_block_index);

    let errors = failed_sector_reads(&disc);
    assert!(!errors.is_empty(), "no sector read failed");
    for e in &errors {
        assert!(e.contains("Audio block failed its integrity check"), "{e}");
    }
}

/// A sector read stops decoding an audio block at its recorded length.
/// With the record one sector short, the block's last sector is left
/// uncovered, and the others fail as too long.
#[test]
fn a_sector_read_rejects_an_audio_block_longer_than_its_record() {
    let mut disc = archive(full_disc(), FULL_DISC_CUE);
    disc.manifest.audio_block_map[0].sector_count -= 1;

    let errors = failed_sector_reads(&disc);
    assert!(
        errors.iter().any(|e| e.contains("Audio block decodes to more than")),
        "{errors:?}"
    );
}

/// A manifest that doesn't cover every sector fails on the uncovered ones,
/// rather than returning sectors rebuilt around zeros. The data cut drops
/// whole chunks, at least 3000 bytes, so at least the last 2048-byte
/// sector is uncovered. (Shortening a chunk's recorded length instead
/// fails earlier, as a chunk too long for its record.) The audio cut drops
/// the last block.
#[test]
fn sectors_the_manifest_does_not_cover_are_not_made_up() {
    let mut short_data = archive(full_disc(), FULL_DISC_CUE);
    let layout = &mut short_data.manifest.data_stream_layout;
    let mut cut = 0;
    while cut < 3000 {
        cut += layout.pop().unwrap().uncomp_len;
    }

    let mut short_audio = archive(full_disc(), FULL_DISC_CUE);
    short_audio.manifest.audio_block_map.pop().unwrap();

    for disc in [short_data, short_audio] {
        let errors = failed_sector_reads(&disc);
        assert!(!errors.is_empty(), "no sector read failed");
        for e in &errors {
            assert!(e.contains("doesn't hold all the data for sector"), "{e}");
        }
    }
}

/// The full disc with each track in its own file, as Redump stores discs.
/// Index times are relative to the file they're in.
const SPLIT_DISC_CUE: &str = r#"FILE "track1.bin" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
FILE "track2.bin" BINARY
  TRACK 02 MODE2/2352
    INDEX 00 00:00:00
    INDEX 01 00:00:30
FILE "track3.bin" BINARY
  TRACK 03 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:00:30
"#;

#[test]
fn a_disc_split_into_one_file_per_track_maps_as_one_file() {
    let joined = full_disc();
    let split = archive_files(
        vec![
            ("track1.bin", joined[..120 * SECTOR].to_vec()),
            ("track2.bin", joined[120 * SECTOR..200 * SECTOR].to_vec()),
            ("track3.bin", joined[200 * SECTOR..].to_vec()),
        ],
        SPLIT_DISC_CUE,
    );

    assert_eq!(
        split.manifest.rle_sector_map,
        archive(joined, FULL_DISC_CUE).manifest.rle_sector_map
    );
    assert_reads_back(&split);
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
        read_stored(&disc.archive),
        &disc.exception_blob,
        &disc.subheader_table,
        &mut writer,
        WORKERS,
        || false,
        |_| {},
    )
    .unwrap_err();
}

const ONE_AUDIO_TRACK_CUE: &str = r#"FILE "disc.bin" BINARY
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#;

#[test]
fn audio_past_the_first_fetch_is_verified() {
    let sectors = 65 * AUDIO_BLOCK_SECTORS;
    let disc = archive(
        image(sectors, |i| audio(i % AUDIO_BLOCK_SECTORS)),
        ONE_AUDIO_TRACK_CUE,
    );
    assert_eq!(disc.audio_blocks.len(), 1);
    assert_eq!(disc.manifest.audio_block_map.len(), 65);

    verify(&disc);
}

/// A writer whose every write fails, as a full disk does.
struct FullDisk;

impl Write for FullDisk {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("no space left on device"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const ONE_DATA_TRACK_CUE: &str = r#"FILE "disc.bin" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
"#;

/// Enough Form 1 sectors (2048 bytes of user data each) for the data stream
/// to outgrow `write_disc`'s 25 MB read-ahead buffer.
const LARGE_DISC_SECTORS: usize = 20_000;

/// How `write_disc` failed: whether the error is a cancellation, and its
/// message.
type WriteError = (bool, String);

/// Runs `write_disc` on another thread and returns its result, or `None` if
/// it hasn't returned within a minute.
fn write_disc_or_hang<W>(
    disc: Archived,
    writer: W,
    is_cancelled: impl Fn() -> bool + Send + 'static,
) -> Option<Result<(), WriteError>>
where
    W: Write + Send + 'static,
{
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut writer = BufWriter::new(writer);
        let result = write_disc(
            &disc.manifest,
            &disc.indices,
            &[],
            read_stored(&disc.archive),
            &disc.exception_blob,
            &disc.subheader_table,
            &mut writer,
            WORKERS,
            is_cancelled,
            |_| {},
        );
        // Fails only if the test already gave up waiting, which the test
        // reports itself.
        let _unsent = done_tx
            .send(result.map_err(|err| (err.is_cancelled(), err.to_string())));
    });
    done_rx.recv_timeout(std::time::Duration::from_secs(60)).ok()
}

#[test]
fn a_failed_write_stops_write_disc() {
    let disc = archive(image(LARGE_DISC_SECTORS, form1), ONE_DATA_TRACK_CUE);

    let result = write_disc_or_hang(disc, FullDisk, || false)
        .expect("write_disc hung after a write failed");
    let (cancelled, err) = result.unwrap_err();
    assert!(!cancelled, "{err}");
    assert!(err.contains("no space left on device"), "{err}");
}

/// `write_disc` stops decoding a data chunk at the length the manifest
/// records for it.
#[test]
fn write_disc_rejects_a_chunk_longer_than_its_record() {
    let mut disc = archive(full_disc(), FULL_DISC_CUE);
    disc.manifest.data_stream_layout[0].uncomp_len -= 1;

    let result = write_disc_or_hang(disc, std::io::sink(), || false)
        .expect("write_disc hung after a chunk failed");
    let (cancelled, err) = result.unwrap_err();
    assert!(!cancelled, "{err}");
    assert!(err.contains("Chunk decodes to more than"), "{err}");
}

/// The same for an audio block and its recorded sector count.
#[test]
fn write_disc_rejects_an_audio_block_longer_than_its_record() {
    let mut disc = archive(full_disc(), FULL_DISC_CUE);
    disc.manifest.audio_block_map[0].sector_count -= 1;

    let result = write_disc_or_hang(disc, std::io::sink(), || false)
        .expect("write_disc hung after an audio block failed");
    let (cancelled, err) = result.unwrap_err();
    assert!(!cancelled, "{err}");
    assert!(err.contains("Audio block decodes to more than"), "{err}");
}

/// A failed read of stored data stops `write_disc` and fails sector
/// reads, with the host's error, for data chunks and audio blocks alike.
#[test]
fn a_failed_read_of_stored_data_is_reported() {
    // Every chunk, or every audio block, recorded one byte past the end.
    let past_the_end = |audio: bool| {
        let mut disc = archive(full_disc(), FULL_DISC_CUE);
        let archive_len = disc.archive.len() as u64;
        let index = if audio {
            &mut disc.indices.audio_block_index
        } else {
            &mut disc.indices.chunk_index
        };
        for location in index.values_mut() {
            location.offset =
                archive_len - u64::from(location.compressed_length) + 1;
        }
        disc
    };
    let host_error = "stored data runs past the end of the archive";

    for audio in [false, true] {
        let errors = failed_sector_reads(&past_the_end(audio));
        assert!(
            errors.iter().any(|e| e.contains(host_error)),
            "audio {audio}: {errors:?}"
        );

        let result = write_disc_or_hang(past_the_end(audio), io::sink(), || false)
            .expect("write_disc hung after a read failed");
        let (cancelled, err) = result.unwrap_err();
        assert!(!cancelled, "{err}");
        assert!(err.contains(host_error), "audio {audio}: {err}");
    }
}

#[test]
fn write_disc_stops_when_cancelled() {
    let disc = archive(image(LARGE_DISC_SECTORS, form1), ONE_DATA_TRACK_CUE);

    // Cancelled partway through, so the workers are mid-stream.
    let checks = AtomicUsize::new(0);
    let is_cancelled = move || checks.fetch_add(1, Ordering::Relaxed) >= 1000;
    let written = Arc::new(Mutex::new(Vec::new()));

    let result = write_disc_or_hang(
        disc,
        SharedSink(Arc::clone(&written)),
        is_cancelled,
    )
    .expect("write_disc hung after it was cancelled");
    let (cancelled, err) = result.unwrap_err();
    assert!(cancelled, "{err}");
    // `BufWriter` holds back what it hasn't flushed, so at most the 1000
    // sectors before the cancel reach the sink.
    assert!(written.lock().unwrap().len() <= 1000 * SECTOR);
}

#[test]
fn write_disc_cancelled_before_the_first_sector_writes_nothing() {
    let disc = archive(full_disc(), FULL_DISC_CUE);
    let written = Arc::new(Mutex::new(Vec::new()));

    let result =
        write_disc_or_hang(disc, SharedSink(Arc::clone(&written)), || true)
            .expect("write_disc hung after it was cancelled");
    let (cancelled, err) = result.unwrap_err();
    assert!(cancelled, "{err}");
    assert!(written.lock().unwrap().is_empty());
}

/// A writer the test can read back after `write_disc_or_hang` moves it to
/// another thread.
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|err| std::io::Error::other(err.to_string()))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

const ONE_TRACK_MODE2_CUE: &str = r#"FILE "disc.bin" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
"#;

/// Where the header numbers jump in `jumping_disc`, and by how much.
const JUMP_AT: usize = 400;
const JUMP_BY: u32 = 10_000;

/// Form 1 sectors whose header numbers jump forward by `JUMP_BY` at sector
/// `JUMP_AT`, as at a GD-ROM's gap between its two areas.
fn jumping_disc() -> Vec<u8> {
    image(600, |i| {
        let jump = if i < JUMP_AT { 0 } else { JUMP_BY };
        numbered_sector(number(i) + jump, SectorType::Mode2Form1, FORM1_SUBHEADER)
    })
}

#[test]
fn header_numbers_that_jump_mid_disc_are_mapped_where_they_jump() {
    let disc = archive(jumping_disc(), ONE_TRACK_MODE2_CUE);
    assert_eq!(
        disc.manifest.lba_map,
        [(0, LEAD_IN), (u32::try_from(JUMP_AT).unwrap(), LEAD_IN + JUMP_BY)]
    );
    assert_eq!(disc.manifest.disc_exception_index, []);
}

#[test]
fn a_disc_whose_header_numbers_jump_reads_back() {
    assert_reads_back(&archive(jumping_disc(), ONE_TRACK_MODE2_CUE));
}

/// A Dreamcast disc as Redump stores it, one BIN per track, with the
/// high-density area at sector 45000. Track 5 has a two-second PREGAP
/// that isn't in its file, so its sector headers jump past it.
const GDROM_CUE: &str = r#"REM SINGLE-DENSITY AREA
FILE "track01.bin" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
FILE "track02.bin" BINARY
  TRACK 02 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00
REM HIGH-DENSITY AREA
FILE "track03.bin" BINARY
  TRACK 03 MODE1/2352
    INDEX 01 00:00:00
FILE "track04.bin" BINARY
  TRACK 04 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00
FILE "track05.bin" BINARY
  TRACK 05 MODE1/2352
    PREGAP 00:02:00
    INDEX 01 00:00:00
"#;

#[test]
fn a_gdrom_keeps_its_areas_and_gaps() {
    // `first_lba` is the sector's address on the disc; headers hold it
    // plus the 150-sector lead-in.
    let data = |first_lba: u32, len: u32| -> Vec<u8> {
        (0..len)
            .flat_map(|k| {
                numbered_sector(first_lba + k + LEAD_IN, SectorType::Mode1, [0; 8])
            })
            .collect()
    };
    let pcm = |seed: usize, len: usize| image(len, |i| audio(seed + i));
    let disc = archive_files(
        vec![
            ("track01.bin", data(0, 300)),
            ("track02.bin", pcm(10_000, 225)),
            ("track03.bin", data(45_000, 300)),
            ("track04.bin", pcm(20_000, 225)),
            ("track05.bin", data(45_525 + 150, 300)),
        ],
        GDROM_CUE,
    );

    // Each jump in the headers, at the high-density area and after the
    // PREGAP, is recorded where the joined image reaches it.
    assert_eq!(
        disc.manifest.lba_map,
        [(0, LEAD_IN), (525, 44_475 + LEAD_IN), (1050, 44_625 + LEAD_IN)]
    );
    assert_reads_back(&disc);
    assert_eq!(
        generate_cue_string(&disc.manifest, BIN_NAME).unwrap(),
        "REM SINGLE-DENSITY AREA\n\
         FILE \"disc.bin\" BINARY\n\
         \x20 TRACK 01 MODE1/2352\n\
         \x20   INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n\
         \x20   INDEX 00 00:04:00\n\
         \x20   INDEX 01 00:06:00\n\
         REM HIGH-DENSITY AREA\n\
         \x20 TRACK 03 MODE1/2352\n\
         \x20   INDEX 01 00:07:00\n\
         \x20 TRACK 04 AUDIO\n\
         \x20   INDEX 00 00:11:00\n\
         \x20   INDEX 01 00:13:00\n\
         \x20 TRACK 05 MODE1/2352\n\
         \x20   PREGAP 00:02:00\n\
         \x20   INDEX 01 00:14:00\n"
    );
}

// Sectors that pass EDC and ECC but don't rebuild, and sectors of no mode

const MODE1_ONE_TRACK_CUE: &str = r#"FILE "disc.bin" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
"#;

/// A disc with one odd sector: what the case is called, its CUE sheet,
/// the odd sector, and the type it must be stored as.
type OddSectorCase = (&'static str, &'static str, fn(usize) -> [u8; SECTOR], SectorType);

fn with_byte(mut sector: [u8; SECTOR], at: usize, value: u8) -> [u8; SECTOR] {
    sector[at] = value;
    sector
}

#[test]
fn sectors_that_dont_rebuild_plain_are_stored_whole() {
    // Mode 2 EDC starts after the header and Mode 2 ECC treats the header
    // as zero, so the first four of these passed both checks, were stored
    // plain, and failed the whole disc's verification. A header below its
    // index couldn't give an offset, so its sector got the one before.
    // The Mode 0 and mode 3 sectors stopped analysis.
    let odd: [OddSectorCase; 7] = [
        ("Form 1, changed sync", ONE_TRACK_MODE2_CUE,
            |i| with_byte(form1(i), 3, 0xFE), SectorType::Mode2Form1Exception),
        ("Form 2, changed sync", ONE_TRACK_MODE2_CUE,
            |i| with_byte(form2(i), 3, 0xFE), SectorType::Mode2Form2Exception),
        ("Form 1, non-BCD header", ONE_TRACK_MODE2_CUE,
            |i| with_byte(form1(i), 13, 0x0A), SectorType::Mode2Form1Exception),
        ("Form 1, header below its index", ONE_TRACK_MODE2_CUE,
            |_| numbered_sector(3, SectorType::Mode2Form1, FORM1_SUBHEADER),
            SectorType::Mode2Form1Exception),
        ("Mode 1, header below its index", MODE1_ONE_TRACK_CUE,
            |_| numbered_sector(3, SectorType::Mode1, [0; 8]),
            SectorType::Mode1Exception),
        ("Mode 0 with a header", ONE_TRACK_MODE2_CUE,
            |i| {
                let mut sector = with_byte(form1(i), 15, 0);
                sector[16..].fill(0);
                sector
            },
            SectorType::Mode1Exception),
        ("mode 3", ONE_TRACK_MODE2_CUE,
            |i| with_byte(form1(i), 15, 3), SectorType::Mode1Exception),
    ];

    for (case, cue, odd_sector, expected) in odd {
        let plain = if cue == MODE1_ONE_TRACK_CUE {
            |i| mode1(i, [0; 8])
        } else {
            form1
        };
        let bin = image(40, |i| if i == 10 { odd_sector(i) } else { plain(i) });
        let disc = archive(bin, cue);

        let types: Vec<SectorType> = disc.manifest.rle_sector_map.runs.iter()
            .flat_map(|&(n, t)| std::iter::repeat_n(t, n as usize))
            .collect();
        assert_eq!(types[10], expected, "{case}");
        assert_eq!(disc.manifest.disc_exception_index.len(), 1, "{case}");
        // Only a valid header at or past its index moves the offset.
        assert_eq!(disc.manifest.lba_map, [(0, LEAD_IN)], "{case}");
        assert_reads_back(&disc);
    }
}
