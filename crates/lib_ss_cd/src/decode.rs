use std::{
    collections::HashMap,
    hash::Hash
};
use bitcode::{
    Decode, decode
};
use bytemuck::try_from_bytes;
use sprite_shrink::{ChunkLocation};
use thiserror::Error;

use crate::{
    lib_error_handling::SpriteShrinkCDError,
    lib_structs::{DecodedSSMDMetadata, DiscManifest, SSMDFormatData}
};

#[derive(Error, Debug)]
pub enum DecodeError {
    #[error("Failed to decode index: {0}")]
    IndexDecodeError(String),

    #[error("Failed to decode {data}: {error}")]
    MetadataDecode{ data: String, error: String },

    #[error("Format data is malformed. {0}")]
    InvalidFormatData(String),

    #[error("Failed to decode file manifest: {0}")]
    ManifestDecodeError(String),
}

pub fn decode_ssmd_format_data(
    format_data: &[u8]
) -> Result<SSMDFormatData, SpriteShrinkCDError> {
    let decoded_format_data = try_from_bytes::<SSMDFormatData>(format_data)
        .map_err(|e| DecodeError::InvalidFormatData(e.to_string()))?;

    Ok(*decoded_format_data)
}

pub fn decode_ssmd_meta_data<H>(
    format_data: &SSMDFormatData,
    bin_metadata: &[u8]
) -> Result<DecodedSSMDMetadata<H>, SpriteShrinkCDError>
where
    for<'de> H: Eq + Decode<'de> + Hash
{
    let base_offset = format_data.enc_disc_manifest.length;

    let disc_manifests: Vec<DiscManifest<H>> = decode(
        &bin_metadata[..base_offset as usize]
    ).map_err(|e| DecodeError::MetadataDecode{
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

    let subheader_table: Vec<[u8; 8]> = decode(
        &bin_metadata[tbl_start..]
    ).map_err(|e| DecodeError::MetadataDecode{
        data: "Subheader Table".to_string(),
        error: e.to_string()
    })?;

    Ok(DecodedSSMDMetadata {
        disc_manifests,
        data_dictionary,
        chunk_index,
        audio_block_index,
        exception_index,
        subheader_table
    })
}
