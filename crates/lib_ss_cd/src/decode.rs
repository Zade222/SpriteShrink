use std::{
    collections::HashMap,
    hash::Hash
};
use bitcode::{
    Decode, decode
};
use sprite_shrink::{ChunkLocation, FileHeader, FileRegion, parse_file_header};
use thiserror::Error;

use crate::{
    lib_error_handling::SpriteShrinkCDError,
    lib_structs::{
        SSMD_UID,
        ContentBlock, DataChunkLayout, DiscManifest, ParsedSSMD,
        RleSectorMap, SSMDFormatData, SSMDIndices, SSMDTocEntry,
        SubHeaderEntry
    }
};

#[derive(Error, Debug)]
pub enum DecodeError {
    //#[error("Failed to decode index: {0}")]
    //IndexDecodeError(String),

    #[error("Failed to decode {data}: {error}")]
    MetadataDecode{ data: String, error: String },

    #[error("Format data is malformed. {0}")]
    InvalidFormatData(String),

    #[error("File header is malformed. {0}")]
    InvalidHeader(String),

    //#[error("Failed to decode file manifest: {0}")]
    //ManifestDecodeError(String),
}

/// The first format version whose disc manifests list the disc's files
/// and tracks (`DiscManifest::files`).
const FILE_LAYOUT_VERSION: u32 = 0x0002_0001;

/// A disc manifest as format versions before 2.1 wrote it: the current
/// fields without `files`. bitcode stores no field names, so the old layout
/// needs its own type to decode.
#[derive(Decode)]
#[cfg_attr(test, derive(bitcode::Encode))]
struct DiscManifestV2<H> {
    lba_map: Vec<(u32, u32)>,
    rle_sector_map: RleSectorMap,
    audio_block_map: Vec<ContentBlock<H>>,
    data_stream_layout: Vec<DataChunkLayout<H>>,
    subheader_index: Vec<SubHeaderEntry>,
    disc_exception_index: Vec<(u32, u32)>,
    integrity_hash: u64,
}

impl<H> From<DiscManifestV2<H>> for DiscManifest<H> {
    /// The old manifest has no file layout, so `files` is empty and the
    /// CUE sheet is inferred from the sector map.
    fn from(old: DiscManifestV2<H>) -> Self {
        Self {
            files: Vec::new(),
            lba_map: old.lba_map,
            rle_sector_map: old.rle_sector_map,
            audio_block_map: old.audio_block_map,
            data_stream_layout: old.data_stream_layout,
            subheader_index: old.subheader_index,
            disc_exception_index: old.disc_exception_index,
            integrity_hash: old.integrity_hash,
        }
    }
}

pub fn decode_ssmd_format_data(
    format_data: &[u8]
) -> Result<SSMDFormatData, SpriteShrinkCDError> {
    let Ok(format_bytes) = format_data.try_into() else {
        return Err(DecodeError::InvalidFormatData(format!(
            "Format data must be {} bytes, got {}.",
            SSMDFormatData::SIZE,
            format_data.len()
        )).into());
    };

    Ok(SSMDFormatData::from_le_bytes(format_bytes))
}

