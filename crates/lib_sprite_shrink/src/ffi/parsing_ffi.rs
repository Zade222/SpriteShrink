//! FFI-safe parsing functions for sprite-shrink archives.
//!
//! This module exposes Rust's parsing logic to C callers, handling the
//! conversion of data types and memory management across the FFI boundary.

use std::{hash::Hash, os::raw::c_void, slice};

use bitcode::Decode;
use serde::Deserialize;

use crate::ffi::FFIResult;
use crate::ffi::ffi_ownership::{
    free_manifests, free_toc, manifests_into_ffi, toc_into_ffi,
    vec_into_raw_parts,
};
use crate::ffi::ffi_types::{
    ChunkIndexMap, FFIParsedSSMC, FFIParsedSSMCU64, FFIParsedSSMCU128,
    U128Bytes,
};
use crate::lib_structs::{FileHeader, SSMCFormatData};
use crate::parsing::{parse_file_header, parse_format_data, parse_ssmc_prefix};

/// Parses a file header from a byte slice.
///
/// On success, returns `FFIResult::StatusOk` and populates `out_ptr`.
/// On failure, returns one of:
/// - `FFIResult::NullArgument` if `header_data_array_ptr` or `out_ptr` is
///   null.
/// - `FFIResult::InvalidHeader` if `header_data_len` is not exactly the size
///   of `FileHeader` or the magic number is wrong.
/// - `FFIResult::UnsupportedVersion` if the archive is newer than this
///   library.
///
/// # Safety
/// - `header_data_array_ptr` must point to valid memory of at least
///   `header_data_len` bytes.
/// - `out_ptr` must be a valid pointer to a `*mut FileHeader`.
/// - The pointer returned via `out_ptr` is owned by the caller and MUST be
///   freed by passing it to `free_file_header`.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks only #[must_use] functions SPRITE_SHRINK_MUST_USE"
)]
#[must_use]
pub unsafe extern "C" fn parse_file_header_ffi(
    header_data_array_ptr: *const u8,
    header_data_len: usize,
    out_ptr: *mut *mut FileHeader,
) -> FFIResult {
    if header_data_array_ptr.is_null() || out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    // SAFETY: the caller guarantees `header_data_array_ptr` points to
    // `header_data_len` readable bytes, and it was checked for null above.
    let header_data = unsafe {
        slice::from_raw_parts(header_data_array_ptr, header_data_len)
    };

    match parse_file_header(header_data) {
        Ok(data) => {
            // SAFETY: `out_ptr` was checked for null above and the caller
            // guarantees it is valid for writes.
            unsafe { *out_ptr = Box::into_raw(Box::new(data)) };
            FFIResult::StatusOk
        }
        Err(e) => e.into(),
    }
}

/// Frees the memory for a `FileHeader` that was allocated by
/// `parse_file_header_ffi`.
///
/// Passing a null pointer does nothing.
///
/// # Safety
/// `ptr` must be null or a pointer returned from a successful call to
/// `parse_file_header_ffi` that has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_file_header(ptr: *mut FileHeader) {
    if ptr.is_null() {
        return;
    }

    // SAFETY: `ptr` is non-null and, per the caller's guarantee, came from
    // `Box::into_raw` in `parse_file_header_ffi` and has not been freed.
    drop(unsafe { Box::from_raw(ptr) });
}

/// Parses an SSMC archive's format data, which records where each metadata
/// section is and where the compressed data starts.
///
/// A host reads the header with `parse_file_header_ffi`, then reads
/// `sizeof(SSMCFormatData)` bytes at the header's `format_data_offset` and
/// passes them here. The archive's metadata ends at `data_offset`, so
/// reading the file up to there gives the prefix for `parse_ssmc_prefix_*`.
///
/// On success, returns `FFIResult::StatusOk` and writes the format data to
/// `out`. Returns `FFIResult::NullArgument` if a pointer is null, and
/// `FFIResult::InternalError` if `format_data_len` isn't the size of
/// `SSMCFormatData`.
///
/// # Safety
/// - `format_data_ptr` must point to `format_data_len` readable bytes.
/// - `out` must be valid for writes. Nothing is allocated, so there's
///   nothing to free.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks only #[must_use] functions SPRITE_SHRINK_MUST_USE"
)]
#[must_use]
pub unsafe extern "C" fn parse_format_data_ffi(
    format_data_ptr: *const u8,
    format_data_len: usize,
    out: *mut SSMCFormatData,
) -> FFIResult {
    if format_data_ptr.is_null() || out.is_null() {
        return FFIResult::NullArgument;
    }

    // SAFETY: the pointer isn't null, and the caller guarantees it points
    // to `format_data_len` readable bytes.
    let format_data = unsafe {
        slice::from_raw_parts(format_data_ptr, format_data_len)
    };

    match parse_format_data(format_data) {
        Ok(data) => {
            // SAFETY: `out` isn't null, and the caller guarantees it's valid
            // for writes.
            unsafe { out.write(data) };
            FFIResult::StatusOk
        }
        Err(e) => e.into(),
    }
}

