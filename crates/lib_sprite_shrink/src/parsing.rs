//! Provides functions for parsing and validating sprite-shrink archives.
//!
//! This module contains the logic required to read the binary data from
//! an archive file and interpret its structure. It includes functions
//! for parsing the main file header, the file manifest, and the chunk
//! index. It also defines critical constants, such as the magic number
//! and supported version, to ensure file integrity and compatibility.

use std::collections::HashMap;

use bitcode::{
    Decode, decode
};
use serde::{Deserialize};
use thiserror::Error;

use crate::lib_error_handling::SpriteShrinkError;
use crate::processing::MAX_CHUNK_LEN;
use crate::lib_structs::{
    SSMC_UID,
    HashType,
    ChunkLocation, FileHeader, FileManifestParent, FileRegion, ParsedSSMC,
    SSMCTocEntry, SSMCFormatData
};

#[derive(Error, Debug)]
pub enum ParsingError {
    #[error("File {file} lists a chunk of {length} bytes, more than the \
        {max} bytes a chunk can hold.", max = MAX_CHUNK_LEN)]
    ChunkTooLong { file: usize, length: u32 },

    #[error("Failed to decode chunk index: {0}")]
    IndexDecodeError(String),

    #[error("Entry name {name:?} can't be used as a file name: {reason}.")]
    InvalidEntryName { name: String, reason: &'static str },

    #[error(
        "Archive format {} is not supported. This version reads formats \
        {} to {}.",
        format_version(*.0),
        format_version(oldest_supported_version()),
        format_version(SUPPORTED_VERSION)
    )]
    InvalidFileVersion(u32),

    #[error("File header is malformed. {0}")]
    InvalidHeader(String),

    #[error("Format data is malformed. {0}")]
    InvalidFormatData(String),

    #[error("File manifest is invalid: {0}. The archive may be damaged.")]
    InvalidManifest(String),

    #[error("Failed to decode file manifest: {0}")]
    ManifestDecodeError(String),

    #[error("Archive section is out of bounds. {0}")]
    SectionOutOfBounds(String),

    #[error("Failed to decode table of contents: {0}")]
    TOCDecodeError(String),

    #[error(
        "Unsupported hash type {0} in the archive header. This version \
        supports {supported}.",
        supported = supported_hash_types()
    )]
    UnsupportedHashType(u8),
}

/// A `file_version` as "major.minor", for example `2.1` for `0x0002_0001`.
fn format_version(version: u32) -> String {
    format!("{}.{}", version >> 16, version & 0xFFFF)
}

/// Lists every [`HashType`] as "ID (name)", for error messages, for
/// example `1 (xxh3_64) and 2 (xxh3_128)`.
fn supported_hash_types() -> String {
    let entries: Vec<String> = HashType::ALL
        .iter()
        .map(|hash_type| format!("{} ({})", hash_type.id(), hash_type.name()))
        .collect();

    match entries.split_last() {
        Some((last, rest)) if !rest.is_empty() => {
            format!("{} and {last}", rest.join(", "))
        }
        _ => entries.concat(),
    }
}

/// Checks that an archive entry name is a single ordinary file name, so a
/// host that extracts the entry into a folder creates a file in that folder
/// and nowhere else.
///
/// Names are checked against the rules of every platform the CLI runs on,
/// so an archive made on one can be extracted on the others. A name that
/// fails is rejected, never rewritten.
///
/// # Errors
///
/// Returns `ParsingError::InvalidEntryName` if the name is empty, is `.` or
/// `..`, contains `/`, `\`, `:` or a control character, ends with a dot or
/// a space, or is a Windows device name such as `CON` or `com1.bin`.
pub fn check_entry_name(name: &str) -> Result<(), ParsingError> {
    match entry_name_problem(name) {
        None => Ok(()),
        Some(reason) => Err(ParsingError::InvalidEntryName {
            name: name.to_owned(),
            reason,
        }),
    }
}

/// Why `name` can't be an entry name, or `None` if it can.
fn entry_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("it is empty")
    } else if name == "." || name == ".." {
        Some("it names a folder")
    } else if name.contains(['/', '\\']) {
        Some("it contains a path separator")
    } else if name.contains(':') {
        Some("it contains a colon, which Windows reads as a drive or a stream")
    } else if name.chars().any(char::is_control) {
        Some("it contains a control character")
    } else if name.ends_with(['.', ' ']) {
        Some("it ends with a dot or a space, which Windows removes")
    } else if is_windows_device_name(name) {
        Some("it is a Windows device name")
    } else {
        None
    }
}

