//! Checks that `build_ssmd_prefix` lays out an archive's metadata where the
//! header and format data inside the prefix say it is, and that
//! `parse_ssmd_prefix` and the older `decode_ssmd_meta_data` read it back.

use bitcode::{Encode, encode};
use sprite_shrink::{
    ALGORITHM_ZSTD, 
    ChunkLocation, CompressionResult, FileHeader,
    parse_file_header,
};
use sprite_shrink_cd::{
    SSMD_UID, 
    ContentBlock, DataChunkLayout, DiscManifest, RleSectorMap,
    SSMDFormatData, SSMDPrefix, SSMDTocEntry, SectorType, SpriteShrinkCDError,
    SubHeaderEntry, 
    build_ssmd_prefix, decode_ssmd_format_data,
    decode_ssmd_meta_data, parse_ssmd_prefix,
};

/// The inputs for a two-disc archive. Owned here so a test can borrow them
/// as `SSMDPrefix`.
struct Inputs<H> {
    toc: Vec<SSMDTocEntry>,
    disc_manifests: Vec<DiscManifest<H>>,
    comp: CompressionResult,
    audio_block_index: Vec<(H, ChunkLocation)>,
    exception_index: Vec<u64>,
    subheader_table: Vec<[u8; 8]>,
    exception_blob: Vec<u8>,
    audio_blob_size: u64,
}

impl<H> Inputs<H> {
    fn parts(&self, hash_type: u8) -> SSMDPrefix<'_, H> {
        SSMDPrefix {
            toc: &self.toc,
            disc_manifests: &self.disc_manifests,
            comp: &self.comp,
            audio_block_index: &self.audio_block_index,
            exception_index: &self.exception_index,
            subheader_table: &self.subheader_table,
            exception_blob: &self.exception_blob,
            audio_blob_size: self.audio_blob_size,
            hash_type,
        }
    }
}

fn manifest<H: Copy>(data_hash: H, audio_hash: H) -> DiscManifest<H> {
    DiscManifest {
        lba_map: vec![(0, 300), (300, 75)],
        rle_sector_map: RleSectorMap {
            runs: vec![(300, SectorType::Mode2Form1), (75, SectorType::Audio)],
        },
        audio_block_map: vec![ContentBlock {
            start_sector: 300,
            sector_count: 75,
            content_hash: audio_hash,
            sector_type: SectorType::Audio,
        }],
        data_stream_layout: vec![DataChunkLayout { hash: data_hash, uncomp_len: 4096 }],
        subheader_index: vec![SubHeaderEntry { start_lba: 0, count: 300, data_id: 0 }],
        disc_exception_index: vec![(12, 0)],
        integrity_hash: 0x1234_5678,
    }
}

/// Two mixed-mode discs, so the audio index is not empty and the audio
/// blob sits between the metadata and the data chunks.
fn inputs_with_audio<H: Copy + Encode>(hashes: [H; 3]) -> Inputs<H> {
    let chunk_index = vec![
        (hashes[0], ChunkLocation { offset: 0, compressed_length: 40 }),
        (hashes[1], ChunkLocation { offset: 40, compressed_length: 60 }),
    ];
    let enc_chunk_index = encode(&chunk_index);
    let dictionary: Vec<u8> = (0..=255_u8).cycle().take(1000).collect();

    Inputs {
        toc: vec![
            SSMDTocEntry {
                filename: "disc 1.bin".to_string(),
                collection_id: 0,
                uncompressed_size: 375 * 2352,
            },
            SSMDTocEntry {
                filename: "disc 2.bin".to_string(),
                collection_id: 1,
                uncompressed_size: 375 * 2352,
            },
        ],
        disc_manifests: vec![
            manifest(hashes[0], hashes[2]),
            manifest(hashes[1], hashes[2]),
        ],
        comp: CompressionResult {
            dictionary_size: dictionary.len() as u64,
            dictionary,
            enc_chunk_index_size: enc_chunk_index.len() as u64,
            enc_chunk_index,
        },
        audio_block_index: vec![
            (hashes[2], ChunkLocation { offset: 0, compressed_length: 3000 }),
        ],
        exception_index: vec![0, 296],
        subheader_table: vec![
            [0, 0, 0x08, 0, 0, 0, 0x08, 0],
            [0, 0, 0x20, 0, 0, 0, 0x20, 0],
        ],
        exception_blob: vec![0xAB; 2 * 296],
        audio_blob_size: 3000,
    }
}