/// Shared body of `parse_ssmc_prefix_u64` and `parse_ssmc_prefix_u128`,
/// parsing hashes as `H` and handing them to C as `F`.
///
/// # Safety
///
/// `prefix_ptr` must be null or point to `prefix_len` readable bytes.
unsafe fn parse_ssmc_prefix_internal<H, F>(
    prefix_ptr: *const u8,
    prefix_len: usize,
) -> Result<FFIParsedSSMC<F>, FFIResult>
where
    for<'de> H: Eq + Hash + Deserialize<'de> + Decode<'de>,
    F: From<H>,
{
    if prefix_ptr.is_null() {
        return Err(FFIResult::NullArgument);
    }

    // SAFETY: the pointer isn't null, and the caller guarantees it points
    // to `prefix_len` readable bytes.
    let prefix = unsafe { slice::from_raw_parts(prefix_ptr, prefix_len) };
    let parsed = parse_ssmc_prefix::<H>(prefix)?;

    /*The only fallible step comes first, so nothing has been handed out
    yet if it fails.*/
    let toc = toc_into_ffi(&parsed.toc)?;
    let manifests = manifests_into_ffi::<H, F>(parsed.manifests);

    let (toc_ptr, toc_len, toc_cap) = vec_into_raw_parts(toc);
    let (manifests_ptr, manifests_len, manifests_cap) =
        vec_into_raw_parts(manifests);
    let (dictionary_ptr, dictionary_len, dictionary_cap) =
        vec_into_raw_parts(parsed.dictionary);
    let chunk_index: *mut ChunkIndexMap<H> =
        Box::into_raw(Box::new(parsed.chunk_index));

    Ok(FFIParsedSSMC {
        header: parsed.header,
        format_data: parsed.format_data,
        toc_ptr,
        toc_len,
        toc_cap,
        manifests_ptr,
        manifests_len,
        manifests_cap,
        dictionary_ptr,
        dictionary_len,
        dictionary_cap,
        chunk_index: chunk_index.cast::<c_void>(),
    })
}

/// Reads every part of an SSMC archive's metadata from its prefix, for
/// archives whose chunks are identified by `u64` hashes.
///
/// `prefix_ptr` holds the start of the archive, at least up to the format
/// data's `data_offset`; any bytes past that are ignored. See
/// `parse_format_data_ffi` for how to find that length.
///
/// On success, returns `FFIResult::StatusOk` and writes the result to
/// `out_ptr`. On failure, returns one of:
/// - `FFIResult::NullArgument` if a pointer is null.
/// - `FFIResult::InvalidHeader` if the header is malformed or the archive
///   isn't SSMC.
/// - `FFIResult::UnsupportedVersion` if the archive is newer than this
///   library.
/// - `FFIResult::SectionOutOfBounds` if a section lies past the end of the
///   prefix.
/// - `FFIResult::ManifestDecodeError` if the manifest fails to decode.
/// - `FFIResult::InternalError` for any other decode failure.
///
/// # Safety
/// - `prefix_ptr` must point to `prefix_len` readable bytes. They're only
///   read, and nothing returned points into them.
/// - `out_ptr` must be valid for writes.
/// - On success, the pointer written to `out_ptr` must be freed with
///   `free_parsed_ssmc_u64`.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks only #[must_use] functions SPRITE_SHRINK_MUST_USE"
)]
#[must_use]
pub unsafe extern "C" fn parse_ssmc_prefix_u64(
    prefix_ptr: *const u8,
    prefix_len: usize,
    out_ptr: *mut *mut FFIParsedSSMCU64,
) -> FFIResult {
    if out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    // SAFETY: the caller guarantees `prefix_ptr` is valid for
    // `prefix_len` bytes.
    match unsafe {
        parse_ssmc_prefix_internal::<u64, u64>(prefix_ptr, prefix_len)
    } {
        Ok(parsed) => {
            // SAFETY: `out_ptr` was checked for null above and the caller
            // guarantees it is valid for writes.
            unsafe { *out_ptr = Box::into_raw(Box::new(parsed)) };
            FFIResult::StatusOk
        }
        Err(status) => status,
    }
}

