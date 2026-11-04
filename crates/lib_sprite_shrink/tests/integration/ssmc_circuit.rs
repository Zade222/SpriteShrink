//! Builds an SSMC archive with the public API and reads every file back,
//! the way a host would.
//!
//! Writing goes through the same functions the CLI uses, so this covers the
//! code that produces real archives, without touching the filesystem.

use std::{
    collections::HashMap,
    fmt::Display,
    sync::{Arc, Mutex},
};

use bitcode::{Decode, Encode};
use dashmap::DashMap;
use serde::Deserialize;
use sprite_shrink::{
    HASH_TYPE_XXH3_64, HASH_TYPE_XXH3_128,
    ArchiveBuilder, FileData, FileHeader, Hashable, ParsedSSMC, SSMCFormatData,
    SpriteShrinkError,
    build_ssmc_prefix, create_file_manifest_and_chunks, decompress_chunk,
    get_seek_chunks, parse_file_header, parse_format_data, parse_ssmc_prefix,
    process_file_in_memory, serialize_uncompressed_data, verify_single_file,
};
use proptest::{prelude::*, sample::Index};
use sprite_shrink_test_support::{
    XorShift, assert_bytes_eq, rom_set, tiled_bytes,
};

/// Chunk sizes average 1 KiB, so each test file splits into dozens of
/// chunks.
const WINDOW_SIZE: u64 = 1024;
const DICTIONARY_SIZE: u64 = 4 * 1024;
const COMPRESSION_LEVEL: i32 = 3;
const WORKER_COUNT: usize = 2;

/// The bounds every hash type in these tests meets: what chunking,
/// serializing, compressing and parsing each require.
trait TestHash:
    Hashable + Display + Ord + Encode + for<'de> Decode<'de>
    + for<'de> Deserialize<'de>
{
}

impl<H> TestHash for H where
    H: Hashable + Display + Ord + Encode + for<'de> Decode<'de>
        + for<'de> Deserialize<'de>
{
}

/// A built archive and the SHA-512 of each file put into it.
struct Built {
    archive: Vec<u8>,
    veri_hashes: HashMap<String, [u8; 64]>,
}

/// Builds the whole archive: the prefix from `build_ssmc_prefix`, then the
/// compressed data the archive builder wrote.
fn build_archive<H: TestHash>(files: &[FileData], hash_type: u8) -> Built {
    let file_manifest = DashMap::new();
    let mut store: HashMap<H, Vec<u8>> = HashMap::new();
    let mut veri_hashes = HashMap::new();

    for file in files {
        let processed = process_file_in_memory(file.clone(), WINDOW_SIZE)
            .unwrap()
            .unwrap();
        let (fmp, chunks) = create_file_manifest_and_chunks::<H>(
            &processed.file_data,
            &processed.chunks,
        );
        file_manifest.insert(processed.file_name.clone(), fmp);
        veri_hashes.insert(processed.file_name, processed.veri_hash);
        store.extend(chunks);
    }

    let total_size = store.values().map(|chunk| chunk.len() as u64).sum();
    // The callbacks must be `'static`, so they share the store through an
    // `Arc` rather than borrowing it.
    let store = Arc::new(store);
    let get_chunks = move |hashes: &[H]| {
        Ok::<_, SpriteShrinkError>(
            hashes.iter().map(|hash| store[hash].clone()).collect()
        )
    };

    let ser = serialize_uncompressed_data(&file_manifest);

    // The write callback must be `'static` too, so the test keeps a second
    // handle to the buffer to read it back after `build`.
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let mut builder = ArchiveBuilder::new(
        &ser.sorted_hashes,
        total_size,
        get_chunks,
        move |data: &[u8]| {
            sink.lock().unwrap().extend_from_slice(data);
            Ok::<_, SpriteShrinkError>(())
        },
    );
    builder
        .compression_level(COMPRESSION_LEVEL)
        .dictionary_size(DICTIONARY_SIZE)
        .worker_threads(WORKER_COUNT);
    let comp = builder.build().unwrap();

    let mut archive = build_ssmc_prefix(
        &ser.archive_toc,
        &ser.ser_file_manifest,
        &comp,
        hash_type,
    )
    .unwrap();
    archive.extend_from_slice(&written.lock().unwrap());

    Built { archive, veri_hashes }
}