/// The same discs without audio: no audio index and no audio blob.
fn inputs_without_audio<H: Copy + Encode>(hashes: [H; 3]) -> Inputs<H> {
    Inputs {
        audio_block_index: Vec::new(),
        audio_blob_size: 0,
        ..inputs_with_audio(hashes)
    }
}

/// Reads the header and format data back out of a prefix, using only the
/// offsets recorded in it.
fn read_layout(prefix: &[u8]) -> (FileHeader, SSMDFormatData) {
    let header = parse_file_header(&prefix[..36]).unwrap();
    let start = header.format_data_offset as usize;
    let format_data = decode_ssmd_format_data(&prefix[start..start + 136]).unwrap();
    (header, format_data)
}

fn section(prefix: &[u8], offset: u64, length: u64) -> &[u8] {
    let start = usize::try_from(offset).unwrap();
    &prefix[start..start + usize::try_from(length).unwrap()]
}

#[test]
fn build_ssmd_prefix_length_equals_metadata_prefix_end() {
    for inputs in [inputs_with_audio([1_u64, 2, 3]), inputs_without_audio([1_u64, 2, 3])] {
        let prefix = build_ssmd_prefix(&inputs.parts(1)).unwrap();
        let (_, format_data) = read_layout(&prefix);

        assert_eq!(prefix.len() as u64, format_data.metadata_prefix_end());
        assert_eq!(
            format_data.data_offset,
            format_data.metadata_prefix_end() + inputs.audio_blob_size
        );
    }
}

#[test]
fn build_ssmd_prefix_with_audio_ends_before_data_offset() {
    let inputs = inputs_with_audio([1_u64, 2, 3]);

    let prefix = build_ssmd_prefix(&inputs.parts(1)).unwrap();
    let (_, format_data) = read_layout(&prefix);

    assert_eq!(format_data.audio_data.length, 3000);
    assert!(format_data.metadata_prefix_end() < format_data.data_offset);
}

#[test]
fn build_ssmd_prefix_header_describes_the_archive() {
    let inputs = inputs_with_audio([1_u64, 2, 3]);

    let prefix = build_ssmd_prefix(&inputs.parts(1)).unwrap();
    let (header, _) = read_layout(&prefix);

    assert_eq!(header.file_count, 2);
    assert_eq!(header.algorithm, ALGORITHM_ZSTD);
    assert_eq!(header.hash_type, 1);
    assert_eq!(header.format_id, SSMD_UID);
    assert_eq!(header.enc_toc_offset, 36);
    assert_eq!(header.enc_toc_length as usize, encode(&inputs.toc).len());
}

#[test]
fn build_ssmd_prefix_empty_audio_index_is_omitted() {
    let inputs = inputs_without_audio([1_u64, 2, 3]);

    let prefix = build_ssmd_prefix(&inputs.parts(1)).unwrap();
    let (_, format_data) = read_layout(&prefix);

    assert_eq!(format_data.enc_audio_index.length, 0);
    assert_eq!(
        format_data.enc_audio_index.offset,
        format_data.enc_excep_index.offset
    );
}

/// Every section's bytes sit at the offset the header or format data
/// records for it.
fn assert_sections_at_recorded_offsets<H: Copy + Encode>(
    hashes: [H; 3],
    hash_type: u8,
) {
    let inputs = inputs_with_audio(hashes);

    let prefix = build_ssmd_prefix(&inputs.parts(hash_type)).unwrap();
    let (header, fd) = read_layout(&prefix);

    let sections = [
        (
            "toc",
            u64::from(header.enc_toc_offset),
            u64::from(header.enc_toc_length),
            encode(&inputs.toc),
        ),
        ("disc manifest", fd.enc_disc_manifest.offset, fd.enc_disc_manifest.length,
            encode(&inputs.disc_manifests)),
        ("dictionary", fd.data_dictionary.offset, fd.data_dictionary.length,
            inputs.comp.dictionary.clone()),
        ("chunk index", fd.enc_chunk_index.offset, fd.enc_chunk_index.length,
            inputs.comp.enc_chunk_index.clone()),
        ("audio index", fd.enc_audio_index.offset, fd.enc_audio_index.length,
            encode(&inputs.audio_block_index)),
        ("exception index", fd.enc_excep_index.offset, fd.enc_excep_index.length,
            encode(&inputs.exception_index)),
        ("subheader table", fd.subheader_table.offset, fd.subheader_table.length,
            inputs.subheader_table.as_flattened().to_vec()),
        ("exception data", fd.exception_data.offset, fd.exception_data.length,
            inputs.exception_blob.clone()),
    ];

    for (name, offset, length, expected) in sections {
        assert!(
            section(&prefix, offset, length) == expected.as_slice(),
            "{name} at offset {offset} doesn't match"
        );
    }
}

