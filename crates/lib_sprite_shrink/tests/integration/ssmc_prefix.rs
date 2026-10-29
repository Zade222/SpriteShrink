//! Checks that `build_ssmc_prefix` lays out an archive's metadata where the
//! header and format data inside the prefix say it is.
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