/// An opened archive: its bytes and the metadata parsed from its prefix.
struct Reader<H> {
    archive: Vec<u8>,
    parsed: ParsedSSMC<H>,
}

impl<H: TestHash> Reader<H> {
    /// Opens `archive` the way a host reading from disk would: the header,
    /// then the format data it points to, then exactly the prefix the format
    /// data says the metadata needs.
    fn open(archive: Vec<u8>) -> Self {
        let header_len = usize::try_from(FileHeader::HEADER_SIZE).unwrap();
        let header = parse_file_header(&archive[..header_len]).unwrap();

        let format_start = usize::try_from(header.format_data_offset).unwrap();
        let format_len = usize::try_from(SSMCFormatData::SIZE).unwrap();
        let format_data = parse_format_data(
            &archive[format_start..format_start + format_len]
        )
        .unwrap();

        let prefix_end =
            usize::try_from(format_data.metadata_prefix_end()).unwrap();
        let parsed = parse_ssmc_prefix(&archive[..prefix_end]).unwrap();

        Self { archive, parsed }
    }

    /// Decompresses one chunk from the archive's data section.
    fn chunk(&self, hash: &H) -> Vec<u8> {
        let location = &self.parsed.chunk_index[hash];
        let start = usize::try_from(location.offset).unwrap();
        let end = start + usize::try_from(location.compressed_length).unwrap();
        decompress_chunk(&self.archive[start..end], &self.parsed.dictionary)
            .unwrap()
    }
}

/// Returns the original contents of the file named `name`.
fn original<'a>(files: &'a [FileData], name: &str) -> &'a [u8] {
    &files
        .iter()
        .find(|file| file.file_name == name)
        .unwrap()
        .file_data
}

fn open<H: TestHash>(files: &[FileData], hash_type: u8) -> (Built, Reader<H>) {
    let built = build_archive::<H>(files, hash_type);
    let reader = Reader::open(built.archive.clone());
    (built, reader)
}

fn rom_set_files() -> Vec<FileData> {
    rom_set()
        .into_iter()
        .map(|(file_name, file_data)| FileData { file_name, file_data })
        .collect()
}

/// Checks that the header, TOC and manifests list `files`.
fn assert_archive_lists_files<H: TestHash>(
    files: &[FileData],
    reader: &Reader<H>,
    hash_type: u8,
) {
    let parsed = &reader.parsed;

    assert_eq!(parsed.header.hash_type, hash_type);
    assert_eq!(usize::try_from(parsed.header.file_count).unwrap(), files.len());
    assert_eq!(parsed.toc.len(), files.len());
    assert_eq!(parsed.manifests.len(), files.len());

    for (entry, manifest) in parsed.toc.iter().zip(&parsed.manifests) {
        let data = original(files, &entry.filename);
        assert_eq!(
            entry.uncompressed_size,
            data.len() as u64,
            "{}",
            entry.filename
        );
        assert_eq!(
            usize::try_from(manifest.chunk_count).unwrap(),
            manifest.chunk_metadata.len(),
            "{}",
            entry.filename
        );
    }
}

fn assert_rom_set_described<H: TestHash>(hash_type: u8) {
    let files = rom_set_files();
    let (built, reader) = open::<H>(&files, hash_type);
    assert_archive_lists_files(&files, &reader, hash_type);

    // The patched file shares most chunks with the base, so the archive
    // stores fewer chunks than the manifests reference.
    let parsed = &reader.parsed;
    let referenced: usize = parsed
        .manifests
        .iter()
        .map(|manifest| manifest.chunk_metadata.len())
        .sum();
    assert!(
        parsed.chunk_index.len() < referenced,
        "{} stored chunks for {referenced} references",
        parsed.chunk_index.len()
    );
    let total: usize = files.iter().map(|file| file.file_data.len()).sum();
    assert!(built.archive.len() < total);

    // There's enough distinct data to train on, so the archive must not
    // fall back to compressing without a dictionary.
    assert_ne!(parsed.dictionary, [] as [u8; 0]);
}

