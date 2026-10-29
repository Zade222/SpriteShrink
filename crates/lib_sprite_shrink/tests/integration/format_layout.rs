//! Pins the on-disk layout of the fixed-size archive structures to
//! `SSMC_File_Specification/SPECIFICATION.md`.
//!
//! Every existing archive depends on these sizes, offsets and constants. If
//! one of these tests fails, the change breaks compatibility with archives
//! already written, so update the spec and `SUPPORTED_VERSION` along with it.

use std::mem::offset_of;

use sprite_shrink::{
    FileHeader, FileRegion, MAGIC_NUMBER, SS_SEED, SSMC_UID, SSMCFormatData,
    SUPPORTED_VERSION, parse_file_header, parse_format_data,
};

/// Header from `build_file_header(3, 98, 1, SSMC_UID, 40)`, written out by
/// hand from the table in SPECIFICATION.md §2.1. Multi-byte fields are little
/// endian.
const GOLDEN_HEADER: [u8; 36] = [
    0x53, 0x53, 0x41, 0x52, 0x43, 0x48, 0x56, 0x31, // magic_num "SSARCHV1"
    0x00, 0x00, 0x02, 0x00,                         // file_version 0x00020000
    0x03, 0x00, 0x00, 0x00,                         // file_count 3
    0x62, 0x00,                                     // algorithm 98 (zstd)
    0x01,                                           // hash_type 1 (xxh3_64)
    0x00,                                           // _pad1
    0x00, 0x00,                                     // format_id SSMC
    0x00, 0x00,                                     // _pad2
    0x24, 0x00, 0x00, 0x00,                         // enc_toc_offset 36
    0x28, 0x00, 0x00, 0x00,                         // enc_toc_length 40
    0x4c, 0x00, 0x00, 0x00,                         // format_data_offset 76
];

#[test]
fn file_header_size_and_alignment_match_spec() {
    assert_eq!(size_of::<FileHeader>(), 36);
    assert_eq!(FileHeader::HEADER_SIZE, 36);
    assert_eq!(align_of::<FileHeader>(), 4);
}

#[test]
fn file_header_field_offsets_match_spec() {
    assert_eq!(offset_of!(FileHeader, magic_num), 0);
    assert_eq!(offset_of!(FileHeader, file_version), 8);
    assert_eq!(offset_of!(FileHeader, file_count), 12);
    assert_eq!(offset_of!(FileHeader, algorithm), 16);
    assert_eq!(offset_of!(FileHeader, hash_type), 18);
    assert_eq!(offset_of!(FileHeader, _pad1), 19);
    assert_eq!(offset_of!(FileHeader, format_id), 20);
    assert_eq!(offset_of!(FileHeader, _pad2), 22);
    assert_eq!(offset_of!(FileHeader, enc_toc_offset), 24);
    assert_eq!(offset_of!(FileHeader, enc_toc_length), 28);
    assert_eq!(offset_of!(FileHeader, format_data_offset), 32);
}

#[test]
fn file_region_layout_matches_spec() {
    assert_eq!(size_of::<FileRegion>(), 16);
    assert_eq!(offset_of!(FileRegion, offset), 0);
    assert_eq!(offset_of!(FileRegion, length), 8);
}

#[test]
fn ssmc_format_data_layout_matches_spec() {
    assert_eq!(size_of::<SSMCFormatData>(), 56);
    assert_eq!(SSMCFormatData::SIZE, 56);
    assert_eq!(align_of::<SSMCFormatData>(), 8);
    assert_eq!(offset_of!(SSMCFormatData, enc_manifest), 0);
    assert_eq!(offset_of!(SSMCFormatData, data_dictionary), 16);
    assert_eq!(offset_of!(SSMCFormatData, enc_chunk_index), 32);
    assert_eq!(offset_of!(SSMCFormatData, data_offset), 48);
}

#[test]
fn format_constants_are_unchanged() {
    assert_eq!(MAGIC_NUMBER, *b"SSARCHV1");
    assert_eq!(SUPPORTED_VERSION, 0x0002_0000);
    // Changing the seed changes every chunk boundary and hash, so the same
    // input no longer produces the same archive.
    assert_eq!(SS_SEED, 0x4202_8030_1019_2019);
    assert_eq!(SSMC_UID, 0x0000);
}

#[test]
fn build_file_header_places_toc_after_header_and_format_data_after_toc() {
    let header = FileHeader::build_file_header(3, 98, 1, SSMC_UID, 40);

    assert_eq!(header.magic_num, MAGIC_NUMBER);
    assert_eq!(header.file_version, SUPPORTED_VERSION);
    assert_eq!(header.file_count, 3);
    assert_eq!(header.algorithm, 98);
    assert_eq!(header.hash_type, 1);
    assert_eq!(header._pad1, 0);
    assert_eq!(header.format_id, SSMC_UID);
    assert_eq!(header._pad2, [0, 0]);
    assert_eq!(header.enc_toc_offset, 36);
    assert_eq!(header.enc_toc_length, 40);
    assert_eq!(header.format_data_offset, 36 + 40);
}

#[test]
fn build_file_header_serializes_to_golden_bytes() {
    let header = FileHeader::build_file_header(3, 98, 1, SSMC_UID, 40);

    assert_eq!(header.to_le_bytes(), GOLDEN_HEADER);
}

#[test]
fn parse_file_header_reads_golden_bytes() {
    let header = parse_file_header(&GOLDEN_HEADER).unwrap();

    assert_eq!(header.file_count, 3);
    assert_eq!(header.algorithm, 98);
    assert_eq!(header.hash_type, 1);
    assert_eq!(header.format_id, SSMC_UID);
    assert_eq!(header.enc_toc_offset, 36);
    assert_eq!(header.enc_toc_length, 40);
    assert_eq!(header.format_data_offset, 76);
}

#[test]
fn build_format_data_regions_are_contiguous() {
    let format_data_offset: u64 = 76;
    let format_data = SSMCFormatData::build_format_data(76, 100, 2000, 300);

    let regions = [
        format_data.enc_manifest,
        format_data.data_dictionary,
        format_data.enc_chunk_index,
    ];

    assert_eq!(
        format_data.enc_manifest.offset,
        format_data_offset + SSMCFormatData::SIZE
    );
    for pair in regions.windows(2) {
        assert_eq!(pair[0].offset + pair[0].length, pair[1].offset);
    }
    let chunk_index = format_data.enc_chunk_index;
    assert_eq!(chunk_index.offset + chunk_index.length, format_data.data_offset);

    assert_eq!(format_data.enc_manifest.length, 100);
    assert_eq!(format_data.data_dictionary.length, 2000);
    assert_eq!(format_data.enc_chunk_index.length, 300);
}

#[test]
fn ssmc_format_data_round_trips_through_bytes() {
    let format_data = SSMCFormatData::build_format_data(76, 100, 2000, 300);
    let expected: Vec<u8> = [132_u64, 100, 232, 2000, 2232, 300, 2532]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert_eq!(format_data.to_le_bytes().as_slice(), expected.as_slice());

    let parsed = parse_format_data(&format_data.to_le_bytes()).unwrap();

    assert_eq!(parsed, format_data);
}
