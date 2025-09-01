//! Defines core data structures for the CLI application.
//!
//! This module centralizes the definitions of fundamental structs used across
//! the application's lifecycle. These structures facilitate communication between
//! threads, manage database interactions, and handle temporary resources,
//! ensuring a clean and organized architecture.

use std::{
    path::PathBuf,
};

use redb::{Database, Key, TableDefinition, Value};

use sprite_shrink::{
    Hashable, SSAChunkMeta
};

/// A message containing a single data chunk and its hash.
///
/// This struct is used to send chunk data from file processing workers to a
/// central aggregator thread, enabling concurrent chunking and storage.
///
/// # Fields
///
/// * `chunk_hash`: The unique hash identifier of the data chunk.
/// * `chunk_data`: The raw binary data of the chunk.
pub struct ChunkMessage<H: Hashable> {
        pub chunk_hash: H,
        pub chunk_data: Vec<u8>
}

/// A handle for accessing the application's temporary database.
///
/// This struct encapsulates the shared database connection (`Arc<Database>`)
/// and the table definition, providing a convenient way to pass database 
/// access information between different parts of the application.
///
/// # Fields
///
/// * `db`: The database instance.
/// * `db_def`: The definition for the table where chunk data is stored.
pub struct DBInfo<'a, K: Key + 'static, V: Value + 'static>{
    pub db: Database,
    pub db_def: TableDefinition<'a, K, V>
}

/// A container for all metadata generated after processing a single file.
///
/// This struct holds the complete results from the chunking and hashing stage
/// for one file, including its verification hash and the metadata for all of 
/// its constituent chunks.
///
/// # Fields
///
/// * `file_name`: The original name of the processed file.
/// * `verification_hash`: The SHA-512 hash of the entire original file, for
///   verification.
/// * `chunk_count`: The total number of chunks the file was divided into.
/// * `chunk_meta`: A vector of metadata for each chunk, including its hash and
///   position.
pub struct FileCompleteData<H: Hashable> {
    pub file_name: String,
    pub verification_hash: [u8; 64],
    pub chunk_count: u64,
    pub chunk_meta: Vec<SSAChunkMeta<H>>
}

/// A resource guard that ensures a temporary database is deleted on drop.
///
/// This struct implements the **RAII** (Resource Acquisition Is 
/// Initialization) pattern. When an instance of `TempDatabase` is created,
/// it holds the path to a temporary file. When the instance goes out of scope,
/// its `drop` implementation is automatically called, ensuring the file is 
/// cleaned up.
///
/// # Fields
///
/// * `path`: The path to the temporary database file to be deleted on drop.
pub struct TempDatabase {
    pub path: PathBuf,
}
impl Drop for TempDatabase {
    fn drop(&mut self) {
        /*Attempt to remove the file, ignoring any errors (e.g., if the file
        was already removed or never created).*/
        let _ = std::fs::remove_file(&self.path);
    }
}





