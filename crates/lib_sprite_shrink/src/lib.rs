//! Builds and reads Sprite Shrink's SSMC archives: files split into
//! content-defined chunks, stored once each and compressed with Zstandard
//! against a shared dictionary.
//!
//! The library does no file I/O. The application opens, reads and writes
//! files, and passes the library bytes and callbacks: the read callbacks
//! fetch chunk data, and the write callbacks take the output. Operating
//! systems differ in how files are opened, locked and read, and a host such
//! as an emulator may not read from files at all.

mod archive;
pub use archive::{
    ArchiveBuilder, ChunkDecompressor, compress_chunks_with, decompress_chunk,
    decompress_chunk_checked, decompress_chunks_with
};

pub mod ffi;

mod lib_error_handling;
pub use lib_error_handling::{
    IsCancelled, SpriteShrinkError,
};

mod lib_structs;
pub use lib_structs::{
    ALGORITHM_ZSTD, HASH_TYPE_XXH3_64, HASH_TYPE_XXH3_128, SSMC_UID,
    ChunkHashState, ChunkHasher, ChunkLocation, ChunkMap, ChunkSet,
    CompressionResult, FileData, FileHeader, FileManifestParent,
    FileRegion, HashType, ParsedSSMC, ProcessedFileData, Progress,
    SeekMetadata, SerializedData, SSAChunkMeta, SSMCFormatData,
    SSMCTocEntry
};

mod parsing;
pub use parsing::{
    SUPPORTED_VERSION, MAGIC_NUMBER, SS_SEED,
    CheckedManifest,
    check_entry_name, parse_file_chunk_index, parse_file_header, parse_file_metadata,
    parse_file_toc, parse_format_data, parse_ssmc_prefix
};

mod processing;
pub use processing::{
    MAX_CHUNK_LEN, MAX_WINDOW_SIZE, MIN_WINDOW_SIZE,
    Hashable,
    create_file_manifest_and_chunks, process_file_in_memory,
    verify_single_file, test_compression};

mod serialization;
pub use serialization::{
    build_ssmc_prefix, dashmap_values_to_vec, serialize_uncompressed_data
};