/// Reads every part of an SSMD archive's metadata from its prefix.
///
/// `prefix` is the start of the archive, at least up to
/// `SSMDFormatData::metadata_prefix_end`; any bytes past that are ignored.
/// A host reads the header and format data first to find that length.
/// Each section is found only through the offsets the header and format
/// data record, and each is checked against the end of `prefix`.
///
/// The chunk and audio block indexes hold absolute file offsets, not the
/// offsets stored in the archive, which are relative to `data_offset` and
/// `audio_data.offset`. An index section recorded with length 0 is empty.
///
/// # Errors
///
/// Returns an error if the header is invalid or isn't for an SSMD
/// archive, a section lies outside `prefix`, a section fails to decode,
/// the subheader table's length isn't a multiple of 8, or a chunk's
/// absolute offset doesn't fit in a `u64`.
pub fn parse_ssmd_prefix<H>(
    prefix: &[u8]
) -> Result<ParsedSSMD<H>, SpriteShrinkCDError>
where
    for<'de> H: Eq + Decode<'de> + Hash
{
    let header_region = FileRegion {
        offset: 0,
        length: FileHeader::HEADER_SIZE.into(),
    };
    let header = parse_file_header(header_region.bytes_in(prefix, "header")?)?;

    if header.format_id != SSMD_UID {
        return Err(DecodeError::InvalidHeader(format!(
            "Format ID {:#06x} is not an SSMD archive.",
            header.format_id
        )).into());
    }

    let toc_region = FileRegion {
        offset: header.enc_toc_offset.into(),
        length: header.enc_toc_length.into(),
    };
    let toc: Vec<SSMDTocEntry> = decode_section(
        toc_region.bytes_in(prefix, "table of contents")?,
        "Table of Contents"
    )?;

    let format_data_region = FileRegion {
        offset: header.format_data_offset.into(),
        length: SSMDFormatData::SIZE,
    };
    let format_data = decode_ssmd_format_data(
        format_data_region.bytes_in(prefix, "format data")?
    )?;

    let disc_manifests = decode_disc_manifests(
        format_data.enc_disc_manifest.bytes_in(prefix, "disc manifest")?,
        header.file_version
    )?;

    let data_dictionary = format_data.data_dictionary
        .bytes_in(prefix, "dictionary")?
        .to_vec();

    let chunk_index = decode_location_index(
        format_data.enc_chunk_index.bytes_in(prefix, "chunk index")?,
        format_data.data_offset,
        "Chunk Index"
    )?;

    let audio_block_index = decode_location_index(
        format_data.enc_audio_index.bytes_in(prefix, "audio index")?,
        format_data.audio_data.offset,
        "Audio Index"
    )?;

    let exception_index_bytes = format_data.enc_excep_index
        .bytes_in(prefix, "exception index")?;
    let exception_index: Vec<u64> = if exception_index_bytes.is_empty() {
        Vec::new()
    } else {
        decode_section(exception_index_bytes, "Exception Index")?
    };

    let (subheader_table, remainder) = format_data.subheader_table
        .bytes_in(prefix, "subheader table")?
        .as_chunks::<8>();
    if !remainder.is_empty() {
        return Err(DecodeError::InvalidFormatData(format!(
            "Subheader table length {} is not a multiple of 8.",
            format_data.subheader_table.length
        )).into());
    }

    let exception_blob = format_data.exception_data
        .bytes_in(prefix, "exception data")?
        .to_vec();

    Ok(ParsedSSMD {
        header,
        format_data,
        toc,
        disc_manifests,
        data_dictionary,
        indices: SSMDIndices {
            chunk_index,
            audio_block_index,
            exception_index,
        },
        subheader_table: subheader_table.to_vec(),
        exception_blob,
    })
}

/// Decodes the disc manifests in the layout `file_version` wrote them
/// with.
fn decode_disc_manifests<H>(
    bytes: &[u8],
    file_version: u32,
) -> Result<Vec<DiscManifest<H>>, DecodeError>
where
    for<'de> H: Decode<'de>
{
    if file_version < FILE_LAYOUT_VERSION {
        let old: Vec<DiscManifestV2<H>> =
            decode_section(bytes, "Disc Manifest")?;
        Ok(old.into_iter().map(DiscManifest::from).collect())
    } else {
        decode_section(bytes, "Disc Manifest")
    }
}

/// Decodes one bitcode section, naming it in the error.
fn decode_section<T>(bytes: &[u8], data: &str) -> Result<T, DecodeError>
where
    for<'de> T: Decode<'de>
{
    decode(bytes).map_err(|e| DecodeError::MetadataDecode {
        data: data.to_string(),
        error: e.to_string()
    })
}

