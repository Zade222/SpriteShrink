//! FFI-safe parsing functions for sprite-shrink archives.
//!
//! This module exposes Rust's parsing logic to C callers, handling the
//! conversion of data types and memory management across the FFI boundary.

use std::slice;

use crate::ffi::FFIResult;
use crate::lib_structs::FileHeader;
use crate::parsing::parse_file_header;

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
