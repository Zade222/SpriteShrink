use std::{
    collections::HashMap,
    hash::Hash
};
use bitcode::{
    Decode, decode
};
use bytemuck::cast_slice;
use sprite_shrink::{ChunkLocation, FileHeader, FileRegion, parse_file_header};
use thiserror::Error;

use crate::{
    lib_error_handling::SpriteShrinkCDError,
    lib_structs::{
        SSMD_UID,
        DecodedSSMDMetadata, DiscManifest, ParsedSSMD, SSMDFormatData,
        SSMDIndices, SSMDTocEntry
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

    let disc_manifests: Vec<DiscManifest<H>> = decode_section(
        format_data.enc_disc_manifest.bytes_in(prefix, "disc manifest")?,
        "Disc Manifest"
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

pub fn decode_ssmd_meta_data<H>(
    format_data: &SSMDFormatData,
    bin_metadata: &[u8]
) -> Result<DecodedSSMDMetadata<H>, SpriteShrinkCDError>
where
    for<'de> H: Eq + Decode<'de> + Hash
{
    let base_offset = format_data.enc_disc_manifest.offset;

    let manifest_len = format_data.enc_disc_manifest.length as usize;

    let disc_manifests: Vec<DiscManifest<H>> = decode(
        &bin_metadata[..manifest_len]
    ).map_err(|e| DecodeError::MetadataDecode {
        data: "Disc Manifest".to_string(),
        error: e.to_string()
    })?;

    let dd_start = (
        format_data.data_dictionary.offset - base_offset
    ) as usize;
    let dd_end = dd_start +
        format_data.data_dictionary.length as usize;

    let data_dictionary: Vec<u8> = bin_metadata[dd_start..dd_end].to_vec();

    let ci_start = (format_data.enc_chunk_index.offset - base_offset) as usize;
    let ci_end = ci_start + format_data.enc_chunk_index.length as usize;

    let dec_chunk_index: Vec<(H, ChunkLocation)> = decode(
        &bin_metadata[ci_start..ci_end]
    ).map_err(|e| DecodeError::MetadataDecode{
        data: "Chunk Index".to_string(),
        error: e.to_string()
    })?;

    let chunk_index: HashMap<H, ChunkLocation> = dec_chunk_index
        .into_iter()
        .map(|(h, mut loc)| {
            loc.offset += format_data.data_offset;
            (h, loc)
        })
        .collect();

    let audio_block_index = if format_data.enc_audio_index.length > 0 {
        let abi_start = (
            format_data.enc_audio_index.offset - base_offset
        ) as usize;
        let abi_end = abi_start + format_data.enc_audio_index.length as usize;

        let dec_audio_index: Vec<(H, ChunkLocation)> = decode(
            &bin_metadata[abi_start..abi_end]
        ).map_err(|e| DecodeError::MetadataDecode{
            data: "Audio Index".to_string(),
            error: e.to_string()
        })?;

        let hash_map: HashMap<H, ChunkLocation> = dec_audio_index
            .into_iter()
            .map(|(h, mut loc)| {
                loc.offset += format_data.audio_data.offset;
                (h, loc)
            })
            .collect();

        Some(hash_map)
    } else {
        None
    };

    let exception_index = if format_data.exception_data.length > 0 {
        let idx_start = (
            format_data.enc_excep_index.offset - base_offset
        ) as usize;
        let idx_end = idx_start + format_data.enc_excep_index.length as usize;

        let dec_excep_idx: Vec<u64> = decode(
            &bin_metadata[idx_start..idx_end]
        ).map_err(|e| DecodeError::MetadataDecode{
            data: "Exception Index".to_string(),
            error: e.to_string()
        })?;

        Some(dec_excep_idx)
    } else {
        None
    };

    let tbl_start = (
        format_data.subheader_table.offset - base_offset
    ) as usize;

    let subheader_table: Vec<[u8; 8]> = cast_slice(
        &bin_metadata[tbl_start..]
    ).to_vec();

    Ok(DecodedSSMDMetadata {
        disc_manifests,
        data_dictionary,
        chunk_index,
        audio_block_index,
        exception_index,
        subheader_table
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An 8-byte aligned buffer, so a test can place bytes at an offset
    /// that is known to be misaligned.
    #[repr(C, align(8))]
    struct Aligned<const N: usize>([u8; N]);

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