/// Decodes a chunk or audio block index and makes each offset absolute by
/// adding `base`, the start of the blob the offsets are stored relative
/// to. An empty section is an empty index.
fn decode_location_index<H>(
    bytes: &[u8],
    base: u64,
    data: &str
) -> Result<HashMap<H, ChunkLocation>, DecodeError>
where
    for<'de> H: Eq + Decode<'de> + Hash
{
    if bytes.is_empty() {
        return Ok(HashMap::new());
    }

    let stored: Vec<(H, ChunkLocation)> = decode_section(bytes, data)?;

    stored
        .into_iter()
        .map(|(hash, mut location)| {
            location.offset = location.offset.checked_add(base).ok_or_else(|| {
                DecodeError::InvalidFormatData(format!(
                    "{data} offset {} past {base} doesn't fit in a u64.",
                    location.offset
                ))
            })?;
            Ok((hash, location))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An 8-byte aligned buffer, so a test can place bytes at an offset
    /// that is known to be misaligned.
    #[repr(C, align(8))]
    struct Aligned<const N: usize>([u8; N]);

    fn v2_manifest() -> DiscManifestV2<u64> {
        DiscManifestV2 {
            lba_map: vec![(0, 150)],
            rle_sector_map: RleSectorMap {
                runs: vec![(300, crate::SectorType::Mode1)],
            },
            audio_block_map: Vec::new(),
            data_stream_layout: vec![DataChunkLayout { hash: 7, uncomp_len: 4096 }],
            subheader_index: Vec::new(),
            disc_exception_index: vec![(3, 0)],
            integrity_hash: 0xABCD,
        }
    }

    #[test]
    fn a_manifest_from_before_2_1_decodes_without_a_layout() {
        let bytes = bitcode::encode(&vec![v2_manifest()]);
        let manifests: Vec<DiscManifest<u64>> =
            decode_disc_manifests(&bytes, 0x0002_0000).unwrap();

        assert_eq!(manifests, vec![DiscManifest::from(v2_manifest())]);
        assert_eq!(manifests[0].files.len(), 0);
        assert_eq!(manifests[0].integrity_hash, 0xABCD);
    }

    #[test]
    fn a_2_1_manifest_decodes_with_its_layout() {
        let manifest = DiscManifest {
            files: vec![crate::DiscFile {
                name: "disc.bin".to_string(),
                sector_count: 300,
                tracks: Vec::new(),
            }],
            ..DiscManifest::from(v2_manifest())
        };
        let bytes = bitcode::encode(&vec![manifest.clone()]);

        assert_eq!(
            decode_disc_manifests::<u64>(&bytes, FILE_LAYOUT_VERSION).unwrap(),
            vec![manifest]
        );
    }

    #[test]
    fn the_layout_version_follows_the_supported_version() {
        // A writer at SUPPORTED_VERSION must write the layout this reader
        // expects for that version.
        assert!(sprite_shrink::SUPPORTED_VERSION >= FILE_LAYOUT_VERSION);
    }

    fn sample_format_data() -> SSMDFormatData {
        SSMDFormatData::build_format_data(
            76, 100, 2000, 300, 50, 60, 16, 24, 5000
        )
    }

    #[test]
    fn decode_ssmd_format_data_wrong_length_returns_invalid_format_data() {
        let buf = [0_u8; 144];

        for len in [0, 135, 137] {
            let result = decode_ssmd_format_data(&buf[..len]);

            assert!(
                matches!(
                    result,
                    Err(SpriteShrinkCDError::Decode(
                        DecodeError::InvalidFormatData(_)
                    ))
                ),
                "len {len}"
            );
        }
    }

    // A host can hand over the format data from anywhere in a larger
    // buffer, so the decoder can't assume the slice is aligned for
    // `SSMDFormatData`.
    #[test]
    fn decode_ssmd_format_data_misaligned_slice_is_accepted() {
        let format_data = sample_format_data();
        let mut buf = Aligned([0_u8; 144]);
        buf.0[4..140].copy_from_slice(&format_data.to_le_bytes());

        let parsed = decode_ssmd_format_data(&buf.0[4..140]).unwrap();

        assert_eq!(parsed, format_data);
    }
}