#[test]
fn build_ssmd_prefix_u64_sections_sit_at_recorded_offsets() {
    assert_sections_at_recorded_offsets([1_u64, u64::MAX, 7], 1);
}

#[test]
fn build_ssmd_prefix_u128_sections_sit_at_recorded_offsets() {
    assert_sections_at_recorded_offsets([1_u128, u128::MAX, 7], 2);
}

#[test]
fn build_ssmd_prefix_decodes_with_decode_ssmd_meta_data() {
    let inputs = inputs_with_audio([1_u64, 2, 3]);

    let prefix = build_ssmd_prefix(&inputs.parts(1)).unwrap();
    let (_, fd) = read_layout(&prefix);

    // decode_ssmd_meta_data reads the subheader table to the end of the
    // slice it's given, so the slice stops where the exception data starts,
    // as it does in the CLI's extractor.
    let metadata = section(
        &prefix,
        fd.enc_disc_manifest.offset,
        fd.exception_data.offset - fd.enc_disc_manifest.offset,
    );
    let decoded = decode_ssmd_meta_data::<u64>(&fd, metadata).unwrap();

    assert_eq!(decoded.disc_manifests, inputs.disc_manifests);
    assert_eq!(decoded.data_dictionary, inputs.comp.dictionary);
    assert_eq!(decoded.subheader_table, inputs.subheader_table);
    assert_eq!(decoded.exception_index, Some(inputs.exception_index.clone()));

    // The reader turns stored offsets into absolute file positions.
    assert_eq!(decoded.chunk_index[&2].offset, fd.data_offset + 40);
    let audio_index = decoded.audio_block_index.unwrap();
    assert_eq!(audio_index[&3].offset, fd.audio_data.offset);
    assert_eq!(audio_index[&3].compressed_length, 3000);
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
    edit: impl FnOnce(&mut SSMDFormatData),
) -> Vec<u8> {
    let (header, mut format_data) = read_layout(prefix);
    edit(&mut format_data);

    let start = header.format_data_offset as usize;
    let mut edited = prefix.to_vec();
    edited[start..start + 136].copy_from_slice(&format_data.to_le_bytes());
    edited
}

fn assert_parse_error(prefix: &[u8], context: &str) {
    let result = parse_ssmd_prefix::<u64>(prefix);

    assert!(
        matches!(
            result,
            Err(SpriteShrinkCDError::Decode(_) | SpriteShrinkCDError::SpriteShrink(_))
        ),
        "{context}: {result:?}"
    );
}

fn sample_prefix() -> Vec<u8> {
    build_ssmd_prefix(&inputs_with_audio([1_u64, 2, 3]).parts(1)).unwrap()
}

fn assert_parse_round_trips<H>(hashes: [H; 3], hash_type: u8)
where
    H: Copy + Encode + Eq + std::hash::Hash + std::fmt::Debug,
    for<'de> H: bitcode::Decode<'de>,
{
    let inputs = inputs_with_audio(hashes);
    let prefix = build_ssmd_prefix(&inputs.parts(hash_type)).unwrap();
    let (header, fd) = read_layout(&prefix);

    let parsed = parse_ssmd_prefix::<H>(&prefix).unwrap();

    assert_eq!(parsed.header, header);
    assert_eq!(parsed.format_data, fd);
    assert_eq!(parsed.toc, inputs.toc);
    assert_eq!(parsed.disc_manifests, inputs.disc_manifests);
    assert_eq!(parsed.data_dictionary, inputs.comp.dictionary);
    assert_eq!(parsed.indices.exception_index, inputs.exception_index);
    assert_eq!(parsed.exception_blob, inputs.exception_blob);

    // The exception data directly follows the subheader table, so a table
    // read past its recorded length would pick up those bytes as entries.
    assert_eq!(parsed.subheader_table, inputs.subheader_table);

    // Chunk offsets become positions in the file: inputs_with_audio stores
    // the data chunks at 0 and 40 and the audio block at 0.
    let chunk_index = &parsed.indices.chunk_index;
    assert_eq!(chunk_index.len(), 2);
    assert_eq!(chunk_index[&hashes[0]].offset, fd.data_offset);
    assert_eq!(chunk_index[&hashes[1]].offset, fd.data_offset + 40);
    assert_eq!(chunk_index[&hashes[1]].compressed_length, 60);

    let audio_index = &parsed.indices.audio_block_index;
    assert_eq!(audio_index.len(), 1);
    assert_eq!(audio_index[&hashes[2]].offset, fd.audio_data.offset);
    assert_eq!(audio_index[&hashes[2]].compressed_length, 3000);
}

