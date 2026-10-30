//! Calls the C exports directly from Rust, the way a C host would, and
//! checks them against the Rust API.
//!
//! The callbacks hand the library buffers from Rust's allocator, as the
//! callback contract requires: the library takes them back with
//! `Vec::from_raw_parts`.

use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    fmt::{Debug, Display},
    hash::Hash,
    mem::ManuallyDrop,
    ptr, slice,
};

use bitcode::{Encode, encode};
use dashmap::DashMap;
use sprite_shrink::{
    ChunkLocation, FileManifestParent, SSAChunkMeta, SSMCTocEntry,
    SerializedData, SpriteShrinkError, serialize_uncompressed_data,
    ffi::{
        FFICallbackStatus, FFIChunkDataArray, FFIFileManifestParent,
        FFIKeyArray, FFISSAChunkMeta, FFISerializedOutput, FFIStatus,
        FFIVecBytes, SerializeUncompressedDataArgsU64,
        SerializeUncompressedDataArgsU128, U128Bytes,
        free_serialized_output_u64, free_serialized_output_u128,
        serialize_uncompressed_data_u64, serialize_uncompressed_data_u128,
    },
};

const CHUNKS: [&[u8]; 3] = [b"AAAA", b"BBB", b"CCCCC"];

/// Each file's name and the `CHUNKS` it's made of. They're out of name
/// order so the tests see the table of contents get sorted, two share a
/// chunk, and one is empty.
const FILES: [(&str, &[usize]); 3] = [
    ("b.bin", &[0, 1]),
    ("a.bin", &[0, 2]),
    ("c.bin", &[]),
];

/// The chunks a test host holds, keyed by hash. The callbacks get it
/// through `user_data`.
type ChunkStore<H> = HashMap<H, Vec<u8>>;

fn chunk_store<H: Copy + Eq + Hash>(hashes: [H; 3]) -> ChunkStore<H> {
    hashes.into_iter().zip(CHUNKS.map(<[u8]>::to_vec)).collect()
}

fn chunk_metadata<H: Copy>(hashes: [H; 3], chunks: &[usize])
    -> Vec<SSAChunkMeta<H>>
{
    let mut offset = 0;

    chunks
        .iter()
        .map(|&i| {
            let length = u32::try_from(CHUNKS[i].len()).unwrap();
            let meta = SSAChunkMeta { hash: hashes[i], offset, length };
            offset += u64::from(length);
            meta
        })
        .collect()
}

/// What `serialize_uncompressed_data` returns for `FILES`.
fn rust_serialize<H>(hashes: [H; 3]) -> SerializedData<H>
where
    H: Copy + Ord + Hash + Display + Send + Sync + 'static,
{
    let file_manifest = DashMap::new();
    for (name, chunks) in FILES {
        let chunk_metadata = chunk_metadata(hashes, chunks);
        file_manifest.insert(name.to_string(), FileManifestParent {
            chunk_count: chunk_metadata.len() as u64,
            chunk_metadata,
        });
    }

    let store = chunk_store(hashes);
    let keys: Vec<H> = store.keys().copied().collect();

    serialize_uncompressed_data(
        &file_manifest,
        &move || Ok::<_, SpriteShrinkError>(keys.clone()),
        &move |hashes: &[H]| {
            Ok(hashes.iter().map(|hash| store[hash].clone()).collect())
        },
    )
    .unwrap()
}

/// The names and manifests a C host passes in for `FILES`. The pointers in
/// `name_ptrs` and `manifests` point into the other fields' heap buffers,
/// which don't move when this struct does.
struct HostInput<F> {
    _names: Vec<CString>,
    name_ptrs: Vec<*const c_char>,
    _chunk_metadata: Vec<Vec<FFISSAChunkMeta<F>>>,
    manifests: Vec<FFIFileManifestParent<F>>,
}

fn host_input<H: Copy, F: From<H>>(hashes: [H; 3]) -> HostInput<F> {
    let names: Vec<CString> = FILES
        .iter()
        .map(|(name, _)| CString::new(*name).unwrap())
        .collect();

    let chunk_metadata: Vec<Vec<FFISSAChunkMeta<F>>> = FILES
        .iter()
        .map(|(_, chunks)| {
            chunk_metadata(hashes, chunks)
                .into_iter()
                .map(|meta| FFISSAChunkMeta {
                    hash: meta.hash.into(),
                    offset: meta.offset,
                    length: meta.length,
                })
                .collect()
        })
        .collect();

    HostInput {
        name_ptrs: names.iter().map(|name| name.as_ptr()).collect(),
        _names: names,
        manifests: chunk_metadata
            .iter()
            .map(|meta| FFIFileManifestParent {
                chunk_metadata: meta.as_ptr(),
                chunk_metadata_len: meta.len(),
                chunk_metadata_cap: meta.capacity(),
            })
            .collect(),
        _chunk_metadata: chunk_metadata,
    }
}

