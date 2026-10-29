//! Builds the metadata prefix of an SSMD archive.
//!
//! This is the counterpart of `decode.rs`: it writes the sections that
//! `decode_ssmd_format_data` and `decode_ssmd_meta_data` read back, in the
//! order the specification lays them out.

use bitcode::{Encode, encode};
use sprite_shrink::{
    ALGORITHM_ZSTD,
    ChunkLocation, CompressionResult, FileHeader
};

use crate::{
    lib_error_handling::SpriteShrinkCDError,
    lib_structs::{SSMD_UID, DiscManifest, SSMDFormatData, SSMDTocEntry}
};

/// Everything [`build_ssmd_prefix`] needs to lay out an SSMD archive's
/// metadata.
///
/// # Fields
///
/// * `toc`: One entry per disc, in the order the discs are numbered.
/// * `disc_manifests`: One manifest per disc, in the same order as `toc`.
/// * `comp`: The dictionary and encoded chunk index from
///   `ArchiveBuilder::build`.
/// * `audio_block_index`: The hash and location of each compressed audio
///   block, relative to the start of the audio data. Empty for discs without
///   audio.
/// * `exception_index`: The exception index from the exception registry.
/// * `subheader_table`: The Mode 2 subheaders shared by all discs.
/// * `exception_blob`: The stored data of every exception sector.
/// * `audio_blob_size`: The total size of the compressed audio blocks,
///   which the host writes after the prefix and before the data chunks.
/// * `hash_type`: The hash used for the chunk hashes. 1 = `xxhash3_64`,
///   2 = `xxhash3_128`.
#[derive(Debug)]
pub struct SSMDPrefix<'a, H> {
    pub toc:                &'a [SSMDTocEntry],
    pub disc_manifests:     &'a [DiscManifest<H>],
    pub comp:               &'a CompressionResult,
    pub audio_block_index:  &'a [(H, ChunkLocation)],
    pub exception_index:    &'a [u64],
    pub subheader_table:    &'a [[u8; 8]],
    pub exception_blob:     &'a [u8],
    pub audio_blob_size:    u64,
    pub hash_type:          u8,
}

/// Builds every byte of an SSMD archive that comes before the compressed
/// audio and data: the file header, table of contents, format data, disc
/// manifest, dictionary, chunk index, audio index, exception index,
/// subheader table and exception data.
///
/// The host writes the returned prefix, then the compressed audio blocks,
/// then the compressed data chunks. The prefix is always
/// `metadata_prefix_end()` bytes long, so the audio starts exactly where the
/// format data says it does.
///
/// An empty audio index is left out entirely and recorded with length 0.
///
/// # Errors
///
/// Returns `SpriteShrinkCDError::SpriteShrink` if there are more than
/// `u32::MAX` discs, or the encoded table of contents is too large for the
/// header's 32-bit offsets.
pub fn build_ssmd_prefix<H>(
    parts: &SSMDPrefix<H>
) -> Result<Vec<u8>, SpriteShrinkCDError>
where
    H: Encode,
{
    let enc_toc = encode(parts.toc);
    let enc_disc_manifest = encode(parts.disc_manifests);
    let enc_audio_index = if parts.audio_block_index.is_empty() {
        Vec::new()
    } else {
        encode(parts.audio_block_index)
    };
    let enc_excep_index = encode(parts.exception_index);
    let subheader_bytes = parts.subheader_table.as_flattened();

    let file_header = FileHeader::build_file_header(
        parts.toc.len(),
        ALGORITHM_ZSTD,
        parts.hash_type,
        SSMD_UID,
        enc_toc.len(),
    )?;

    /*Offsets come from the lengths of the bytes actually written, so the
    format data always matches the prefix.*/
    let format_data = SSMDFormatData::build_format_data(
        file_header.format_data_offset as usize,
        enc_disc_manifest.len(),
        parts.comp.dictionary.len() as u64,
        parts.comp.enc_chunk_index.len() as u64,
        enc_audio_index.len() as u64,
        enc_excep_index.len() as u64,
        subheader_bytes.len() as u64,
        parts.exception_blob.len() as u64,
        parts.audio_blob_size,
    );

    let mut prefix = Vec::with_capacity(
        size_of::<FileHeader>() + enc_toc.len() +
            size_of::<SSMDFormatData>() + enc_disc_manifest.len() +
            parts.comp.dictionary.len() + parts.comp.enc_chunk_index.len() +
            enc_audio_index.len() + enc_excep_index.len() +
            subheader_bytes.len() + parts.exception_blob.len()
    );

    prefix.extend_from_slice(&file_header.to_le_bytes());
    prefix.extend_from_slice(&enc_toc);
    prefix.extend_from_slice(&format_data.to_le_bytes());
    prefix.extend_from_slice(&enc_disc_manifest);
    prefix.extend_from_slice(&parts.comp.dictionary);
    prefix.extend_from_slice(&parts.comp.enc_chunk_index);
    prefix.extend_from_slice(&enc_audio_index);
    prefix.extend_from_slice(&enc_excep_index);
    prefix.extend_from_slice(subheader_bytes);
    prefix.extend_from_slice(parts.exception_blob);

    Ok(prefix)
}
