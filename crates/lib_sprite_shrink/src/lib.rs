mod archive;
pub use archive::ArchiveBuilder;

mod lib_error_handling;
pub use lib_error_handling::LibError;


mod processing;
pub use processing::{
    create_file_manifest_and_chunks, process_file_in_memory, 
    rebuild_and_verify_single_file, test_compression};

mod parsing;
pub use parsing::{
    parse_file_chunk_index, parse_file_header, parse_file_metadata, 
    SUPPORTED_VERSION, MAGIC_NUMBER
};

mod serialization;
pub use serialization::{serialize_uncompressed_data};

mod lib_structs;
pub use lib_structs::{
    FileData, FileHeader, FileManifestParent, ProcessedFileData, Progress, 
    SSAChunkMeta, ChunkLocation};