/// Splits a `Vec` into the parts a callback hands the library.
fn into_raw_parts<T>(vec: Vec<T>) -> (*mut T, usize, usize) {
    let mut vec = ManuallyDrop::new(vec);
    (vec.as_mut_ptr(), vec.len(), vec.capacity())
}

fn chunk_data_array(chunks: Vec<Vec<u8>>) -> FFIChunkDataArray {
    let chunks: Vec<FFIVecBytes> = chunks
        .into_iter()
        .map(|chunk| {
            let (ptr, len, cap) = into_raw_parts(chunk);
            FFIVecBytes { ptr, len, cap }
        })
        .collect();
    let (ptr, len, cap) = into_raw_parts(chunks);

    FFIChunkDataArray { ptr, len, cap }
}

unsafe extern "C" fn get_keys_u64(
    user_data: *mut c_void,
    out_keys: *mut FFIKeyArray<u64>,
) -> FFICallbackStatus {
    // SAFETY: the tests pass a `ChunkStore<u64>` as `user_data`.
    let store = unsafe { &*user_data.cast::<ChunkStore<u64>>() };
    let (ptr, len, cap) = into_raw_parts(store.keys().copied().collect());

    // SAFETY: the library passes a pointer that's valid for writes.
    unsafe { out_keys.write(FFIKeyArray { ptr, len, cap }) };
    FFICallbackStatus::CallbackOk
}

unsafe extern "C" fn get_chunks_u64(
    user_data: *mut c_void,
    hashes: *const u64,
    hashes_len: usize,
    out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    // SAFETY: the tests pass a `ChunkStore<u64>` as `user_data`.
    let store = unsafe { &*user_data.cast::<ChunkStore<u64>>() };
    // SAFETY: the library passes `hashes_len` hashes.
    let hashes = unsafe { slice::from_raw_parts(hashes, hashes_len) };
    let chunks = hashes.iter().map(|hash| store[hash].clone()).collect();

    // SAFETY: the library passes a pointer that's valid for writes.
    unsafe { out_chunks.write(chunk_data_array(chunks)) };
    FFICallbackStatus::CallbackOk
}

unsafe extern "C" fn get_keys_u128(
    user_data: *mut c_void,
    out_keys: *mut FFIKeyArray<U128Bytes>,
) -> FFICallbackStatus {
    // SAFETY: the tests pass a `ChunkStore<u128>` as `user_data`.
    let store = unsafe { &*user_data.cast::<ChunkStore<u128>>() };
    let keys = store.keys().map(|&hash| U128Bytes::from(hash)).collect();
    let (ptr, len, cap) = into_raw_parts(keys);

    // SAFETY: the library passes a pointer that's valid for writes.
    unsafe { out_keys.write(FFIKeyArray { ptr, len, cap }) };
    FFICallbackStatus::CallbackOk
}

unsafe extern "C" fn get_chunks_u128(
    user_data: *mut c_void,
    hashes: *const u8,
    hashes_len: usize,
    out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    // SAFETY: the tests pass a `ChunkStore<u128>` as `user_data`.
    let store = unsafe { &*user_data.cast::<ChunkStore<u128>>() };
    // SAFETY: the library passes `hashes_len` 16-byte hashes.
    let bytes = unsafe { slice::from_raw_parts(hashes, hashes_len * 16) };
    let chunks = bytes
        .as_chunks::<16>()
        .0
        .iter()
        .map(|hash| store[&u128::from_le_bytes(*hash)].clone())
        .collect();

    // SAFETY: the library passes a pointer that's valid for writes.
    unsafe { out_chunks.write(chunk_data_array(chunks)) };
    FFICallbackStatus::CallbackOk
}

unsafe extern "C" fn fail_chunks_u64(
    _user_data: *mut c_void,
    _hashes: *const u64,
    _hashes_len: usize,
    _out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    FFICallbackStatus::Error
}

unsafe extern "C" fn fail_chunks_u128(
    _user_data: *mut c_void,
    _hashes: *const u8,
    _hashes_len: usize,
    _out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    FFICallbackStatus::Error
}

/// Runs `serialize_uncompressed_data_u64` on `input`, after `edit` has
/// changed the arguments.
fn ffi_serialize_u64(
    input: &HostInput<u64>,
    store: &mut ChunkStore<u64>,
    edit: impl FnOnce(&mut SerializeUncompressedDataArgsU64),
) -> (FFIStatus, *mut FFISerializedOutput<u64>) {
    let mut args = SerializeUncompressedDataArgsU64 {
        manifest_array_ptr: input.manifests.as_ptr(),
        filenames: input.name_ptrs.as_ptr(),
        manifest_len: input.manifests.len(),
        user_data: ptr::from_mut(store).cast(),
        get_keys_cb: get_keys_u64,
        get_chunks_cb: get_chunks_u64,
    };
    edit(&mut args);
    let mut out = ptr::null_mut();

    // SAFETY: `args` points into `input` and `store`, which outlive the
    // call.
    let status = unsafe {
        serialize_uncompressed_data_u64(&raw const args, &raw mut out)
    };
    (status, out)
}

