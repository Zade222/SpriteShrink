//! Checks that `build_ssmc_prefix` lays out an archive's metadata where the
//! header and format data inside the prefix say it is, and that
//! `parse_ssmc_prefix` reads it back.
//!
//! A reader finds every section only through those offsets, so a prefix
//! whose sections don't match them is unreadable even if every section is
//! encoded correctly.

use bitcode::{Encode, encode};
use sprite_shrink::{
    ALGORITHM_ZSTD, ChunkLocation, CompressionResult, FileHeader,
    FileManifestParent, SSAChunkMeta, SSMC_UID, SSMCFormatData, SSMCTocEntry,
    SpriteShrinkError, build_ssmc_prefix, parse_file_chunk_index,
    parse_file_header, parse_file_metadata, parse_file_toc, parse_format_data,
    parse_ssmc_prefix,
};

fn sample_toc() -> Vec<SSMCTocEntry> {
    vec![
        SSMCTocEntry { filename: "game (Rev 0).bin".to_string(), uncompressed_size: 4196 },
        SSMCTocEntry { filename: "game (Rev 1).bin".to_string(), uncompressed_size: 8192 },
    ]
}

fn sample_manifests<H: Copy>(hashes: [H; 3]) -> Vec<FileManifestParent<H>> {
    vec![
        FileManifestParent {
            chunk_count: 2,
            chunk_metadata: vec![
                SSAChunkMeta { hash: hashes[0], offset: 0, length: 4096 },
                SSAChunkMeta { hash: hashes[1], offset: 4096, length: 100 },
            ],
        },
        FileManifestParent {
            chunk_count: 2,
            chunk_metadata: vec![
                SSAChunkMeta { hash: hashes[0], offset: 0, length: 4096 },
                SSAChunkMeta { hash: hashes[2], offset: 4096, length: 4096 },
            ],
        },
    ]
}

/// A dictionary of arbitrary bytes and a real encoded chunk index, so the
/// index section can be decoded back.
fn sample_comp<H: Copy + Encode>(hashes: [H; 3]) -> CompressionResult {
    let dictionary: Vec<u8> = (0..=255_u8).cycle().take(1000).collect();
    let chunk_index: Vec<(H, ChunkLocation)> = hashes
        .iter()
        .zip([0_u64, 900, 940])
        .map(|(&hash, offset)| {
            (hash, ChunkLocation { offset, compressed_length: 40 })
        })
        .collect();
    let enc_chunk_index = encode(&chunk_index);

    CompressionResult {
        dictionary_size: dictionary.len() as u64,
        dictionary,
        enc_chunk_index_size: enc_chunk_index.len() as u64,
        enc_chunk_index,
    }
}

fn empty_comp() -> CompressionResult {
    let enc_chunk_index = encode(&Vec::<(u64, ChunkLocation)>::new());
    CompressionResult {
        dictionary: Vec::new(),
        dictionary_size: 0,
        enc_chunk_index_size: enc_chunk_index.len() as u64,
        enc_chunk_index,
    }
}

/// Reads the header and format data back out of a prefix, using only the
/// offsets recorded in it.
fn read_layout(prefix: &[u8]) -> (FileHeader, SSMCFormatData) {
    let header = parse_file_header(&prefix[..36]).unwrap();
    let start = header.format_data_offset as usize;
    let format_data = parse_format_data(&prefix[start..start + 56]).unwrap();
    (header, format_data)
}

fn section(prefix: &[u8], offset: u64, length: u64) -> &[u8] {
    let start = usize::try_from(offset).unwrap();
    &prefix[start..start + usize::try_from(length).unwrap()]
}

#[test]
fn build_ssmc_prefix_length_equals_metadata_prefix_end() {
    let cases = [
        (sample_toc(), sample_manifests([1_u64, 2, 3]), sample_comp([1_u64, 2, 3])),
        (Vec::new(), Vec::new(), empty_comp()),
    ];

    for (toc, manifests, comp) in cases {
        let prefix = build_ssmc_prefix(&toc, &manifests, &comp, 1).unwrap();
        let (_, format_data) = read_layout(&prefix);

        assert_eq!(prefix.len() as u64, format_data.metadata_prefix_end());
    }
}

#[test]
fn build_ssmc_prefix_header_describes_the_archive() {
    let toc = sample_toc();
    let manifests = sample_manifests([1_u64, 2, 3]);
    let comp = sample_comp([1_u64, 2, 3]);

    let prefix = build_ssmc_prefix(&toc, &manifests, &comp, 1).unwrap();
    let (header, _) = read_layout(&prefix);

    assert_eq!(header.file_count, 2);
    assert_eq!(header.algorithm, ALGORITHM_ZSTD);
    assert_eq!(header.hash_type, 1);
    assert_eq!(header.format_id, SSMC_UID);
    assert_eq!(header.enc_toc_offset, 36);
    assert_eq!(header.enc_toc_length as usize, encode(&toc).len());
}

