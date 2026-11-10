//! Helpers shared by the FFI modules for handing Rust-owned data to C and
//! taking it back to free.

use std::{
    ffi::{CString, c_void},
    mem::ManuallyDrop,
    ptr,
    slice,
};

use crate::ffi::ffi_error_handling::{FFICallbackStatus, FFIResult};
use crate::ffi::ffi_types::{
    FFIChunkDataArray, FFIFileManifestParent, FFISSAChunkMeta, FFITocEntry,
    FFIVecBytes,
};
use crate::lib_error_handling::SpriteShrinkError;
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

/// The free callback every chunk-fetching export takes.
pub(crate) type FreeChunksCb = unsafe extern "C" fn(
    user_data: *mut c_void,
    chunks: FFIChunkDataArray,
);

/// Asks the host for `expected` chunks, copies them, and gives the host's
/// array back to `free`.
///
/// `get` calls the host's chunk callback with the out-pointer it's given.
/// When the callback succeeds and writes a non-null array, `free` is called
/// once with that array, after the copy, on this thread, even if the array
/// is rejected. When the callback fails, or writes a null array, `free`
/// isn't called: the host keeps whatever it allocated.
///
/// The host's memory is never freed here, so it can come from any
/// allocator.
///
/// # Errors
///
/// - `Cancelled`, or `External` for any other failure the callback reports.
/// - `External` if the array is null when chunks were asked for, holds a
///   different number of chunks than `expected`, or holds a null buffer with
///   a non-zero length.
///
/// # Safety
///
/// When `get` reports success, the array it wrote must be null, or point to
/// `len` initialised, aligned `FFIVecBytes`. Each of those must be null, or
/// point to `len` readable bytes. All of it must stay valid until `free`
/// returns. `free` must be safe to call with `user_data` and that array.
pub(crate) unsafe fn copy_host_chunks(
    expected: usize,
    get: impl FnOnce(*mut FFIChunkDataArray) -> FFICallbackStatus,
    free: FreeChunksCb,
    user_data: *mut c_void,
) -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
    let mut array = FFIChunkDataArray {
        ptr: ptr::null_mut(),
        len: 0,
        cap: 0,
    };
    Result::from(get(&raw mut array))?;

    if array.ptr.is_null() {
        return if expected == 0 && array.len == 0 {
            Ok(Vec::new())
        } else {
            Err(callback_error(format!(
                "the chunk callback returned no array for {expected} hashes"
            )))
        };
    }

    let chunks = if array.len == expected {
        // SAFETY: `array.ptr` isn't null, and the caller guarantees it
        // points to `array.len` aligned, initialised entries, each null or
        // pointing to its `len` readable bytes, valid until `free` runs
        // below.
        unsafe { copy_entries(&array) }
    } else {
        Err(callback_error(format!(
            "the chunk callback returned {} chunks for {expected} hashes",
            array.len
        )))
    };

    // SAFETY: the callback succeeded and wrote this non-null array, and the
    // caller guarantees `free` accepts it with `user_data`. Nothing reads
    // the array after this: every chunk was copied above.
    unsafe { free(user_data, array) };
    chunks
}

/// Copies every buffer in `array` into a `Vec`.
///
/// # Safety
///
/// `array.ptr` must point to `array.len` initialised, aligned
/// `FFIVecBytes`, each null or pointing to its `len` readable bytes.
unsafe fn copy_entries(
    array: &FFIChunkDataArray,
) -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
    // SAFETY: the caller guarantees `array.ptr` points to `array.len`
    // initialised, aligned entries.
    let entries: &[FFIVecBytes] = unsafe {
        slice::from_raw_parts(array.ptr, array.len)
    };

    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            if entry.ptr.is_null() {
                return if entry.len == 0 {
                    Ok(Vec::new())
                } else {
                    Err(callback_error(format!(
                        "the chunk callback returned a null buffer of {} \
                        bytes for chunk {index}",
                        entry.len
                    )))
                };
            }
            // SAFETY: `entry.ptr` isn't null, and the caller guarantees it
            // points to `entry.len` readable bytes. `u8` has an alignment
            // of 1, so any address is aligned for it.
            Ok(unsafe { slice::from_raw_parts(entry.ptr, entry.len) }.to_vec())
        })
        .collect()
}