/// Checks that every file rebuilt from its manifest's chunks equals the
/// original.
fn assert_files_extract<H: TestHash>(files: &[FileData], reader: &Reader<H>) {
    let parsed = &reader.parsed;
    for (entry, manifest) in parsed.toc.iter().zip(&parsed.manifests) {
        let mut rebuilt = Vec::new();
        for meta in &manifest.chunk_metadata {
            let chunk = reader.chunk(&meta.hash);
            assert_eq!(
                chunk.len(),
                usize::try_from(meta.length).unwrap(),
                "{}",
                entry.filename
            );
            assert_eq!(
                usize::try_from(meta.offset).unwrap(),
                rebuilt.len(),
                "{}",
                entry.filename
            );
            rebuilt.extend(chunk);
        }
        assert_bytes_eq(
            &rebuilt,
            original(files, &entry.filename),
            &entry.filename,
        );
    }
}

/// Runs `verify_single_file` on file `index`, reading its chunks from the
/// archive, against `veri_hash`.
fn verify<H: TestHash>(
    reader: &Arc<Reader<H>>,
    index: usize,
    veri_hash: &[u8; 64],
) -> Result<(), SpriteShrinkError> {
    let source = Arc::clone(reader);
    let get_chunks = move |hashes: &[H]| {
        Ok::<_, SpriteShrinkError>(
            hashes.iter().map(|hash| source.chunk(hash)).collect()
        )
    };
    verify_single_file(
        reader.parsed.toc[index].filename.clone(),
        &reader.parsed.manifests[index],
        veri_hash,
        get_chunks,
        |_| {},
    )
}

/// Checks that every file in the archive passes verification against its
/// own hash.
fn assert_files_verify<H: TestHash>(built: &Built, reader: &Arc<Reader<H>>) {
    for (index, entry) in reader.parsed.toc.iter().enumerate() {
        verify(reader, index, &built.veri_hashes[&entry.filename]).unwrap();
    }
}

fn assert_rom_set_verifies<H: TestHash>(hash_type: u8) {
    let (built, reader) = open::<H>(&rom_set_files(), hash_type);
    // `verify_single_file` needs a `'static` chunk callback.
    let reader = Arc::new(reader);
    assert_files_verify(&built, &reader);

    // Each file's hash must not verify another file, or a check that always
    // passes would go unnoticed. The ROM set's files all differ.
    let toc = &reader.parsed.toc;
    for (index, entry) in toc.iter().enumerate() {
        let other = &toc[(index + 1) % toc.len()].filename;
        assert!(
            verify(&reader, index, &built.veri_hashes[other]).is_err(),
            "{} verified against the hash of {other}",
            entry.filename
        );
    }
}

/// Byte ranges to seek, as `(offset, length)`: the edges of the file, every
/// chunk boundary, and pseudo-random ranges.
fn seek_ranges(len: u64, boundaries: &[u64], seed: u64) -> Vec<(u64, u64)> {
    let mut ranges = vec![(0, len), (0, 0), (0, 1), (len - 1, 1), (len, 0)];

    // Two bytes either side of each boundary, so the read spans two chunks.
    ranges.extend(boundaries.iter().map(|&boundary| (boundary - 1, 2)));

    let mut rng = XorShift::new(seed);
    for _ in 0..32 {
        let offset = rng.next_u64() % len;
        let length = rng.next_u64() % (len - offset + 1);
        ranges.push((offset, length));
    }
    ranges
}

/// Checks that seeking into each file through `get_seek_chunks` reads the
/// original bytes. `seed` must not be zero.
fn assert_seeks_match<H: TestHash>(
    files: &[FileData],
    reader: &Reader<H>,
    seed: u64,
) {
    // Xorshift never returns 0 from a nonzero seed, so every file's seed is
    // usable too.
    let mut seeds = XorShift::new(seed);
    let parsed = &reader.parsed;
    for (entry, manifest) in parsed.toc.iter().zip(&parsed.manifests) {
        let data = original(files, &entry.filename);
        let boundaries: Vec<u64> = manifest
            .chunk_metadata
            .iter()
            .skip(1)
            .map(|meta| meta.offset)
            .collect();

        let ranges =
            seek_ranges(data.len() as u64, &boundaries, seeds.next_u64());
        for (offset, length) in ranges {
            let mut read = Vec::new();
            for seek in get_seek_chunks(manifest, offset, length).unwrap() {
                let chunk = reader.chunk(&seek.hash);
                let start = usize::try_from(seek.start_offset).unwrap();
                let end = usize::try_from(seek.end_offset).unwrap();
                read.extend_from_slice(&chunk[start..end]);
            }

            let start = usize::try_from(offset).unwrap();
            let end = start + usize::try_from(length).unwrap();
            assert_bytes_eq(
                &read,
                &data[start..end],
                &format!("{}: {length} bytes at {offset}", entry.filename),
            );
        }
    }
}

