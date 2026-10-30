//! Helpers shared by the FFI modules for handing Rust-owned data to C and
//! taking it back to free.

use std::{ffi::CString, mem::ManuallyDrop};

use crate::ffi::ffi_error_handling::FFIResult;
use crate::ffi::ffi_types::{
    FFIFileManifestParent, FFISSAChunkMeta, FFITocEntry,
};
use crate::lib_structs::{FileManifestParent, SSMCTocEntry};

/// Splits a `Vec` into the pointer, length and capacity handed to C. The
/// matching free function rebuilds it with `Vec::from_raw_parts`.
pub(crate) fn vec_into_raw_parts<T>(vec: Vec<T>) -> (*mut T, usize, usize) {
    let mut vec = ManuallyDrop::new(vec);
    (vec.as_mut_ptr(), vec.len(), vec.capacity())
}

/// Converts a table of contents into the entries handed to C. Free them
/// with `free_toc`.
///
/// # Errors
///
/// Returns `InvalidString` if a filename contains a null byte. That's
/// checked before any name is handed out, so there's nothing to free.
pub(crate) fn toc_into_ffi(
    toc: &[SSMCTocEntry]
) -> Result<Vec<FFITocEntry>, FFIResult> {
    let Ok(filenames) = toc
        .iter()
        .map(|entry| CString::new(entry.filename.as_str()))
        .collect::<Result<Vec<_>, _>>()
    else {
        return Err(FFIResult::InvalidString);
    };

    Ok(filenames
        .into_iter()
        .zip(toc)
        .map(|(filename, entry)| FFITocEntry {
            filename: filename.into_raw(),
            uncompressed_size: entry.uncompressed_size,
        })
        .collect())
}

/// Converts manifests into the form handed to C, converting each hash to
/// its FFI form `F`. Free them with `free_manifests`.
pub(crate) fn manifests_into_ffi<H, F>(
    manifests: Vec<FileManifestParent<H>>
) -> Vec<FFIFileManifestParent<F>>
where
    F: From<H>,
{
    manifests
        .into_iter()
        .map(|fmp| {
            let chunk_metadata: Vec<FFISSAChunkMeta<F>> = fmp.chunk_metadata
                .into_iter()
                .map(|meta| FFISSAChunkMeta {
                    hash: meta.hash.into(),
                    offset: meta.offset,
                    length: meta.length,
                })
                .collect();
            let (ptr, len, cap) = vec_into_raw_parts(chunk_metadata);

            FFIFileManifestParent {
                chunk_metadata: ptr,
                chunk_metadata_len: len,
                chunk_metadata_cap: cap,
            }
        })
        .collect()
}

/// Frees a table of contents from `toc_into_ffi`, including its names.
///
/// # Safety
///
/// The parts must come from `vec_into_raw_parts` on a `toc_into_ffi`
/// result, unchanged, and not have been freed.
pub(crate) unsafe fn free_toc(ptr: *mut FFITocEntry, len: usize, cap: usize) {
    // SAFETY: the caller guarantees the parts came from
    // `vec_into_raw_parts` and are unchanged.
    let toc = unsafe { Vec::from_raw_parts(ptr, len, cap) };

    for entry in toc {
        // SAFETY: each name came from `CString::into_raw` in `toc_into_ffi`.
        drop(unsafe { CString::from_raw(entry.filename) });
    }
}

/// Frees manifests from `manifests_into_ffi`, including their chunk
/// metadata.
///
/// # Safety
///
/// The parts must come from `vec_into_raw_parts` on a `manifests_into_ffi`
/// result, unchanged, and not have been freed.
pub(crate) unsafe fn free_manifests<F>(
    ptr: *mut FFIFileManifestParent<F>,
    len: usize,
    cap: usize,
) {
    // SAFETY: the caller guarantees the parts came from
    // `vec_into_raw_parts` and are unchanged.
    let manifests = unsafe { Vec::from_raw_parts(ptr, len, cap) };

    for fmp in manifests {
        // SAFETY: each array came from `vec_into_raw_parts` in
        // `manifests_into_ffi`.
        drop(unsafe {
            Vec::from_raw_parts(
                fmp.chunk_metadata.cast_mut(),
                fmp.chunk_metadata_len,
                fmp.chunk_metadata_cap,
            )
        });
    }
}