/// A host callback broke its contract. Reported as `CallbackError`.
fn callback_error(message: String) -> SpriteShrinkError {
    SpriteShrinkError::External(message.into())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::ffi::ffi_error_handling::FFIStatus;

    /// A host that lends its chunks and counts the arrays given back.
    struct Host {
        chunks: Vec<Vec<u8>>,
        freed: AtomicUsize,
    }

    impl Host {
        fn new() -> Self {
            Self {
                chunks: vec![b"one".to_vec(), Vec::new(), b"three".to_vec()],
                freed: AtomicUsize::new(0),
            }
        }

        fn user_data(&self) -> *mut c_void {
            ptr::from_ref(self).cast_mut().cast()
        }

        /// The host's chunks as entries pointing into `self.chunks`.
        fn entries(&self) -> Vec<FFIVecBytes> {
            self.chunks
                .iter()
                .map(|chunk| FFIVecBytes {
                    ptr: chunk.as_ptr().cast_mut(),
                    len: chunk.len(),
                    cap: chunk.len(),
                })
                .collect()
        }
    }

    /// Frees only the array. Its entries point into the host's chunks.
    unsafe extern "C" fn free_array(
        user_data: *mut c_void,
        chunks: FFIChunkDataArray,
    ) {
        // SAFETY: these tests pass a `Host` as `user_data`.
        let host = unsafe { &*user_data.cast::<Host>() };
        host.freed.fetch_add(1, Ordering::Relaxed);
        // SAFETY: every array these tests return came from
        // `vec_into_raw_parts` and is freed only here.
        drop(unsafe { Vec::from_raw_parts(chunks.ptr, chunks.len, chunks.cap) });
    }

    /// Runs `copy_host_chunks` with a callback that writes `entries` and
    /// returns `status`.
    fn copy(
        host: &Host,
        expected: usize,
        entries: Option<Vec<FFIVecBytes>>,
        status: FFICallbackStatus,
    ) -> Result<Vec<Vec<u8>>, SpriteShrinkError> {
        let get = |out: *mut FFIChunkDataArray| {
            if let Some(entries) = entries {
                let (ptr, len, cap) = vec_into_raw_parts(entries);
                // SAFETY: `copy_host_chunks` passes a pointer to a local
                // array, valid for writes.
                unsafe { out.write(FFIChunkDataArray { ptr, len, cap }) };
            }
            status
        };
        // SAFETY: `get` writes an array from `vec_into_raw_parts` whose
        // entries point into `host.chunks`, which outlives the call, and
        // `free_array` frees exactly such an array.
        unsafe { copy_host_chunks(expected, get, free_array, host.user_data()) }
    }

    /// Checks that `result` failed with what a C host sees as
    /// `CallbackError`.
    fn assert_callback_error(result: Result<Vec<Vec<u8>>, SpriteShrinkError>) {
        match result {
            Err(error) => assert_eq!(
                FFIStatus::from(error) as i32,
                FFIStatus::CallbackError as i32
            ),
            Ok(chunks) => panic!("expected a callback error, got {chunks:?}"),
        }
    }

    #[test]
    fn copies_every_chunk_and_gives_the_array_back() {
        let host = Host::new();
        let chunks =
            copy(&host, 3, Some(host.entries()), FFICallbackStatus::CallbackOk);

        assert_eq!(chunks.unwrap(), host.chunks);
        assert_eq!(host.freed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_null_buffer_with_no_bytes_is_an_empty_chunk() {
        let host = Host::new();
        let mut entries = host.entries();
        entries[1].ptr = ptr::null_mut();

        let chunks = copy(&host, 3, Some(entries), FFICallbackStatus::CallbackOk);

        assert_eq!(chunks.unwrap(), host.chunks);
        assert_eq!(host.freed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_null_buffer_with_bytes_is_rejected_and_the_array_given_back() {
        let host = Host::new();
        let mut entries = host.entries();
        entries[2].ptr = ptr::null_mut();

        assert_callback_error(copy(
            &host, 3, Some(entries), FFICallbackStatus::CallbackOk,
        ));
        assert_eq!(host.freed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_wrong_chunk_count_is_rejected_and_the_array_given_back() {
        let host = Host::new();

        for expected in [2, 4] {
            assert_callback_error(copy(
                &host, expected, Some(host.entries()),
                FFICallbackStatus::CallbackOk,
            ));
        }
        assert_eq!(host.freed.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_null_array_is_rejected_unless_nothing_was_asked_for() {
        let host = Host::new();

        assert_callback_error(copy(&host, 3, None, FFICallbackStatus::CallbackOk));
        assert_eq!(
            copy(&host, 0, None, FFICallbackStatus::CallbackOk).unwrap(),
            Vec::<Vec<u8>>::new()
        );
        assert_eq!(host.freed.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_failed_callback_keeps_its_array() {
        let host = Host::new();

        assert_callback_error(copy(&host, 0, None, FFICallbackStatus::Error));
        assert!(matches!(
            copy(&host, 0, None, FFICallbackStatus::Cancelled),
            Err(SpriteShrinkError::Cancelled)
        ));
        assert_eq!(host.freed.load(Ordering::Relaxed), 0);
    }
}