/// Whether Windows opens a device instead of a file for `name`. Windows
/// ignores the extension and trailing spaces, so `nul.bin` and `COM1 .txt`
/// are devices too.
fn is_windows_device_name(name: &str) -> bool {
    let stem = name
        .split_once('.')
        .map_or(name, |(stem, _)| stem)
        .trim_end_matches(' ');

    if ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
    {
        return true;
    }

    let Some((prefix, number)) = stem.split_at_checked(3) else {
        return false;
    };
    let mut number = number.chars();
    (prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT"))
        && matches!(
            (number.next(), number.next()),
            (Some('0'..='9' | '¹' | '²' | '³'), None)
        )
}

/// The magic number used to identify a sprite-shrink archive file.
///
/// This 8-byte signature is at the beginning of the file and is used to
/// quickly verify that the file is a valid archive before parsing. The
/// value is "SSARCHV1".
#[unsafe(no_mangle)]
pub static MAGIC_NUMBER: [u8; 8] = *b"SSARCHV1";

/// The latest archive format version that this library supports.
///
/// The major version is in the high 16 bits and the minor in the low 16.
/// Header parsing accepts every version with the same major version, up
/// to this one: a newer minor version may store data this library doesn't
/// know, and a different major version lays the archive out differently.
pub static SUPPORTED_VERSION: u32 = 0x0002_0001;

/// The first minor version of [`SUPPORTED_VERSION`]'s major version, the
/// oldest format this library reads.
fn oldest_supported_version() -> u32 {
    SUPPORTED_VERSION & 0xFFFF_0000
}

/// The seed value used for deterministic chunking and hashing.
///
/// This constant is passed to the FastCDC chunking algorithm and the
/// xxHash hashing function. Using a fixed seed ensures that the same
/// file will always produce the same set of chunks and hashes, which is
/// critical for reliable deduplication.
pub static SS_SEED: u64 = 0x4202803010192019;


/// Parses the binary header data from a sprite-shrink archive.
///
/// This function takes a slice of bytes from the beginning of an archive
/// and interprets it as a `FileHeader`. It performs critical validation,
/// such as checking the magic number and ensuring the archive version
/// and hash type are supported by the current library.
///
/// # Arguments
///
/// * `header_data`: A byte slice of the raw header data from an archive.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(FileHeader)` containing the parsed and validated header.
/// - `Err(SpriteShrinkError)` if the data is malformed, the magic number is
///   incorrect, or the file version or hash type is unsupported.
pub fn parse_file_header(
    header_data: &[u8]
) -> Result<FileHeader, SpriteShrinkError> {
    let Ok(header_bytes) = header_data.try_into() else {
        return Err(ParsingError::InvalidHeader(format!(
            "Header must be {} bytes, got {}.",
            FileHeader::HEADER_SIZE,
            header_data.len()
        )).into());
    };
    let file_header = FileHeader::from_le_bytes(header_bytes);

    //Verify the magic number to ensure it's the correct file type.
    if file_header.magic_num != MAGIC_NUMBER {
        return Err(ParsingError::InvalidHeader(
            "File Magic Number is invalid.".to_string(),
        ).into());
    }

    /*Only versions with this library's major version, up to its own, share
    the layout read here. Format 1.x archives have the same magic number
    but a different header, so they must be stopped here.*/
    let version = file_header.file_version;
    if !(oldest_supported_version()..=SUPPORTED_VERSION).contains(&version) {
        return Err(ParsingError::InvalidFileVersion(version).into());
    }

    /*Reject hash types this library can't read here, so every reader stops
    at the header instead of failing later on hashes of an unknown width.*/
    HashType::try_from(file_header.hash_type)?;

    Ok(file_header)
}

/// Deserializes the file manifest from a raw byte slice.
///
/// This function is responsible for parsing the binary representation
/// of the file manifest, which contains metadata for every file in the
/// archive. It uses `bitcode` to decode the byte slice into a
/// structured `Vec<FileManifestParent>`.
///
/// # Arguments
///
/// * `manifest_data`: A byte slice holding the binary-encoded manifest.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<FileManifestParent>)` containing the parsed file manifests.
/// - `Err(SpriteShrinkError)` if the byte slice cannot be decoded due to data
///   corruption or a format mismatch.
pub fn parse_file_metadata<H>(
    manifest_data: &[u8]
) -> Result<Vec<FileManifestParent<H>>, SpriteShrinkError>
where
    for<'de> H: serde::Deserialize<'de> + Decode<'de>
{
    let file_manifest = decode(manifest_data)
        .map_err(|e| ParsingError::ManifestDecodeError(e.to_string()))?;

    Ok(file_manifest)
}

/// Deserializes the chunk index from a raw byte slice.
///
/// This function parses the binary data representing the chunk index,
/// which maps each unique chunk hash to its `ChunkLocation`. It uses
/// `bitcode` to decode the data into a `HashMap` for efficient lookups
/// during file extraction.
///
/// Offsets are returned as stored, relative to the format data's
/// `data_offset`. [`parse_ssmc_prefix`] returns absolute file offsets.
///
/// # Arguments
///
/// * `chunk_index_data`: A byte slice of the binary-encoded chunk index.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(HashMap<u64, ChunkLocation>)` containing the parsed index.
/// - `Err(SpriteShrinkError)` if the byte slice cannot be decoded.
pub fn parse_file_chunk_index<H>(
    chunk_index_data: &[u8]
) -> Result<HashMap<H, ChunkLocation>, SpriteShrinkError>
where
    for<'de> H: Eq + std::hash::Hash + Deserialize<'de> + Decode<'de>,
{
    let bin_chunk_index: Vec<(H, ChunkLocation)> = decode(chunk_index_data)
        .map_err(|e| ParsingError::IndexDecodeError(e.to_string()))?;

    let chunk_index: HashMap<H, ChunkLocation> = bin_chunk_index
        .into_iter()
        .collect();

    Ok(chunk_index)
}


/// Checks that a file manifest's chunks describe one file, in order: the
/// manifest holds `chunk_count` chunks, the first starts at byte 0, and
/// each later one starts where the one before it ends. Returns the file's
/// size.
///
/// Readers pick chunks by their offsets, so a gap, an overlap or an offset
/// out of order would make them return the wrong bytes.
///
/// # Errors
///
/// Returns `ParsingError::InvalidManifest` saying which rule the manifest
/// breaks.
pub(crate) fn check_manifest<H>(
    manifest: &FileManifestParent<H>
) -> Result<u64, ParsingError> {
    manifest_size(manifest).map_err(ParsingError::InvalidManifest)
}

/// [`check_manifest`], with the problem as text, so a caller can add which
/// file it's in.
fn manifest_size<H>(manifest: &FileManifestParent<H>) -> Result<u64, String> {
    let listed = manifest.chunk_metadata.len();
    if usize::try_from(manifest.chunk_count) != Ok(listed) {
        return Err(format!(
            "it gives its chunk count as {} but lists {listed} chunks",
            manifest.chunk_count
        ));
    }

    let mut end: u64 = 0;
    for (index, chunk) in manifest.chunk_metadata.iter().enumerate() {
        let number = index + 1;
        if chunk.offset != end {
            return Err(format!(
                "chunk {number} starts at byte {}, not at byte {end}, where \
                the chunks before it end",
                chunk.offset
            ));
        }
        end = end.checked_add(chunk.length.into()).ok_or_else(|| format!(
            "chunk {number} ends past the largest possible file size"
        ))?;
    }
    Ok(end)
}

/// Decodes an SSMC table of contents and checks each entry's name with
/// [`check_entry_name`].
///
/// # Errors
///
/// Returns `ParsingError::TOCDecodeError` if the bytes don't decode, or
/// `ParsingError::InvalidEntryName` for the first entry whose name can't be
/// used as a file name.
pub fn parse_file_toc(
    enc_toc_data: &[u8]
) -> Result<Vec<SSMCTocEntry>, SpriteShrinkError> {
    let bin_toc: Vec<SSMCTocEntry> = decode(enc_toc_data)
        .map_err(|e| ParsingError::TOCDecodeError(e.to_string()))?;

    for entry in &bin_toc {
        check_entry_name(&entry.filename)?;
    }

    Ok(bin_toc)
}


pub fn parse_format_data(
    format_data: &[u8]
) -> Result<SSMCFormatData, SpriteShrinkError> {
    let Ok(format_bytes) = format_data.try_into() else {
        return Err(ParsingError::InvalidFormatData(format!(
            "Format data must be {} bytes, got {}.",
            SSMCFormatData::SIZE,
            format_data.len()
        )).into());
    };

    Ok(SSMCFormatData::from_le_bytes(format_bytes))
}

/// Reads every part of an SSMC archive's metadata from its prefix.
///
/// `prefix` is the start of the archive, at least up to
/// `SSMCFormatData::metadata_prefix_end`; any bytes past that are ignored.
/// A host reads the header and format data first to find that length.
/// Each section is found only through the offsets the header and format
/// data record, and each is checked against the end of `prefix`.
///
/// The returned chunk index holds absolute file offsets, not the offsets
/// stored in the archive, which are relative to `data_offset`.
///
/// # Errors
///
/// Returns a `ParsingError` if the header is invalid or isn't for an SSMC
/// archive, a section lies outside `prefix`, a section fails to decode, a
/// file manifest lists a chunk longer than `MAX_CHUNK_LEN`, a manifest
/// fails [`check_manifest`] or disagrees with its table of contents entry
/// (`InvalidManifest`), or a chunk's absolute offset doesn't fit in a
/// `u64`.
pub fn parse_ssmc_prefix<H>(
    prefix: &[u8]
) -> Result<ParsedSSMC<H>, SpriteShrinkError>
where
    for<'de> H: Eq + std::hash::Hash + Deserialize<'de> + Decode<'de>,
{
    let header_region = FileRegion {
        offset: 0,
        length: FileHeader::HEADER_SIZE.into(),
    };
    let header = parse_file_header(header_region.bytes_in(prefix, "header")?)?;

    if header.format_id != SSMC_UID {
        return Err(ParsingError::InvalidHeader(format!(
            "Format ID {:#06x} is not an SSMC archive.",
            header.format_id
        )).into());
    }

    let toc_region = FileRegion {
        offset: header.enc_toc_offset.into(),
        length: header.enc_toc_length.into(),
    };
    let toc = parse_file_toc(toc_region.bytes_in(prefix, "table of contents")?)?;

    let format_data_region = FileRegion {
        offset: header.format_data_offset.into(),
        length: SSMCFormatData::SIZE,
    };
    let format_data = parse_format_data(
        format_data_region.bytes_in(prefix, "format data")?
    )?;

    let manifests = parse_file_metadata(
        format_data.enc_manifest.bytes_in(prefix, "file manifest")?
    )?;
    /*Each table of contents entry describes the manifest at the same
    position, so the two lists must be the same length.*/
    if manifests.len() != toc.len() {
        return Err(ParsingError::InvalidManifest(format!(
            "the archive has {} file manifests but {} table of contents \
            entries",
            manifests.len(),
            toc.len()
        )).into());
    }
    for (index, (manifest, entry)) in manifests.iter().zip(&toc).enumerate() {
        let file = index + 1;
        /*Readers size their chunk buffers from these lengths, so a damaged
        manifest could otherwise ask for gigabytes.*/
        let too_long = manifest.chunk_metadata.iter()
            .map(|chunk| chunk.length)
            .find(|&length| length as usize > MAX_CHUNK_LEN);
        if let Some(length) = too_long {
            return Err(ParsingError::ChunkTooLong { file, length }.into());
        }

        let size = manifest_size(manifest).map_err(|problem| {
            ParsingError::InvalidManifest(format!("file {file}: {problem}"))
        })?;
        if size != entry.uncompressed_size {
            return Err(ParsingError::InvalidManifest(format!(
                "file {file} is listed as {} bytes, but its chunks add up \
                to {size}",
                entry.uncompressed_size
            )).into());
        }
    }

    let dictionary = format_data.data_dictionary
        .bytes_in(prefix, "dictionary")?
        .to_vec();

    let mut chunk_index: HashMap<H, ChunkLocation> = parse_file_chunk_index(
        format_data.enc_chunk_index.bytes_in(prefix, "chunk index")?
    )?;

    for location in chunk_index.values_mut() {
        location.offset = location.offset
            .checked_add(format_data.data_offset)
            .ok_or_else(|| ParsingError::SectionOutOfBounds(format!(
                "Chunk offset {} past data offset {} doesn't fit in a u64.",
                location.offset,
                format_data.data_offset
            )))?;
    }

    Ok(ParsedSSMC {
        header,
        format_data,
        toc,
        manifests,
        dictionary,
        chunk_index,
    })
}

#[cfg(test)]
mod tests {
    use bitcode::encode;

    use super::*;
    use crate::lib_structs::{ALGORITHM_ZSTD, HASH_TYPE_XXH3_64, SSAChunkMeta};

    /// A manifest of chunks given as (offset, length), with a matching
    /// `chunk_count`.
    fn chunks(layout: &[(u64, u32)]) -> FileManifestParent<u64> {
        FileManifestParent {
            chunk_count: layout.len() as u64,
            chunk_metadata: layout
                .iter()
                .zip(0_u64..)
                .map(|(&(offset, length), hash)| SSAChunkMeta { hash, offset, length })
                .collect(),
        }
    }

    #[test]
    fn a_manifest_must_lay_its_chunks_end_to_end() {
        assert_eq!(manifest_size(&chunks(&[(0, 50), (50, 50), (100, 7)])), Ok(107));
        assert_eq!(manifest_size(&chunks(&[])), Ok(0));

        let mut miscounted = chunks(&[(0, 50)]);
        miscounted.chunk_count = 2;
        let cases = [
            ("a gap", chunks(&[(0, 50), (100, 50)]), "chunk 2 starts at byte 100"),
            ("an overlap", chunks(&[(0, 50), (25, 50)]), "chunk 2 starts at byte 25"),
            ("out of order", chunks(&[(50, 50), (0, 50)]), "chunk 1 starts at byte 50"),
            ("a late start", chunks(&[(1, 50)]), "chunk 1 starts at byte 1"),
            (
                "an offset that would overflow",
                chunks(&[(0, 50), (u64::MAX - 10, 50)]),
                "chunk 2 starts",
            ),
            ("a wrong count", miscounted, "chunk count as 2 but lists 1"),
        ];
        for (case, manifest, expected) in cases {
            let problem = manifest_size(&manifest).unwrap_err();
            assert!(problem.contains(expected), "{case}: {problem}");
        }
    }

    /// An 8-byte aligned buffer, so a test can place bytes at an offset
    /// that is known to be misaligned.
    #[repr(C, align(8))]
    struct Aligned<const N: usize>([u8; N]);

    fn sample_header() -> FileHeader {
        FileHeader::build_file_header(
            3, ALGORITHM_ZSTD, HASH_TYPE_XXH3_64, SSMC_UID, 40,
        ).unwrap()
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
                chunk_count: 1,
                chunk_metadata: vec![
                    SSAChunkMeta { hash: hashes[2], offset: 0, length: 8192 },
                ],
            },
        ]
    }

    fn assert_manifests_eq<H: PartialEq + std::fmt::Debug>(
        actual: &[FileManifestParent<H>],
        expected: &[FileManifestParent<H>],
    ) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert_eq!(a.chunk_count, e.chunk_count);
            assert_eq!(a.chunk_metadata.len(), e.chunk_metadata.len());
            for (am, em) in a.chunk_metadata.iter().zip(&e.chunk_metadata) {
                assert_eq!(am.hash, em.hash);
                assert_eq!(am.offset, em.offset);
                assert_eq!(am.length, em.length);
            }
        }
    }

    #[test]
    fn entry_names_that_are_not_one_plain_file_name_are_rejected() {
        let rejected = [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "/abs",
            "a\\b",
            "C:\\x",
            "C:x",
            "\\\\server\\share",
            "game.bin:stream",
            "a\u{1b}[2J",
            "a\nb",
            "a\u{7f}",
            "a\u{85}",
            "game.",
            "game ",
            "CON",
            "con.bin",
            "Nul.tar.gz",
            "AUX .bin",
            "COM0",
            "lpt9.bin",
            "COM\u{b9}.bin",
        ];

        for name in rejected {
            let err = check_entry_name(name).unwrap_err();
            assert!(
                matches!(&err, ParsingError::InvalidEntryName { name: n, .. } if n == name),
                "{name:?}: {err:?}"
            );
        }
    }

    #[test]
    fn ordinary_entry_names_are_accepted() {
        let accepted = [
            "Game (USA) [!].gba",
            "a b.c",
            ".hidden",
            "a..b",
            "\u{30b2}\u{30fc}\u{30e0}.bin",
            "CONSOLE.bin",
            "nul_x.bin",
            "COM10.bin",
            "LPT.bin",
            "COMx.bin",
        ];

        for name in accepted {
            assert!(check_entry_name(name).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn the_error_shows_control_characters_escaped() {
        let err = check_entry_name("a\u{1b}[2J").unwrap_err();

        assert!(!err.to_string().contains('\u{1b}'), "{err}");
        assert!(err.to_string().contains(r"a\u{1b}[2J"), "{err}");
    }

    #[test]
    fn parse_file_header_wrong_magic_returns_invalid_header() {
        let mut header = sample_header();
        header.magic_num = *b"SSARCHV0";

        let err = parse_file_header(&header.to_le_bytes()).unwrap_err();

        assert!(
            matches!(err, SpriteShrinkError::Parsing(
                ParsingError::InvalidHeader(_)
            )),
            "{err:?}"
        );
    }

    #[test]
    fn parse_file_header_accepts_format_2_0_and_2_1() {
        for version in [0x0002_0000, 0x0002_0001] {
            let mut header = sample_header();
            header.file_version = version;

            let parsed = parse_file_header(&header.to_le_bytes()).unwrap();

            assert_eq!(parsed.file_version, version);
        }
    }

    #[test]
    fn parse_file_header_rejects_other_major_and_newer_versions() {
        let cases = [
            (0, "0.0"),
            (0x0001_0000, "1.0"),
            (0x0001_FFFF, "1.65535"),
            (0x0002_0002, "2.2"),
            (0x0003_0000, "3.0"),
        ];

        for (version, shown) in cases {
            let mut header = sample_header();
            header.file_version = version;

            let err = parse_file_header(&header.to_le_bytes()).unwrap_err();

            assert!(
                matches!(err, SpriteShrinkError::Parsing(
                    ParsingError::InvalidFileVersion(v)
                ) if v == version),
                "{shown}: {err:?}"
            );
            assert_eq!(
                err.to_string(),
                format!(
                    "Archive format {shown} is not supported. This version \
                    reads formats 2.0 to 2.1."
                )
            );
        }
    }

    #[test]
    fn parse_file_header_accepts_only_known_hash_types() {
        // Every possible byte, so a check that accepts too wide a range
        // fails as well as one that rejects a known type.
        for id in 0..=u8::MAX {
            let mut header = sample_header();
            header.hash_type = id;

            let result = parse_file_header(&header.to_le_bytes());

            if HashType::ALL.iter().any(|hash_type| hash_type.id() == id) {
                assert_eq!(result.unwrap().hash_type, id);
            } else {
                assert!(
                    matches!(result, Err(SpriteShrinkError::Parsing(
                        ParsingError::UnsupportedHashType(found)
                    )) if found == id),
                    "hash type {id}: {result:?}"
                );
            }
        }
    }

    #[test]
    fn unsupported_hash_type_message_names_the_value_and_supported_types() {
        assert_eq!(
            ParsingError::UnsupportedHashType(3).to_string(),
            "Unsupported hash type 3 in the archive header. This version \
             supports 1 (xxh3_64) and 2 (xxh3_128)."
        );
    }

    #[test]
    fn parse_file_header_wrong_length_returns_invalid_header() {
        let mut buf = [0_u8; 40];
        buf[..36].copy_from_slice(&sample_header().to_le_bytes());

        for len in [0, 35, 37] {
            let err = parse_file_header(&buf[..len]).unwrap_err();

            assert!(
                matches!(err, SpriteShrinkError::Parsing(
                    ParsingError::InvalidHeader(_)
                )),
                "len {len}: {err:?}"
            );
        }
    }

    // A host can hand over the header from anywhere in a larger buffer, so
    // the parser can't assume the slice is aligned for `FileHeader`.
    #[test]
    fn parse_file_header_misaligned_slice_is_accepted() {
        let header = sample_header();
        let mut buf = Aligned([0_u8; 40]);
        buf.0[1..37].copy_from_slice(&header.to_le_bytes());

        let parsed = parse_file_header(&buf.0[1..37]).unwrap();

        assert_eq!(parsed, header);
    }

    #[test]
    fn parse_format_data_wrong_length_returns_invalid_format_data() {
        let buf = [0_u8; 64];

        for len in [0, 55, 57] {
            let result = parse_format_data(&buf[..len]);

            assert!(
                matches!(
                    result,
                    Err(SpriteShrinkError::Parsing(ParsingError::InvalidFormatData(_)))
                ),
                "len {len}"
            );
        }
    }

    #[test]
    fn parse_format_data_misaligned_slice_is_accepted() {
        let format_data = SSMCFormatData::build_format_data(76, 100, 2000, 300);
        let mut buf = Aligned([0_u8; 64]);
        buf.0[4..60].copy_from_slice(&format_data.to_le_bytes());

        let parsed = parse_format_data(&buf.0[4..60]).unwrap();

        assert_eq!(parsed, format_data);
    }

    #[test]
    fn parse_file_toc_encoded_toc_round_trips() {
        let toc = vec![
            SSMCTocEntry { filename: "a.gba".to_string(), uncompressed_size: 4196 },
            SSMCTocEntry { filename: "b (Rev 1).gba".to_string(), uncompressed_size: 0 },
        ];

        let parsed = parse_file_toc(&encode(&toc)).unwrap();

        assert_eq!(parsed, toc);
    }

    #[test]
    fn parse_file_toc_truncated_input_returns_toc_decode_error() {
        let toc = vec![
            SSMCTocEntry { filename: "a.gba".to_string(), uncompressed_size: 4196 },
        ];
        let bytes = encode(&toc);

        let result = parse_file_toc(&bytes[..bytes.len() - 1]);

        assert!(
            matches!(result, Err(SpriteShrinkError::Parsing(ParsingError::TOCDecodeError(_)))),
            "{result:?}"
        );
    }

    #[test]
    fn parse_file_metadata_u64_hashes_round_trip() {
        let manifests = sample_manifests::<u64>([1, u64::MAX, 7]);

        let parsed = parse_file_metadata::<u64>(&encode(&manifests)).unwrap();

        assert_manifests_eq(&parsed, &manifests);
    }

    #[test]
    fn parse_file_metadata_u128_hashes_round_trip() {
        let manifests = sample_manifests::<u128>([1, u128::MAX, 7]);

        let parsed = parse_file_metadata::<u128>(&encode(&manifests)).unwrap();

        assert_manifests_eq(&parsed, &manifests);
    }

    #[test]
    fn parse_file_metadata_truncated_input_returns_manifest_decode_error() {
        let bytes = encode(&sample_manifests::<u64>([1, 2, 3]));

        let result = parse_file_metadata::<u64>(&bytes[..bytes.len() - 1]);

        assert!(
            matches!(
                result,
                Err(SpriteShrinkError::Parsing(
                    ParsingError::ManifestDecodeError(_)
                ))
            ),
            "{result:?}"
        );
    }

    #[test]
    fn parse_file_chunk_index_u64_and_u128_round_trip() {
        let index_64: Vec<(u64, ChunkLocation)> = vec![
            (10, ChunkLocation { offset: 0, compressed_length: 512 }),
            (20, ChunkLocation { offset: 512, compressed_length: 33 }),
        ];
        let index_128: Vec<(u128, ChunkLocation)> = vec![
            (u128::MAX, ChunkLocation { offset: 0, compressed_length: 1 }),
        ];

        let parsed_64 = parse_file_chunk_index::<u64>(
            &encode(&index_64)
        ).unwrap();
        let parsed_128 = parse_file_chunk_index::<u128>(
            &encode(&index_128)
        ).unwrap();

        assert_eq!(parsed_64.len(), 2);
        assert_eq!(parsed_64[&20].offset, 512);
        assert_eq!(parsed_64[&20].compressed_length, 33);
        assert_eq!(parsed_128[&u128::MAX].compressed_length, 1);
    }

    #[test]
    fn parse_file_chunk_index_truncated_input_returns_index_decode_error() {
        let index: Vec<(u64, ChunkLocation)> =
            vec![(10, ChunkLocation { offset: 0, compressed_length: 512 })];
        let bytes = encode(&index);

        let result = parse_file_chunk_index::<u64>(&bytes[..bytes.len() - 1]);

        assert!(
            matches!(result, Err(SpriteShrinkError::Parsing(
                ParsingError::IndexDecodeError(_)
            ))),
            "{result:?}"
        );
    }
}