#[test]
fn parse_ssmd_prefix_u64_round_trips_build_ssmd_prefix() {
    assert_parse_round_trips([1_u64, u64::MAX, 7], 1);
}

#[test]
fn parse_ssmd_prefix_u128_round_trips_build_ssmd_prefix() {
    assert_parse_round_trips([1_u128, u128::MAX, 7], 2);
}

#[test]
fn parse_ssmd_prefix_without_audio_has_empty_audio_index() {
    let inputs = inputs_without_audio([1_u64, 2, 3]);
    let prefix = build_ssmd_prefix(&inputs.parts(1)).unwrap();

    let parsed = parse_ssmd_prefix::<u64>(&prefix).unwrap();

    assert!(parsed.indices.audio_block_index.is_empty());
    assert_eq!(parsed.indices.chunk_index.len(), 2);
    assert_eq!(parsed.subheader_table, inputs.subheader_table);
}

// A host may read more than the prefix, up to the whole archive.
#[test]
fn parse_ssmd_prefix_ignores_bytes_past_the_prefix() {
    let prefix = sample_prefix();
    let mut archive = prefix.clone();
    archive.extend_from_slice(&[0xEE; 3100]);

    let from_prefix = parse_ssmd_prefix::<u64>(&prefix).unwrap();
    let from_archive = parse_ssmd_prefix::<u64>(&archive).unwrap();

    assert_eq!(from_archive.disc_manifests, from_prefix.disc_manifests);
    assert_eq!(from_archive.subheader_table, from_prefix.subheader_table);
    assert_eq!(from_archive.exception_blob, from_prefix.exception_blob);
}

#[test]
fn parse_ssmd_prefix_truncated_prefix_returns_error() {
    let prefix = sample_prefix();

    for len in 0..prefix.len() {
        assert_parse_error(&prefix[..len], &format!("{len} bytes"));
    }
}

#[test]
fn parse_ssmd_prefix_section_past_the_end_returns_error() {
    let prefix = sample_prefix();

    // These sections are raw bytes, so only the bounds check can catch them.
    let longer_dictionary = with_format_data(&prefix, |fd| {
        fd.data_dictionary.length = 1 << 40;
    });
    assert_parse_error(&longer_dictionary, "dictionary past the end");

    let longer_subheaders = with_format_data(&prefix, |fd| {
        fd.subheader_table.length = 1 << 40;
    });
    assert_parse_error(&longer_subheaders, "subheader table past the end");

    let longer_exceptions = with_format_data(&prefix, |fd| {
        fd.exception_data.length = 1 << 40;
    });
    assert_parse_error(&longer_exceptions, "exception data past the end");

    let wrapping_manifest = with_format_data(&prefix, |fd| {
        fd.enc_disc_manifest.offset = u64::MAX;
    });
    assert_parse_error(&wrapping_manifest, "disc manifest end overflows");
}

#[test]
fn parse_ssmd_prefix_partial_subheader_entry_returns_error() {
    let prefix = sample_prefix();

    let partial_entry = with_format_data(&prefix, |fd| {
        fd.subheader_table.length = 12;
    });
    let result = parse_ssmd_prefix::<u64>(&partial_entry);

    assert!(
        matches!(result, Err(SpriteShrinkCDError::Decode(_))),
        "{result:?}"
    );
}

#[test]
fn parse_ssmd_prefix_other_format_id_returns_error() {
    let other_format = with_header(&sample_prefix(), |header| {
        header.format_id = SSMD_UID + 1;
    });

    assert_parse_error(&other_format, "format ID");
}

#[test]
fn parse_ssmd_prefix_chunk_offset_overflow_returns_error() {
    // The data chunk stored at offset 40 ends up past u64::MAX.
    let near_max = with_format_data(&sample_prefix(), |fd| {
        fd.data_offset = u64::MAX - 10;
    });

    assert_parse_error(&near_max, "chunk offset overflow");
}
