mod archive;
pub use archive::{
    ArchiveBuilder, decompress_chunk
};

pub mod ffi;

mod lib_error_handling;
pub use lib_error_handling::{
    IsCancelled, SpriteShrinkError,
};

mod lib_structs;
pub use lib_structs::{
    ChunkLocation, FileData, FileHeader, FileManifestParent,
    ProcessedFileData, Progress, SerializedData, SSAChunkMeta, SSMCFormatData,
    TocEntry
};

mod parsing;
pub use parsing::{
    parse_file_chunk_index, parse_file_header, parse_file_metadata,
    parse_file_toc, parse_format_data,
    SUPPORTED_VERSION, MAGIC_NUMBER, SS_SEED
};

mod processing;
pub use processing::{
    Hashable,
    create_file_manifest_and_chunks, get_seek_chunks, process_file_in_memory,
    verify_single_file, test_compression};

mod serialization;
pub use serialization::{dashmap_values_to_vec, serialize_uncompressed_data};