/// Runs `serialize_uncompressed_data_u128` on `input`, after `edit` has
/// changed the arguments.
fn ffi_serialize_u128(
    input: &HostInput<U128Bytes>,
    store: &mut ChunkStore<u128>,
    edit: impl FnOnce(&mut SerializeUncompressedDataArgsU128),
) -> (FFIStatus, *mut FFISerializedOutput<U128Bytes>) {
    let mut args = SerializeUncompressedDataArgsU128 {
        manifest_array_ptr: input.manifests.as_ptr(),
        filenames: input.name_ptrs.as_ptr(),
        manifest_len: input.manifests.len(),
        user_data: ptr::from_mut(store).cast(),
        get_keys_cb: get_keys_u128,
        get_chunks_cb: get_chunks_u128,
    };
    edit(&mut args);
    let mut out = ptr::null_mut();

    // SAFETY: `args` points into `input` and `store`, which outlive the
    // call.
    let status = unsafe {
        serialize_uncompressed_data_u128(&raw const args, &raw mut out)
    };
    (status, out)
}

/// Copies what `out` points to back into Rust types, converting each hash
/// with `H::from`.
///
/// # Safety
///
/// `out` must come from a successful `serialize_uncompressed_data_*` call
/// and not have been freed.
unsafe fn read_output<F, H>(out: *const FFISerializedOutput<F>)
    -> SerializedData<H>
where
    F: Copy,
    H: From<F> + Eq + Hash,
{
    // SAFETY: the caller guarantees `out` is valid.
    let out = unsafe { &*out };
    // SAFETY: each array holds `len` elements, as written by the library.
    let toc = unsafe { slice::from_raw_parts(out.toc_ptr, out.toc_len) };
    // SAFETY: as above.
    let manifests = unsafe {
        slice::from_raw_parts(out.ser_manifest_ptr, out.ser_manifest_len)
    };
    // SAFETY: as above.
    let chunk_index = unsafe {
        slice::from_raw_parts(out.ser_chunk_index_ptr, out.ser_chunk_index_len)
    };
    // SAFETY: as above.
    let sorted_hashes = unsafe {
        slice::from_raw_parts(out.sorted_hashes_ptr, out.sorted_hashes_len)
    };

    SerializedData {
        archive_toc: toc
            .iter()
            .map(|entry| SSMCTocEntry {
                // SAFETY: the library writes null-terminated filenames.
                filename: unsafe { CStr::from_ptr(entry.filename) }
                    .to_str()
                    .unwrap()
                    .to_string(),
                uncompressed_size: entry.uncompressed_size,
            })
            .collect(),
        ser_file_manifest: manifests
            .iter()
            .map(|fmp| {
                // SAFETY: as above.
                let metadata = unsafe {
                    slice::from_raw_parts(
                        fmp.chunk_metadata,
                        fmp.chunk_metadata_len,
                    )
                };
                FileManifestParent {
                    chunk_count: metadata.len() as u64,
                    chunk_metadata: metadata
                        .iter()
                        .map(|meta| SSAChunkMeta {
                            hash: meta.hash.into(),
                            offset: meta.offset,
                            length: meta.length,
                        })
                        .collect(),
                }
            })
            .collect(),
        chunk_index: chunk_index
            .iter()
            .map(|entry| {
                (entry.hash.into(), ChunkLocation {
                    offset: entry.data.offset,
                    compressed_length: entry.data.length,
                })
            })
            .collect(),
        sorted_hashes: sorted_hashes.iter().map(|&hash| hash.into()).collect(),
    }
}

fn sorted_index<H: Copy + Ord>(index: &HashMap<H, ChunkLocation>)
    -> Vec<(H, u64, u32)>
{
    let mut entries: Vec<_> = index
        .iter()
        .map(|(&hash, loc)| (hash, loc.offset, loc.compressed_length))
        .collect();
    entries.sort_unstable();
    entries
}