/// Every section's bytes sit at the offset the header or format data
/// records for it, and decode back to the input.
fn assert_sections_at_recorded_offsets<H>(hashes: [H; 3], hash_type: u8)
where
    H: Copy + Encode + Eq + std::hash::Hash + std::fmt::Debug,
    for<'de> H: serde::Deserialize<'de> + bitcode::Decode<'de>,
{
    let toc = sample_toc();
    let manifests = sample_manifests(hashes);
    let comp = sample_comp(hashes);

    let prefix = build_ssmc_prefix(&toc, &manifests, &comp, hash_type).unwrap();
    let (header, format_data) = read_layout(&prefix);

    let toc_bytes = section(
        &prefix,
        header.enc_toc_offset.into(),
        header.enc_toc_length.into(),
    );
    assert_eq!(toc_bytes, encode(&toc));
    assert_eq!(parse_file_toc(toc_bytes).unwrap(), toc);

    let region = format_data.enc_manifest;
    let manifest_bytes = section(&prefix, region.offset, region.length);
    assert_eq!(manifest_bytes, encode(&manifests));
    let parsed = parse_file_metadata::<H>(manifest_bytes).unwrap();
    assert_eq!(parsed.len(), manifests.len());
    assert_eq!(parsed[1].chunk_metadata[1].hash, hashes[2]);

    let region = format_data.data_dictionary;
    assert_eq!(section(&prefix, region.offset, region.length), comp.dictionary);

    let region = format_data.enc_chunk_index;
    let index_bytes = section(&prefix, region.offset, region.length);
    assert_eq!(index_bytes, comp.enc_chunk_index);
    let index = parse_file_chunk_index::<H>(index_bytes).unwrap();
    assert_eq!(index.len(), 3);
    assert_eq!(index[&hashes[2]].offset, 940);
}

#[test]
fn build_ssmc_prefix_u64_sections_sit_at_recorded_offsets() {
    assert_sections_at_recorded_offsets([1_u64, u64::MAX, 7], 1);
}

#[test]
fn build_ssmc_prefix_u128_sections_sit_at_recorded_offsets() {
    assert_sections_at_recorded_offsets([1_u128, u128::MAX, 7], 2);
}

#[test]
fn build_file_header_format_data_offset_overflow_returns_error() {
    let largest = usize::try_from(u32::MAX - FileHeader::HEADER_SIZE).unwrap();

    let header = FileHeader::build_file_header(1, ALGORITHM_ZSTD, 1, SSMC_UID, largest)
        .unwrap();
    assert_eq!(header.format_data_offset, u32::MAX);

    let result = FileHeader::build_file_header(
        1, ALGORITHM_ZSTD, 1, SSMC_UID, largest + 1,
    );
    assert!(
        matches!(result, Err(SpriteShrinkError::Serialization(_))),
        "{result:?}"
    );
}

// Lengths past `u32::MAX` only exist where `usize` is wider than 32 bits.
#[cfg(target_pointer_width = "64")]
#[test]
fn build_file_header_counts_past_u32_max_return_error() {
    let too_many = usize::try_from(u32::MAX).unwrap() + 1;

    for (file_count, toc_length) in [(too_many, 40), (1, too_many)] {
        let result = FileHeader::build_file_header(
            file_count, ALGORITHM_ZSTD, 1, SSMC_UID, toc_length,
        );

        assert!(
            matches!(result, Err(SpriteShrinkError::Serialization(_))),
            "{file_count} files, {toc_length}-byte TOC: {result:?}"
        );
    }
}

/// Returns `prefix` with its header rewritten by `edit`.
fn with_header(prefix: &[u8], edit: impl FnOnce(&mut FileHeader)) -> Vec<u8> {
    let (mut header, _) = read_layout(prefix);
    edit(&mut header);

    let mut edited = prefix.to_vec();
    edited[..36].copy_from_slice(&header.to_le_bytes());
    edited
}

/// Returns `prefix` with its format data rewritten by `edit`.
fn with_format_data(
    prefix: &[u8],
    edit: impl FnOnce(&mut SSMCFormatData),
) -> Vec<u8> {
    let (header, mut format_data) = read_layout(prefix);
    edit(&mut format_data);

    let start = header.format_data_offset as usize;
    let mut edited = prefix.to_vec();
    edited[start..start + 56].copy_from_slice(&format_data.to_le_bytes());
    edited
}

fn assert_parse_error(prefix: &[u8], context: &str) {
    let result = parse_ssmc_prefix::<u64>(prefix);

    assert!(
        matches!(result, Err(SpriteShrinkError::Parsing(_))),
        "{context}: {result:?}"
    );
}