/// Reads every part of an SSMC archive's metadata from its prefix, for
/// archives whose chunks are identified by `u128` hashes.
///
/// `prefix_ptr` holds the start of the archive, at least up to the format
/// data's `data_offset`; any bytes past that are ignored. See
/// `parse_format_data_ffi` for how to find that length.
///
/// On success, returns `FFIResult::StatusOk` and writes the result to
/// `out_ptr`. On failure, returns one of:
/// - `FFIResult::NullArgument` if a pointer is null.
/// - `FFIResult::InvalidHeader` if the header is malformed or the archive
///   isn't SSMC.
/// - `FFIResult::UnsupportedVersion` if the archive is newer than this
///   library.
/// - `FFIResult::SectionOutOfBounds` if a section lies past the end of the
///   prefix.
/// - `FFIResult::ManifestDecodeError` if the manifest fails to decode.
/// - `FFIResult::InternalError` for any other decode failure.
///
/// # Safety
/// - `prefix_ptr` must point to `prefix_len` readable bytes. They're only
///   read, and nothing returned points into them.
/// - `out_ptr` must be valid for writes.
/// - On success, the pointer written to `out_ptr` must be freed with
///   `free_parsed_ssmc_u128`.
#[unsafe(no_mangle)]
#[expect(
    clippy::double_must_use,
    reason = "cbindgen marks only #[must_use] functions SPRITE_SHRINK_MUST_USE"
)]
#[must_use]
pub unsafe extern "C" fn parse_ssmc_prefix_u128(
    prefix_ptr: *const u8,
    prefix_len: usize,
    out_ptr: *mut *mut FFIParsedSSMCU128,
) -> FFIResult {
    if out_ptr.is_null() {
        return FFIResult::NullArgument;
    }

    // SAFETY: the caller guarantees `prefix_ptr` is valid for
    // `prefix_len` bytes.
    match unsafe {
        parse_ssmc_prefix_internal::<u128, U128Bytes>(prefix_ptr, prefix_len)
    } {
        Ok(parsed) => {
            // SAFETY: `out_ptr` was checked for null above and the caller
            // guarantees it is valid for writes.
            unsafe { *out_ptr = Box::into_raw(Box::new(parsed)) };
            FFIResult::StatusOk
        }
        Err(status) => status,
    }
}

/// Frees everything a `parse_ssmc_prefix_*` result owns, with `H` the hash
/// type its chunk index was parsed as.
///
/// # Safety
///
/// `ptr` must be non-null, come from the matching `parse_ssmc_prefix_*`
/// function, and not have been freed.
unsafe fn free_parsed_ssmc_internal<F, H>(ptr: *mut FFIParsedSSMC<F>) {
    // SAFETY: the caller guarantees `ptr` came from `Box::into_raw`.
    let parsed = unsafe { Box::from_raw(ptr) };

    // SAFETY: the parts came from `vec_into_raw_parts` on `toc_into_ffi`'s
    // result and are unchanged.
    unsafe { free_toc(parsed.toc_ptr, parsed.toc_len, parsed.toc_cap) };
    let manifests =
        (parsed.manifests_ptr, parsed.manifests_len, parsed.manifests_cap);
    // SAFETY: the parts came from `vec_into_raw_parts` on
    // `manifests_into_ffi`'s result and are unchanged.
    unsafe { free_manifests(manifests.0, manifests.1, manifests.2) };
    // SAFETY: the parts came from `vec_into_raw_parts` and are unchanged.
    drop(unsafe {
        Vec::from_raw_parts(
            parsed.dictionary_ptr,
            parsed.dictionary_len,
            parsed.dictionary_cap,
        )
    });
    let chunk_index = parsed.chunk_index.cast::<ChunkIndexMap<H>>();
    // SAFETY: the handle came from `Box::into_raw` on a `ChunkIndexMap<H>`.
    drop(unsafe { Box::from_raw(chunk_index) });
}

/// Frees a result from `parse_ssmc_prefix_u64`, including its
/// `chunk_index` handle.
///
/// Passing a null pointer does nothing.
///
/// # Safety
/// `ptr` must be null or a pointer written by `parse_ssmc_prefix_u64` that
/// has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_parsed_ssmc_u64(ptr: *mut FFIParsedSSMCU64) {
    if ptr.is_null() {
        return;
    }

    // SAFETY: `ptr` isn't null and, per the caller's guarantee, came from
    // `parse_ssmc_prefix_u64`.
    unsafe { free_parsed_ssmc_internal::<u64, u64>(ptr) };
}

/// Frees a result from `parse_ssmc_prefix_u128`, including its
/// `chunk_index` handle.
///
/// Passing a null pointer does nothing.
///
/// # Safety
/// `ptr` must be null or a pointer written by `parse_ssmc_prefix_u128` that
/// has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_parsed_ssmc_u128(ptr: *mut FFIParsedSSMCU128) {
    if ptr.is_null() {
        return;
    }

    // SAFETY: `ptr` isn't null and, per the caller's guarantee, came from
    // `parse_ssmc_prefix_u128`.
    unsafe { free_parsed_ssmc_internal::<U128Bytes, u128>(ptr) };
}
