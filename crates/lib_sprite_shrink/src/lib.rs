mod archive;
pub use archive::{
    ArchiveBuilder, decompress_chunk
};

pub mod ffi;

mod lib_error_handling;
pub use lib_error_handling::LibError;

mod lib_structs;
pub use lib_structs::{
    ChunkLocation, FileData, FileHeader, FileManifestParent, 
    ProcessedFileData, Progress, SSAChunkMeta
};

mod parsing;
pub use parsing::{
    parse_file_chunk_index, parse_file_header, parse_file_metadata, 
    SUPPORTED_VERSION, MAGIC_NUMBER, SS_SEED
};

mod processing;
pub use processing::{
    Hashable,
    create_file_manifest_and_chunks, get_seek_chunks, process_file_in_memory,
    verify_single_file, test_compression};

mod serialization;
pub use serialization::{serialize_uncompressed_data};