fn assert_parse_round_trips<H>(hashes: [H; 3], hash_type: u8)
where
    H: Copy + Encode + Eq + std::hash::Hash + std::fmt::Debug,
    for<'de> H: serde::Deserialize<'de> + bitcode::Decode<'de>,
{
    let toc = sample_toc();
    let manifests = sample_manifests(hashes);
    let comp = sample_comp(hashes);
    let prefix = build_ssmc_prefix(&toc, &manifests, &comp, hash_type).unwrap();
    let (header, format_data) = read_layout(&prefix);

    let parsed = parse_ssmc_prefix::<H>(&prefix).unwrap();

    assert_eq!(parsed.header, header);
    assert_eq!(parsed.format_data, format_data);
    assert_eq!(parsed.toc, toc);
    assert_eq!(encode(&parsed.manifests), encode(&manifests));
    assert_eq!(parsed.dictionary, comp.dictionary);

    // sample_comp stores the chunks at offsets 0, 900 and 940.
    assert_eq!(parsed.chunk_index.len(), 3);
    for (hash, stored_offset) in hashes.iter().zip([0_u64, 900, 940]) {
        let location = parsed.chunk_index[hash];
        assert_eq!(location.offset, format_data.data_offset + stored_offset);
        assert_eq!(location.compressed_length, 40);
    }
}

#[test]
fn parse_ssmc_prefix_u64_round_trips_build_ssmc_prefix() {
    assert_parse_round_trips([1_u64, u64::MAX, 7], 1);
}

#[test]
fn parse_ssmc_prefix_u128_round_trips_build_ssmc_prefix() {
    assert_parse_round_trips([1_u128, u128::MAX, 7], 2);
}

#[test]
fn parse_ssmc_prefix_empty_archive_round_trips() {
    let prefix = build_ssmc_prefix::<u64>(&[], &[], &empty_comp(), 1).unwrap();

    let parsed = parse_ssmc_prefix::<u64>(&prefix).unwrap();

    assert_eq!(parsed.toc, Vec::new());
    assert!(parsed.manifests.is_empty());
    assert_eq!(parsed.dictionary, Vec::<u8>::new());
    assert!(parsed.chunk_index.is_empty());
}

// A host may read more than the prefix, up to the whole archive.
#[test]
fn parse_ssmc_prefix_ignores_bytes_past_the_prefix() {
    let comp = sample_comp([1_u64, 2, 3]);
    let prefix = build_ssmc_prefix(
        &sample_toc(), &sample_manifests([1_u64, 2, 3]), &comp, 1,
    ).unwrap();
    let mut archive = prefix.clone();
    archive.extend_from_slice(&[0xEE; 980]);

    let from_prefix = parse_ssmc_prefix::<u64>(&prefix).unwrap();
    let from_archive = parse_ssmc_prefix::<u64>(&archive).unwrap();

    assert_eq!(from_archive.toc, from_prefix.toc);
    assert_eq!(from_archive.dictionary, from_prefix.dictionary);
    assert_eq!(
        encode(&from_archive.manifests),
        encode(&from_prefix.manifests)
    );
    assert_eq!(from_archive.chunk_index.len(), from_prefix.chunk_index.len());
}

#[test]
fn parse_ssmc_prefix_truncated_prefix_returns_error() {
    let prefix = build_ssmc_prefix(
        &sample_toc(),
        &sample_manifests([1_u64, 2, 3]),
        &sample_comp([1_u64, 2, 3]),
        1,
    ).unwrap();

    for len in 0..prefix.len() {
        assert_parse_error(&prefix[..len], &format!("{len} bytes"));
    }
}

#[test]
fn parse_ssmc_prefix_section_past_the_end_returns_error() {
    let prefix = build_ssmc_prefix(
        &sample_toc(),
        &sample_manifests([1_u64, 2, 3]),
        &sample_comp([1_u64, 2, 3]),
        1,
    ).unwrap();

    let longer_index = with_format_data(&prefix, |format_data| {
        format_data.enc_chunk_index.length += 1;
    });
    assert_parse_error(&longer_index, "chunk index one byte too long");

    // The dictionary is raw bytes, so only the bounds check can catch this.
    let longer_dictionary = with_format_data(&prefix, |format_data| {
        format_data.data_dictionary.length = 1 << 40;
    });
    assert_parse_error(&longer_dictionary, "dictionary past the end");

    let wrapping_dictionary = with_format_data(&prefix, |format_data| {
        format_data.data_dictionary.offset = u64::MAX;
    });
    assert_parse_error(&wrapping_dictionary, "dictionary end overflows");

    let longer_toc = with_header(&prefix, |header| {
        header.enc_toc_length = u32::MAX;
    });
    assert_parse_error(&longer_toc, "TOC past the end");
}

#[test]
fn parse_ssmc_prefix_other_format_id_returns_error() {
    let prefix = build_ssmc_prefix(
        &sample_toc(),
        &sample_manifests([1_u64, 2, 3]),
        &sample_comp([1_u64, 2, 3]),
        1,
    ).unwrap();

    let other_format = with_header(&prefix, |header| {
        header.format_id = SSMC_UID + 1;
    });

    assert_parse_error(&other_format, "format ID");
}

#[test]
fn parse_ssmc_prefix_chunk_offset_overflow_returns_error() {
    let prefix = build_ssmc_prefix(
        &sample_toc(),
        &sample_manifests([1_u64, 2, 3]),
        &sample_comp([1_u64, 2, 3]),
        1,
    ).unwrap();

    // The chunk stored at offset 940 ends up past u64::MAX.
    let near_max = with_format_data(&prefix, |format_data| {
        format_data.data_offset = u64::MAX - 100;
    });

    assert_parse_error(&near_max, "chunk offset overflow");
}