fn assert_matches_rust<H>(ffi: &SerializedData<H>, rust: &SerializedData<H>)
where
    H: Copy + Ord + Debug + Encode,
{
    assert_eq!(ffi.archive_toc, rust.archive_toc);
    assert_eq!(encode(&ffi.ser_file_manifest), encode(&rust.ser_file_manifest));
    assert_eq!(sorted_index(&ffi.chunk_index), sorted_index(&rust.chunk_index));
    assert_eq!(ffi.sorted_hashes, rust.sorted_hashes);

    // a.bin is AAAA + CCCCC and b.bin is AAAA + BBB.
    let toc: Vec<_> = ffi
        .archive_toc
        .iter()
        .map(|entry| (entry.filename.as_str(), entry.uncompressed_size))
        .collect();
    assert_eq!(toc, [("a.bin", 9), ("b.bin", 7), ("c.bin", 0)]);
}

fn status_code(status: FFIStatus) -> i32 {
    status as i32
}

#[test]
fn serialize_u64_matches_rust_api() {
    let hashes = [1_u64, u64::MAX, 7];
    let input = host_input::<u64, u64>(hashes);
    let mut store = chunk_store(hashes);

    let (status, out) = ffi_serialize_u64(&input, &mut store, |_| {});
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    // SAFETY: `out` came from the successful call above.
    let ffi = unsafe { read_output::<u64, u64>(out) };
    assert_matches_rust(&ffi, &rust_serialize(hashes));

    // SAFETY: `out` came from the call above and is freed once.
    unsafe { free_serialized_output_u64(out) };
}

#[test]
fn serialize_u128_matches_rust_api() {
    // Hashes that differ in their high bytes, so a byte order mix-up shows.
    let hashes = [1_u128, u128::MAX, 1 << 120];
    let input = host_input::<u128, U128Bytes>(hashes);
    let mut store = chunk_store(hashes);

    let (status, out) = ffi_serialize_u128(&input, &mut store, |_| {});
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    // SAFETY: `out` came from the successful call above.
    let ffi = unsafe { read_output::<U128Bytes, u128>(out) };
    assert_matches_rust(&ffi, &rust_serialize(hashes));

    // SAFETY: `out` came from the call above and is freed once.
    unsafe { free_serialized_output_u128(out) };
}

#[test]
fn serialize_rejects_null_filenames() {
    let hashes = [1_u64, 2, 3];
    let mut store = chunk_store(hashes);

    let input = host_input::<u64, u64>(hashes);
    let (status, out) = ffi_serialize_u64(&input, &mut store, |args| {
        args.filenames = ptr::null();
    });
    assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    assert!(out.is_null());

    let mut input = host_input::<u64, u64>(hashes);
    input.name_ptrs[1] = ptr::null();
    let (status, out) = ffi_serialize_u64(&input, &mut store, |_| {});
    assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    assert!(out.is_null());
}

#[test]
fn serialize_rejects_duplicate_filename() {
    let hashes = [1_u64, 2, 3];
    let mut store = chunk_store(hashes);
    let mut input = host_input::<u64, u64>(hashes);
    input.name_ptrs[2] = input.name_ptrs[0];

    let (status, out) = ffi_serialize_u64(&input, &mut store, |_| {});

    assert_eq!(status_code(status), status_code(FFIStatus::DuplicateFilename));
    assert!(out.is_null());
}

#[test]
fn serialize_rejects_filename_that_is_not_utf8() {
    let hashes = [1_u64, 2, 3];
    let mut store = chunk_store(hashes);
    let invalid = CString::new([0xff, 0xfe]).unwrap();
    let mut input = host_input::<u64, u64>(hashes);
    input.name_ptrs[0] = invalid.as_ptr();

    let (status, out) = ffi_serialize_u64(&input, &mut store, |_| {});

    assert_eq!(status_code(status), status_code(FFIStatus::InvalidString));
    assert!(out.is_null());
}

/// A failed chunk callback is reported as the host's error, not as a
/// missing chunk, and the u128 version no longer ignores the status and
/// reads the array the callback never filled in.
#[test]
fn serialize_reports_chunk_callback_error() {
    let hashes = [1_u64, 2, 3];
    let input = host_input::<u64, u64>(hashes);
    let mut store = chunk_store(hashes);
    let (status, out) = ffi_serialize_u64(&input, &mut store, |args| {
        args.get_chunks_cb = fail_chunks_u64;
    });
    assert_eq!(status_code(status), status_code(FFIStatus::InternalError));
    assert!(out.is_null());

    let hashes = [1_u128, 2, 3];
    let input = host_input::<u128, U128Bytes>(hashes);
    let mut store = chunk_store(hashes);
    let (status, out) = ffi_serialize_u128(&input, &mut store, |args| {
        args.get_chunks_cb = fail_chunks_u128;
    });
    assert_eq!(status_code(status), status_code(FFIStatus::InternalError));
    assert!(out.is_null());
}

#[test]
fn free_serialized_output_accepts_null() {
    // SAFETY: both free functions return early on null.
    unsafe { free_serialized_output_u64(ptr::null_mut()) };
    // SAFETY: as above.
    unsafe { free_serialized_output_u128(ptr::null_mut()) };
}
