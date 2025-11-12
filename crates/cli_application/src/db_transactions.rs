//! Manages all database transactions for the CLI application.
//!
//! This module provides a high-level API for interacting with the temporary
//! `redb` database used during the compression process. It abstracts away the
//! complexities of database connections, table management, and read/write
//! transactions. All functions are designed to be robust and provide clear
//! error handling, ensuring data consistency and integrity.

use std::{
    borrow::Borrow,
    fmt::Display,
};

use redb::{ReadableDatabase, ReadableTable, Value};

use crate::{
    cli_types::DBInfo,
    error_handling::CliError
};

use sprite_shrink::{
    Hashable
};

/// Inserts multiple key-value pairs into the database in a single transaction.
///
/// This function is designed for efficient bulk data insertion. It takes a
/// vector of key-value tuples (representing chunk hashes and their data) and
/// writes them to the database within a single write transaction. To support
/// data deduplication, it checks for the existence of each key before
/// inserting, avoiding redundant writes if a key is already present.
///
/// # Arguments
///
/// * `db_info`: A `DBInfo` struct containing the database connection and table
///   definition.
/// * `items`: A `Vec` where each element is a tuple containing the key (`H`)
///   and its corresponding data (`Vec<u8>`).
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(())` if the batch insertion is completed successfully.
/// - `Err(CliError)` if any database operation fails during the transaction.
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must implement the traits required for
///   use as a `redb` database key.
pub fn batch_insert<H>(
    db_info: &DBInfo<H, Vec<u8>>,
    items: &[(H, Vec<u8>)],
) -> Result<(), CliError>
where
    H: Hashable + redb::Key,
    for<'a> H: Borrow<<H as Value>::SelfType<'a>>,
{
    let write_txn = db_info.db.begin_write()?;
    {
        let mut table = write_txn.open_table(db_info.db_def)?;
        for (key, data) in items {
            // Using get/insert to avoid overwriting existing keys
            if table.get(*key)?.is_none() {
                table.insert(*key, data)?;
            }
        }
    }
    write_txn.commit()?;
    Ok(())
}

/// Retrieves a collection of data chunks from the database by their hashes.
///
/// This function efficiently fetches multiple data chunks corresponding to a
/// provided slice of hashes. It performs all reads within a single, consistent
/// read transaction. The order of the returned byte vectors is guaranteed to
/// match the order of the input hashes.
///
/// # Arguments
///
/// * `hashes`: A slice of hash keys (`&[H]`) specifying which chunks to
///   retrieve.
/// * `db_info`: A `DBInfo` struct containing the database connection and table
///   definition.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<Vec<u8>>)` containing the binary data for each requested chunk,
///   in the same order as the input hashes.
/// - `Err(CliError::KeyNotFound)` if any of the provided hashes do not exist
///   in the database.
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must implement the traits required for
///   use as a `redb` database key.
pub fn get_chunks<H>(
    hashes: &[H],
    db_info: &DBInfo<H, Vec<u8>>,
) -> Result<Vec<Vec<u8>>, CliError>
where
    H: Hashable + redb::Key + Display,
    //This bound is important for redb's generic key operations
    for<'a> H: Borrow<<H as Value>::SelfType<'a>>,
    for<'a> H: From<<H as Value>::SelfType<'a>>,
{
    //Prepare database for read transaction
    let read_txn = db_info.db.begin_read()?;
    let table = read_txn.open_table(db_info.db_def)?;

    let mut ret_chunks: Vec<Vec<u8>> = Vec::with_capacity(hashes.len());

    //If a chunk is found with the provided hash, return it's value.
    for hash in hashes{
        match table.get(*hash)?{
            Some(access_guard) => {
                ret_chunks.push(access_guard.value())
            }
            //If not return an error.
            None => {
                return Err(CliError::KeyNotFound(
                    format!("Hash {} not found in data_store", *hash)
                ));
            }
        }
    }

    Ok(ret_chunks)
}

/// Retrieves all keys (chunk hashes) from the database.
///
/// This function performs a full scan of the data store table to collect every
/// unique key. It operates within a single read transaction for consistency
/// and efficiency. The resulting vector of keys represents all unique chunks
/// identified during the file processing stage.
///
/// # Arguments
///
/// * `db_info`: A `DBInfo` struct containing the database connection and table
///   definition.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(Vec<H>)` containing a vector of all unique hash keys found in the
///   database.
/// - `Err(CliError)` if any database operation fails during the transaction.
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must implement the traits required for
///   use as a `redb` database key.
pub fn get_keys<H>(
    db_info: &DBInfo<H, Vec<u8>>,
) -> Result<Vec<H>, CliError>
where
    H: Hashable + redb::Key,
    //This bound is important for redb's generic key operations
    for<'a> H: From<<H as Value>::SelfType<'a>>,
{
    //Prepare database for read transaction
    let read_txn = db_info.db.begin_read()?;
    let table = read_txn.open_table(db_info.db_def)?;

    let all_keys: Result<Vec<H>, CliError> = table
        .iter()?
        //The iterator yields Result<(Key, Value), Error>
        .map(|result| {
            /*The result variable is a Result.
            We map over it to get the (key, _value) pair.*/
            result.map(|(key, _)| H::from(key.value()))
            .map_err(CliError::from)
        })
        //Collect the iterator of Results into a single Result containing a Vec.
        .collect();

    //Return all_keys
    all_keys
}

/// Calculates the total size of all data chunks stored in the database.
///
/// This function iterates through every entry in the data store table and sums
/// the lengths of their value fields (the chunk data). It provides an accurate
/// measure of the total unique, uncompressed data size, which is essential for
/// optimizing compression dictionary training.
///
/// # Arguments
///
/// * `db_info`: A `DBInfo` struct containing the database connection and table
///   definition.
///
/// # Returns
///
/// A `Result` which is:
/// - `Ok(u64)` containing the total combined size of all chunks in bytes.
/// - `Err(CliError)` if any database operation fails during the transaction.
///
/// # Type Parameters
///
/// * `H`: The generic hash type, which must implement the traits required for
///   use as a `redb` database key.
pub fn get_tot_data_size<H>(
    db_info: &DBInfo<H, Vec<u8>>,
) -> Result<u64, CliError>
where
    H: Hashable + redb::Key + Display,
    for<'a> H: Borrow<<H as Value>::SelfType<'a>>,
{
    //Prepare database for read transaction
    let read_txn = db_info.db.begin_read()?;
    let table = read_txn.open_table(db_info.db_def)?;

    //Prepare variable for storing total size.
    let mut total_size: u64 = 0;

    //The range call with .. creates an iterator over all key-value pairs.
    for result in table.range::<H>(..)? {
        let (_key, value) = result?;
        //.value() returns a byte slice, .len() gives its size in bytes.
        total_size += value.value().len() as u64;
    }

    Ok(total_size)
}
