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
use sprite_shrink_test_support::{XorShift, rom_set};

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
    let keys: Vec<H> = store.keys().copied().collect();
    // The callbacks must be `'static`, so they share the store through an
    // `Arc` rather than borrowing it.
    let store = Arc::new(store);
    let get_chunks = move |hashes: &[H]| {
        Ok::<_, SpriteShrinkError>(
            hashes.iter().map(|hash| store[hash].clone()).collect()
        )
    };

    let ser = serialize_uncompressed_data(
        &file_manifest,
        &move || Ok::<_, SpriteShrinkError>(keys.clone()),
        &get_chunks.clone(),
    )
    .unwrap();

    // The write callback must be `'static` too, so the test keeps a second
    // handle to the buffer to read it back after `build`.
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let mut builder = ArchiveBuilder::new(
        &ser.sorted_hashes,
        total_size,
        get_chunks,
        move |data: &[u8], _flush: bool| {
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

fn open_rom_set<H: TestHash>(
    hash_type: u8
) -> (Vec<FileData>, Built, Reader<H>) {
    let files: Vec<FileData> = rom_set()
        .into_iter()
        .map(|(file_name, file_data)| FileData { file_name, file_data })
        .collect();
    let built = build_archive::<H>(&files, hash_type);
    let reader = Reader::open(built.archive.clone());
    (files, built, reader)
}

fn assert_archive_describes_files<H: TestHash>(hash_type: u8) {
    let (files, built, reader) = open_rom_set::<H>(hash_type);
    let parsed = &reader.parsed;

    assert_eq!(parsed.header.hash_type, hash_type);
    assert_eq!(usize::try_from(parsed.header.file_count).unwrap(), files.len());
    assert_eq!(parsed.toc.len(), files.len());
    assert_eq!(parsed.manifests.len(), files.len());

    for (entry, manifest) in parsed.toc.iter().zip(&parsed.manifests) {
        let data = original(&files, &entry.filename);
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

    // The patched files share most chunks with the base, so the archive
    // stores fewer chunks than the manifests reference.
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
}

fn assert_files_extract<H: TestHash>(hash_type: u8) {
    let (files, _built, reader) = open_rom_set::<H>(hash_type);

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
        assert!(
            rebuilt == original(&files, &entry.filename),
            "{} differs from the original",
            entry.filename
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

fn assert_files_verify<H: TestHash>(hash_type: u8) {
    let (_files, built, reader) = open_rom_set::<H>(hash_type);
    // `verify_single_file` needs a `'static` chunk callback.
    let reader = Arc::new(reader);
    let toc = &reader.parsed.toc;

    for (index, entry) in toc.iter().enumerate() {
        verify(&reader, index, &built.veri_hashes[&entry.filename]).unwrap();

        // The next file's hash must not match, or a check that always
        // passes would go unnoticed.
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

fn assert_seeks_match<H: TestHash>(hash_type: u8) {
    let (files, _built, reader) = open_rom_set::<H>(hash_type);

    // One seed per file. Xorshift maps 0 to 0, so the seeds start above it.
    for (seed, (entry, manifest)) in (0x5EED_1001..)
        .zip(reader.parsed.toc.iter().zip(&reader.parsed.manifests))
    {
        let data = original(&files, &entry.filename);
        let boundaries: Vec<u64> = manifest
            .chunk_metadata
            .iter()
            .skip(1)
            .map(|meta| meta.offset)
            .collect();

        let ranges = seek_ranges(data.len() as u64, &boundaries, seed);
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
            assert!(
                read == data[start..end],
                "{}: {length} bytes at {offset} differ",
                entry.filename
            );
        }
    }
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_archive_describes_its_files() {
    assert_archive_describes_files::<u64>(HASH_TYPE_XXH3_64);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_archive_describes_its_files() {
    assert_archive_describes_files::<u128>(HASH_TYPE_XXH3_128);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_archive_extracts_original_files() {
    assert_files_extract::<u64>(HASH_TYPE_XXH3_64);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_archive_extracts_original_files() {
    assert_files_extract::<u128>(HASH_TYPE_XXH3_128);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_verification_accepts_only_each_files_hash() {
    assert_files_verify::<u64>(HASH_TYPE_XXH3_64);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_verification_accepts_only_each_files_hash() {
    assert_files_verify::<u128>(HASH_TYPE_XXH3_128);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u64_seeks_read_the_original_bytes() {
    assert_seeks_match::<u64>(HASH_TYPE_XXH3_64);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn u128_seeks_read_the_original_bytes() {
    assert_seeks_match::<u128>(HASH_TYPE_XXH3_128);
}