const ROM_SET_SEEK_SEED: u64 = 0x5EED_1001;

/// File sets with too few distinct chunks for zstd to train a dictionary
/// on, so the archive is built without one.
fn small_sets() -> Vec<(&'static str, Vec<FileData>)> {
    let file = |name: &str, file_data: Vec<u8>| FileData {
        file_name: name.to_string(),
        file_data,
    };

    // Code followed by 0xFF fill, like a cartridge ROM with unused space.
    // The fill becomes a few identical chunks, so 16 KiB holds little
    // distinct data.
    let mut padded = tiled_bytes(0x5EED_2001, 1024);
    padded.resize(16 * 1024, 0xFF);

    let copy = tiled_bytes(0x5EED_2002, 100);

    vec![
        (
            "one 4 KiB file",
            vec![file("small.bin", tiled_bytes(0x5EED_2003, 4096))],
        ),
        ("one 1-byte file", vec![file("byte.bin", vec![0x42])]),
        ("a padded 16 KiB ROM", vec![file("padded.bin", padded)]),
        (
            "three identical files",
            vec![
                file("a.bin", copy.clone()),
                file("b.bin", copy.clone()),
                file("c.bin", copy),
            ],
        ),
    ]
}

fn assert_small_sets_round_trip<H: TestHash>(hash_type: u8) {
    for (label, files) in small_sets() {
        let (built, reader) = open::<H>(&files, hash_type);
        assert!(
            reader.parsed.dictionary.is_empty(),
            "{label}: a dictionary was trained"
        );
        assert_eq!(reader.parsed.format_data.data_dictionary.length, 0);

        assert_archive_lists_files(&files, &reader, hash_type);
        assert_files_extract(&files, &reader);
        assert_seeks_match(&files, &reader, ROM_SET_SEEK_SEED);
        assert_files_verify(&built, &Arc::new(reader));
    }
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_archive_describes_its_files() {
    assert_rom_set_described::<u64>(HASH_TYPE_XXH3_64);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_archive_describes_its_files() {
    assert_rom_set_described::<u128>(HASH_TYPE_XXH3_128);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_archive_extracts_original_files() {
    let files = rom_set_files();
    let (_built, reader) = open::<u64>(&files, HASH_TYPE_XXH3_64);
    assert_files_extract(&files, &reader);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_archive_extracts_original_files() {
    let files = rom_set_files();
    let (_built, reader) = open::<u128>(&files, HASH_TYPE_XXH3_128);
    assert_files_extract(&files, &reader);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_verification_accepts_only_each_files_hash() {
    assert_rom_set_verifies::<u64>(HASH_TYPE_XXH3_64);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_verification_accepts_only_each_files_hash() {
    assert_rom_set_verifies::<u128>(HASH_TYPE_XXH3_128);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_seeks_read_the_original_bytes() {
    let files = rom_set_files();
    let (_built, reader) = open::<u64>(&files, HASH_TYPE_XXH3_64);
    assert_seeks_match(&files, &reader, ROM_SET_SEEK_SEED);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_seeks_read_the_original_bytes() {
    let files = rom_set_files();
    let (_built, reader) = open::<u128>(&files, HASH_TYPE_XXH3_128);
    assert_seeks_match(&files, &reader, ROM_SET_SEEK_SEED);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_small_sets_round_trip_without_a_dictionary() {
    assert_small_sets_round_trip::<u64>(HASH_TYPE_XXH3_64);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_small_sets_round_trip_without_a_dictionary() {
    assert_small_sets_round_trip::<u128>(HASH_TYPE_XXH3_128);
}

/// The largest generated base file. Bases from one byte up cover archives
/// with too little data to train a dictionary as well as ones with enough.
const MAX_BASE_LEN: usize = 96 * 1024;

/// How one extra file in a generated set relates to the base file.
#[derive(Clone, Debug)]
enum Extra {
    /// The base, cut short if `keep` is set, with patches written over it
    /// and `tail_len` new bytes appended. Without a cut, patches or tail it
    /// is an exact copy.
    Variant {
        keep: Option<Index>,
        patches: Vec<Patch>,
        tail_len: usize,
        tail_seed: u64,
    },
    /// Data unrelated to the base.
    Fresh { len: usize, seed: u64 },
}

/// New bytes written over part of a file.
#[derive(Clone, Debug)]
struct Patch {
    at: Index,
    len: usize,
    seed: u64,
}

/// A generated file set, kept as the choices that produce it so a failing
/// case prints as a few numbers rather than kilobytes of data.
#[derive(Clone, Debug)]
struct SetSpec {
    base_len: usize,
    base_seed: u64,
    extras: Vec<Extra>,
}

impl SetSpec {
    fn files(&self) -> Vec<FileData> {
        let base = tiled_bytes(self.base_seed, self.base_len);
        let extras = self.extras.iter().map(|extra| match extra {
            Extra::Variant { keep, patches, tail_len, tail_seed } => {
                let mut data = base.clone();
                if let Some(keep) = keep {
                    data.truncate(keep.index(base.len()) + 1);
                }
                for patch in patches {
                    let start = patch.at.index(data.len());
                    let end = (start + patch.len).min(data.len());
                    data.splice(
                        start..end,
                        tiled_bytes(patch.seed, end - start),
                    );
                }
                data.extend(tiled_bytes(*tail_seed, *tail_len));
                data
            }
            Extra::Fresh { len, seed } => tiled_bytes(*seed, *len),
        });

        std::iter::once(base.clone())
            .chain(extras)
            .enumerate()
            .map(|(index, file_data)| FileData {
                file_name: format!("file{index}.bin"),
                file_data,
            })
            .collect()
    }
}

/// Xorshift seeds, which must not be zero.
fn seed() -> impl Strategy<Value = u64> {
    1..=u64::MAX
}

fn patch() -> impl Strategy<Value = Patch> {
    (any::<Index>(), 1..=2048_usize, seed())
        .prop_map(|(at, len, seed)| Patch { at, len, seed })
}

fn extra() -> impl Strategy<Value = Extra> {
    prop_oneof![
        (
            proptest::option::of(any::<Index>()),
            proptest::collection::vec(patch(), 0..4),
            0..=8192_usize,
            seed(),
        )
            .prop_map(|(keep, patches, tail_len, tail_seed)| {
                Extra::Variant { keep, patches, tail_len, tail_seed }
            }),
        (1..=8192_usize, seed())
            .prop_map(|(len, seed)| Extra::Fresh { len, seed }),
    ]
}

fn set_spec() -> impl Strategy<Value = SetSpec> {
    (
        1..=MAX_BASE_LEN,
        seed(),
        proptest::collection::vec(extra(), 0..=5),
    )
        .prop_map(|(base_len, base_seed, extras)| {
            SetSpec { base_len, base_seed, extras }
        })
}

/// Runs every round-trip check on the file set `spec` generates.
fn assert_round_trips<H: TestHash>(spec: &SetSpec, hash_type: u8, seed: u64) {
    let files = spec.files();
    let (built, reader) = open::<H>(&files, hash_type);
    assert_archive_lists_files(&files, &reader, hash_type);
    assert_files_extract(&files, &reader);
    assert_seeks_match(&files, &reader, seed);
    assert_files_verify(&built, &Arc::new(reader));
}

/// 64 cases by default: each one builds and reads a whole archive, and
/// proptest's usual 256 take several seconds in a debug build.
/// `PROPTEST_CASES` still overrides it, for a longer run.
fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(64);
    ProptestConfig { cases, ..ProptestConfig::default() }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    #[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
    fn u64_generated_sets_round_trip(spec in set_spec(), seed in seed()) {
        assert_round_trips::<u64>(&spec, HASH_TYPE_XXH3_64, seed);
    }

    #[test]
    #[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
    fn u128_generated_sets_round_trip(spec in set_spec(), seed in seed()) {
        assert_round_trips::<u128>(&spec, HASH_TYPE_XXH3_128, seed);
    }
}
