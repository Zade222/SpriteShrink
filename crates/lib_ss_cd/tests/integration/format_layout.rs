//! Pins the on-disk layout of `SSMDFormatData` to
//! `File_Specification/SPECIFICATION.md`.
//!
//! Every existing SSMD archive depends on these sizes, offsets and
//! constants. If one of these tests fails, the change breaks compatibility
//! with archives already written, so update the spec and `SUPPORTED_VERSION`
//! along with it.

use std::mem::offset_of;

use sprite_shrink_cd::{SSMD_UID, SSMDFormatData, decode_ssmd_format_data};

/// Format data from `sample_format_data()`, as the little-endian `u64`
/// values of each region's offset and length in field order, then
/// `data_offset`.
const GOLDEN_FORMAT_DATA: [u64; 17] = [
    212, 100,   // enc_disc_manifest
    312, 2000,  // data_dictionary
    2312, 300,  // enc_chunk_index
    2612, 50,   // enc_audio_index
    2662, 60,   // enc_excep_index
    2722, 16,   // subheader_table
    2738, 24,   // exception_data
    2762, 5000, // audio_data
    7762,       // data_offset
];

/// Format data placed at offset 76, after a 36-byte header and a 40-byte
/// TOC, with a different length for every section.
fn sample_format_data() -> SSMDFormatData {
    SSMDFormatData::build_format_data(76, 100, 2000, 300, 50, 60, 16, 24, 5000)
}

fn golden_bytes() -> Vec<u8> {
    GOLDEN_FORMAT_DATA.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn ssmd_format_data_layout_matches_spec() {
    assert_eq!(size_of::<SSMDFormatData>(), 136);
    assert_eq!(SSMDFormatData::SIZE, 136);
    assert_eq!(align_of::<SSMDFormatData>(), 8);
    assert_eq!(offset_of!(SSMDFormatData, enc_disc_manifest), 0);
    assert_eq!(offset_of!(SSMDFormatData, data_dictionary), 16);
    assert_eq!(offset_of!(SSMDFormatData, enc_chunk_index), 32);
    assert_eq!(offset_of!(SSMDFormatData, enc_audio_index), 48);
    assert_eq!(offset_of!(SSMDFormatData, enc_excep_index), 64);
    assert_eq!(offset_of!(SSMDFormatData, subheader_table), 80);
    assert_eq!(offset_of!(SSMDFormatData, exception_data), 96);
    assert_eq!(offset_of!(SSMDFormatData, audio_data), 112);
    assert_eq!(offset_of!(SSMDFormatData, data_offset), 128);
}

#[test]
fn ssmd_uid_is_unchanged() {
    assert_eq!(SSMD_UID, 0xd541);
}

#[test]
fn build_format_data_regions_are_contiguous() {
    let format_data = sample_format_data();

    let regions = [
        format_data.enc_disc_manifest,
        format_data.data_dictionary,
        format_data.enc_chunk_index,
        format_data.enc_audio_index,
        format_data.enc_excep_index,
        format_data.subheader_table,
        format_data.exception_data,
        format_data.audio_data,
    ];

    assert_eq!(format_data.enc_disc_manifest.offset, 76 + SSMDFormatData::SIZE);
    for pair in regions.windows(2) {
        assert_eq!(pair[0].offset + pair[0].length, pair[1].offset);
    }
    let audio = format_data.audio_data;
    assert_eq!(audio.offset + audio.length, format_data.data_offset);
}

#[test]
fn ssmd_format_data_serializes_to_golden_bytes() {
    assert_eq!(
        sample_format_data().to_le_bytes().as_slice(),
        golden_bytes().as_slice()
    );
}

#[test]
fn decode_ssmd_format_data_reads_golden_bytes() {
    let format_data = decode_ssmd_format_data(&golden_bytes()).unwrap();

    assert_eq!(format_data, sample_format_data());
}
