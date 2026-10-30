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
    HASH_TYPE_XXH3_64, HASH_TYPE_XXH3_128, SSMC_UID, SUPPORTED_VERSION,
    ChunkLocation, CompressionResult, FileHeader, FileManifestParent,
    ParsedSSMC, SSAChunkMeta, SSMCFormatData, SSMCTocEntry, SerializedData,
    SpriteShrinkError, build_ssmc_prefix, parse_file_header,
    parse_ssmc_prefix, serialize_uncompressed_data,
    ffi::{
        FFICallbackStatus, FFIChunkDataArray, FFIChunkLocation, FFICompData,
        FFIFileManifestParent, FFIKeyArray, FFIParsedSSMC, FFISSAChunkMeta,
        FFISerializedOutput, FFIStatus, FFITocEntry, FFIVecBytes,
        SerializeUncompressedDataArgsU64, SerializeUncompressedDataArgsU128,
        U128Bytes,
        build_ssmc_prefix_u64, build_ssmc_prefix_u128, free_file_header,
        free_parsed_ssmc_u64, free_parsed_ssmc_u128,
        free_serialized_output_u64, free_serialized_output_u128,
        free_ssmc_prefix, lookup_chunk_location_u64,
        lookup_chunk_location_u128, parse_file_header_ffi,
        parse_format_data_ffi, parse_ssmc_prefix_u64, parse_ssmc_prefix_u128,
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

/// Copies a table of contents the library wrote back into Rust types.
///
/// # Safety
///
/// `ptr` must point to `len` entries the library wrote, not yet freed.
unsafe fn read_toc(ptr: *const FFITocEntry, len: usize) -> Vec<SSMCTocEntry> {
    // SAFETY: the caller guarantees `ptr` holds `len` entries.
    let toc = unsafe { slice::from_raw_parts(ptr, len) };

    toc.iter()
        .map(|entry| SSMCTocEntry {
            // SAFETY: the library writes null-terminated filenames.
            filename: unsafe { CStr::from_ptr(entry.filename) }
                .to_str()
                .unwrap()
                .to_string(),
            uncompressed_size: entry.uncompressed_size,
        })
        .collect()
}

/// Copies manifests the library wrote back into Rust types, converting
/// each hash with `H::from`.
///
/// # Safety
///
/// `ptr` must point to `len` manifests the library wrote, not yet freed.
unsafe fn read_manifests<F, H>(
    ptr: *const FFIFileManifestParent<F>,
    len: usize,
) -> Vec<FileManifestParent<H>>
where
    F: Copy,
    H: From<F>,
{
    // SAFETY: the caller guarantees `ptr` holds `len` manifests.
    let manifests = unsafe { slice::from_raw_parts(ptr, len) };

    manifests
        .iter()
        .map(|fmp| {
            // SAFETY: each manifest holds `chunk_metadata_len` entries.
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
        .collect()
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
    // SAFETY: the library wrote `ser_chunk_index_len` entries at
    // `ser_chunk_index_ptr`, and `out` hasn't been freed.
    let chunk_index = unsafe {
        slice::from_raw_parts(out.ser_chunk_index_ptr, out.ser_chunk_index_len)
    };
    // SAFETY: the library wrote `sorted_hashes_len` hashes at
    // `sorted_hashes_ptr`, and `out` hasn't been freed.
    let sorted_hashes = unsafe {
        slice::from_raw_parts(out.sorted_hashes_ptr, out.sorted_hashes_len)
    };

    SerializedData {
        // SAFETY: the library wrote `toc_len` entries with null-terminated
        // names at `toc_ptr`, and `out` hasn't been freed.
        archive_toc: unsafe { read_toc(out.toc_ptr, out.toc_len) },
        // SAFETY: the library wrote `ser_manifest_len` manifests at
        // `ser_manifest_ptr`, and `out` hasn't been freed.
        ser_file_manifest: unsafe {
            read_manifests(out.ser_manifest_ptr, out.ser_manifest_len)
        },
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

fn header_bytes() -> [u8; 36] {
    FileHeader::build_file_header(3, 98, 1, SSMC_UID, 40)
        .unwrap()
        .to_le_bytes()
}

fn ffi_parse_header(bytes: &[u8]) -> (FFIStatus, *mut FileHeader) {
    let mut out = ptr::null_mut();
    // SAFETY: `bytes` is `bytes.len()` readable bytes and `out` is a local.
    let status = unsafe {
        parse_file_header_ffi(bytes.as_ptr(), bytes.len(), &raw mut out)
    };
    (status, out)
}

#[test]
fn parse_file_header_matches_rust_api() {
    let bytes = header_bytes();
    let (status, out) = ffi_parse_header(&bytes);
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    // SAFETY: on success `out` points to a header from `Box::into_raw`.
    let header = unsafe { *out };
    assert_eq!(header, parse_file_header(&bytes).unwrap());
    // SAFETY: `out` came from `parse_file_header_ffi` and is freed once.
    unsafe { free_file_header(out) };
}

#[test]
fn parse_file_header_rejects_null_arguments() {
    let bytes = header_bytes();
    let mut out = ptr::null_mut();
    // SAFETY: the function returns before reading through a null pointer.
    let status = unsafe {
        parse_file_header_ffi(ptr::null(), bytes.len(), &raw mut out)
    };
    assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    assert!(out.is_null());

    // SAFETY: as above.
    let status = unsafe {
        parse_file_header_ffi(bytes.as_ptr(), bytes.len(), ptr::null_mut())
    };
    assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
}

#[test]
fn parse_file_header_rejects_wrong_length() {
    let bytes = header_bytes();
    let mut longer = bytes.to_vec();
    longer.push(0);

    for input in [&bytes[..bytes.len() - 1], &longer[..]] {
        let (status, out) = ffi_parse_header(input);
        assert_eq!(
            status_code(status),
            status_code(FFIStatus::InvalidHeader),
            "length {}",
            input.len()
        );
        assert!(out.is_null());
    }
}

#[test]
fn parse_file_header_rejects_wrong_magic_number() {
    let mut bytes = header_bytes();
    bytes[0] ^= 0xff;

    let (status, out) = ffi_parse_header(&bytes);
    assert_eq!(status_code(status), status_code(FFIStatus::InvalidHeader));
    assert!(out.is_null());
}

#[test]
fn parse_file_header_rejects_newer_version() {
    let mut header = FileHeader::build_file_header(3, 98, 1, SSMC_UID, 40)
        .unwrap();
    header.file_version = SUPPORTED_VERSION + 1;

    let (status, out) = ffi_parse_header(&header.to_le_bytes());
    assert_eq!(
        status_code(status),
        status_code(FFIStatus::UnsupportedVersion)
    );
    assert!(out.is_null());
}

#[test]
fn free_file_header_accepts_null() {
    // SAFETY: `free_file_header` returns early on null.
    unsafe { free_file_header(ptr::null_mut()) };
}

/// The signature shared by `build_ssmc_prefix_u64` and
/// `build_ssmc_prefix_u128`.
type BuildPrefixFn<F> = unsafe extern "C" fn(
    *const FFITocEntry,
    *const FFIFileManifestParent<F>,
    usize,
    *const FFICompData,
    *mut *mut FFIVecBytes,
) -> FFIStatus;

/// Stands in for what `archive_builder_build_*` returns. The prefix builder
/// only copies these bytes, so they don't have to decode.
fn comp_result() -> CompressionResult {
    let dictionary = b"dictionary bytes".to_vec();
    let enc_chunk_index = b"encoded chunk index".to_vec();

    CompressionResult {
        dictionary_size: dictionary.len() as u64,
        dictionary,
        enc_chunk_index_size: enc_chunk_index.len() as u64,
        enc_chunk_index,
    }
}

/// Points an `FFICompData` at `comp`'s buffers, which the library only
/// reads.
fn ffi_comp_data(comp: &mut CompressionResult) -> FFICompData {
    FFICompData {
        dictionary_data_ptr: comp.dictionary.as_mut_ptr(),
        dictionary_data_len: comp.dictionary.len(),
        dictionary_data_cap: comp.dictionary.capacity(),
        enc_chunk_index_data_ptr: comp.enc_chunk_index.as_mut_ptr(),
        enc_chunk_index_data_len: comp.enc_chunk_index.len(),
        enc_chunk_index_data_cap: comp.enc_chunk_index.capacity(),
    }
}

/// A table of contents a C host builds itself, naming `input`'s files.
fn host_toc<F>(input: &HostInput<F>) -> Vec<FFITocEntry> {
    input
        .name_ptrs
        .iter()
        .map(|&name| FFITocEntry {
            filename: name.cast_mut(),
            uncompressed_size: 0,
        })
        .collect()
}

/// Copies the prefix out and frees it.
///
/// # Safety
///
/// `prefix` must come from a successful `build_ssmc_prefix_*` call and not
/// have been freed.
unsafe fn take_prefix(prefix: *mut FFIVecBytes) -> Vec<u8> {
    // SAFETY: the caller guarantees `prefix` is valid.
    let parts = unsafe { &*prefix };
    // SAFETY: the library wrote `len` bytes at `ptr`.
    let bytes = unsafe { slice::from_raw_parts(parts.ptr, parts.len) }.to_vec();
    // SAFETY: `prefix` is freed once, here.
    unsafe { free_ssmc_prefix(prefix) };
    bytes
}

/// Feeds a serializer result back in the way a host would. Returns that
/// prefix and what `build_ssmc_prefix` gives with `hash_type` for the same
/// files.
///
/// # Safety
///
/// `ser` must come from a successful `serialize_uncompressed_data_*` call
/// and not have been freed.
unsafe fn ffi_and_rust_prefixes<F, H>(
    build: BuildPrefixFn<F>,
    ser: *const FFISerializedOutput<F>,
    hashes: [H; 3],
    hash_type: u8,
) -> (Vec<u8>, Vec<u8>)
where
    H: Copy + Ord + Hash + Display + Encode + Send + Sync + 'static,
{
    let mut comp = comp_result();
    let ffi_comp = ffi_comp_data(&mut comp);
    let mut out = ptr::null_mut();

    // SAFETY: the caller guarantees `ser` is valid.
    let ser = unsafe { &*ser };
    // SAFETY: the TOC and manifests both hold `toc_len` elements.
    let status = unsafe {
        build(
            ser.toc_ptr,
            ser.ser_manifest_ptr,
            ser.toc_len,
            &raw const ffi_comp,
            &raw mut out,
        )
    };
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    let rust = rust_serialize(hashes);
    let expected = build_ssmc_prefix(
        &rust.archive_toc,
        &rust.ser_file_manifest,
        &comp,
        hash_type,
    )
    .unwrap();

    // SAFETY: `out` came from the successful call above.
    (unsafe { take_prefix(out) }, expected)
}

#[test]
fn build_ssmc_prefix_u64_matches_rust_api() {
    let hashes = [1_u64, u64::MAX, 7];
    let input = host_input::<u64, u64>(hashes);
    let mut store = chunk_store(hashes);
    let (status, ser) = ffi_serialize_u64(&input, &mut store, |_| {});
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    // SAFETY: `ser` came from the successful call above.
    let (ffi, rust) = unsafe {
        ffi_and_rust_prefixes(
            build_ssmc_prefix_u64,
            ser,
            hashes,
            HASH_TYPE_XXH3_64,
        )
    };
    assert_eq!(ffi, rust);
    // SAFETY: `ser` is freed once, here.
    unsafe { free_serialized_output_u64(ser) };
}

#[test]
fn build_ssmc_prefix_u128_matches_rust_api() {
    let hashes = [1_u128, u128::MAX, 1 << 120];
    let input = host_input::<u128, U128Bytes>(hashes);
    let mut store = chunk_store(hashes);
    let (status, ser) = ffi_serialize_u128(&input, &mut store, |_| {});
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    // SAFETY: `ser` came from the successful call above.
    let (ffi, rust) = unsafe {
        ffi_and_rust_prefixes(
            build_ssmc_prefix_u128,
            ser,
            hashes,
            HASH_TYPE_XXH3_128,
        )
    };
    assert_eq!(ffi, rust);
    // SAFETY: `ser` is freed once, here.
    unsafe { free_serialized_output_u128(ser) };
}

/// Changes the TOC or build result before a prefix call.
type PrefixEdit = fn(&mut Vec<FFITocEntry>, &mut FFICompData);

/// Calls `build` on a host-built TOC, `input`'s manifests and `comp`, after
/// `edit` has changed the TOC and build result.
fn ffi_build_prefix<F>(
    build: BuildPrefixFn<F>,
    input: &HostInput<F>,
    edit: impl FnOnce(&mut Vec<FFITocEntry>, &mut FFICompData),
) -> (FFIStatus, *mut FFIVecBytes) {
    let mut comp = comp_result();
    let mut ffi_comp = ffi_comp_data(&mut comp);
    let mut toc = host_toc(input);
    edit(&mut toc, &mut ffi_comp);
    let mut out = ptr::null_mut();

    // SAFETY: every pointer points into a live local or `input`, each array
    // holds `input.manifests.len()` elements.
    let status = unsafe {
        build(
            toc.as_ptr(),
            input.manifests.as_ptr(),
            input.manifests.len(),
            &raw const ffi_comp,
            &raw mut out,
        )
    };
    (status, out)
}

fn assert_rejects_null_arguments<F>(
    build: BuildPrefixFn<F>,
    input: &HostInput<F>,
) {
    let cases: [(&str, PrefixEdit); 3] = [
        ("TOC filename", |toc, _| toc[1].filename = ptr::null_mut()),
        ("dictionary", |_, comp| comp.dictionary_data_ptr = ptr::null_mut()),
        ("chunk index", |_, comp| {
            comp.enc_chunk_index_data_ptr = ptr::null_mut();
        }),
    ];
    for (what, edit) in cases {
        let (status, out) = ffi_build_prefix(build, input, edit);
        assert_eq!(
            status_code(status),
            status_code(FFIStatus::NullArgument),
            "null {what}"
        );
        assert!(out.is_null(), "null {what}");
    }

    let mut comp = comp_result();
    let ffi_comp = ffi_comp_data(&mut comp);
    let host_toc = host_toc(input);
    let mut out_slot = ptr::null_mut();

    let toc = host_toc.as_ptr();
    let manifests = input.manifests.as_ptr();
    let comp_data = &raw const ffi_comp;
    let out = &raw mut out_slot;
    let calls = [
        ("toc", ptr::null(), manifests, comp_data, out),
        ("manifests", toc, ptr::null(), comp_data, out),
        ("comp_data", toc, manifests, ptr::null(), out),
        ("out_ptr", toc, manifests, comp_data, ptr::null_mut()),
    ];
    for (what, toc, manifests, comp_data, out) in calls {
        // SAFETY: one pointer is null, which the function checks before
        // reading anything; the others point into live locals or `input`.
        let status = unsafe {
            build(toc, manifests, input.manifests.len(), comp_data, out)
        };
        assert_eq!(
            status_code(status),
            status_code(FFIStatus::NullArgument),
            "null {what}"
        );
    }
    assert!(out_slot.is_null());
}

#[test]
fn build_ssmc_prefix_rejects_null_arguments() {
    let hashes = [1_u64, 2, 3];
    assert_rejects_null_arguments(build_ssmc_prefix_u64, &host_input(hashes));

    let hashes = [1_u128, 2, 3];
    assert_rejects_null_arguments(build_ssmc_prefix_u128, &host_input(hashes));
}

#[test]
fn build_ssmc_prefix_rejects_filename_that_is_not_utf8() {
    let input = host_input::<u64, u64>([1, 2, 3]);
    let bad_name = CString::new([0xff, 0xfe]).unwrap();

    let (status, out) =
        ffi_build_prefix(build_ssmc_prefix_u64, &input, |toc, _| {
            toc[0].filename = bad_name.as_ptr().cast_mut();
        });
    assert_eq!(status_code(status), status_code(FFIStatus::InvalidString));
    assert!(out.is_null());
}

#[test]
fn free_ssmc_prefix_accepts_null() {
    // SAFETY: `free_ssmc_prefix` returns early on null.
    unsafe { free_ssmc_prefix(ptr::null_mut()) };
}

/// Stands in for the compressed data after the prefix. The parser has to
/// ignore it.
const DATA_AFTER_PREFIX: &[u8] = b"compressed chunks";

/// An archive for `FILES` built with the Rust API: the prefix, with a chunk
/// index that decodes, then `DATA_AFTER_PREFIX`.
fn rust_archive<H>(hashes: [H; 3], hash_type: u8) -> Vec<u8>
where
    H: Copy + Ord + Hash + Display + Encode + Send + Sync + 'static,
{
    let ser = rust_serialize(hashes);
    // Encoded as pairs, which decode as the map the parser reads.
    let chunk_index: Vec<(H, ChunkLocation)> =
        ser.chunk_index.into_iter().collect();
    let enc_chunk_index = encode(&chunk_index);
    let comp = CompressionResult {
        enc_chunk_index_size: enc_chunk_index.len() as u64,
        enc_chunk_index,
        ..comp_result()
    };

    let mut archive = build_ssmc_prefix(
        &ser.archive_toc,
        &ser.ser_file_manifest,
        &comp,
        hash_type,
    )
    .unwrap();
    archive.extend_from_slice(DATA_AFTER_PREFIX);
    archive
}

/// Finds where `archive`'s metadata ends the way a C host would: parse the
/// header, then the format data it points to.
fn ffi_prefix_len(archive: &[u8]) -> usize {
    let (status, header) = ffi_parse_header(&archive[..36]);
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));
    // SAFETY: `header` came from the successful call above.
    let start = unsafe { (*header).format_data_offset } as usize;
    // SAFETY: `header` is freed once, here.
    unsafe { free_file_header(header) };

    let bytes = &archive[start..start + size_of::<SSMCFormatData>()];
    let mut format_data = SSMCFormatData::from_le_bytes(&[0; 56]);
    // SAFETY: `bytes` is `bytes.len()` readable bytes and `format_data` is
    // a local.
    let status = unsafe {
        parse_format_data_ffi(bytes.as_ptr(), bytes.len(), &raw mut format_data)
    };
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    usize::try_from(format_data.data_offset).unwrap()
}

/// Checks a parse result against `parse_ssmc_prefix` for the same bytes.
/// `lookup` looks a hash up in the result's `chunk_index`, and `absent` is
/// a hash that isn't in it.
///
/// # Safety
///
/// `parsed` must come from a successful `parse_ssmc_prefix_*` call and not
/// have been freed.
unsafe fn assert_parsed_matches_rust<F, H>(
    parsed: *const FFIParsedSSMC<F>,
    rust: &ParsedSSMC<H>,
    lookup: impl Fn(*const c_void, H) -> (FFIStatus, FFIChunkLocation),
    absent: H,
) where
    F: Copy,
    H: Copy + From<F> + Encode,
{
    // SAFETY: the caller guarantees `parsed` is valid.
    let parsed = unsafe { &*parsed };
    assert_eq!(parsed.header, rust.header);
    assert_eq!(parsed.format_data, rust.format_data);

    // SAFETY: the library wrote `toc_len` entries with null-terminated
    // names at `toc_ptr`, and `parsed` hasn't been freed.
    let toc = unsafe { read_toc(parsed.toc_ptr, parsed.toc_len) };
    assert_eq!(toc, rust.toc);
    // SAFETY: the library wrote `manifests_len` manifests at
    // `manifests_ptr`, and `parsed` hasn't been freed.
    let manifests: Vec<FileManifestParent<H>> = unsafe {
        read_manifests(parsed.manifests_ptr, parsed.manifests_len)
    };
    assert_eq!(encode(&manifests), encode(&rust.manifests));
    // SAFETY: the library wrote `dictionary_len` bytes at `dictionary_ptr`,
    // and `parsed` hasn't been freed.
    let dictionary = unsafe {
        slice::from_raw_parts(parsed.dictionary_ptr, parsed.dictionary_len)
    };
    assert_eq!(dictionary, rust.dictionary);

    assert_eq!(rust.chunk_index.len(), CHUNKS.len());
    for (&hash, location) in &rust.chunk_index {
        let (status, found) = lookup(parsed.chunk_index, hash);
        assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));
        assert_eq!(found.offset, location.offset);
        assert_eq!(found.length, location.compressed_length);
    }
    let (status, _) = lookup(parsed.chunk_index, absent);
    assert_eq!(status_code(status), status_code(FFIStatus::HashNotFound));
}

fn lookup_u64(map: *const c_void, hash: u64) -> (FFIStatus, FFIChunkLocation) {
    let mut location = FFIChunkLocation { offset: 0, length: 0 };
    // SAFETY: `map` is a live `chunk_index` handle for `u64` hashes and
    // `location` is a local.
    let status = unsafe {
        lookup_chunk_location_u64(map, hash, &raw mut location)
    };
    (status, location)
}

fn lookup_u128(map: *const c_void, hash: u128)
    -> (FFIStatus, FFIChunkLocation)
{
    let key = hash.to_le_bytes();
    let mut location = FFIChunkLocation { offset: 0, length: 0 };
    // SAFETY: `map` is a live `chunk_index` handle for `u128` hashes, `key`
    // is 16 bytes and `location` is a local.
    let status = unsafe {
        lookup_chunk_location_u128(map, key.as_ptr(), &raw mut location)
    };
    (status, location)
}

#[test]
fn parse_ssmc_prefix_u64_matches_rust_api() {
    let archive = rust_archive([1_u64, u64::MAX, 7], HASH_TYPE_XXH3_64);
    let prefix = &archive[..ffi_prefix_len(&archive)];
    assert_eq!(prefix.len(), archive.len() - DATA_AFTER_PREFIX.len());

    let mut out = ptr::null_mut();
    // SAFETY: `archive` is `archive.len()` readable bytes and `out` is a
    // local. The whole archive is passed, so the parser has to stop at the
    // prefix.
    let status = unsafe {
        parse_ssmc_prefix_u64(archive.as_ptr(), archive.len(), &raw mut out)
    };
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    let rust = parse_ssmc_prefix::<u64>(prefix).unwrap();
    // SAFETY: `out` came from the successful call above.
    unsafe { assert_parsed_matches_rust(out, &rust, lookup_u64, 2) };
    // SAFETY: `out` is freed once, here.
    unsafe { free_parsed_ssmc_u64(out) };
}

#[test]
fn parse_ssmc_prefix_u128_matches_rust_api() {
    let hashes = [1_u128, u128::MAX, 1 << 120];
    let archive = rust_archive(hashes, HASH_TYPE_XXH3_128);
    let prefix = &archive[..ffi_prefix_len(&archive)];

    let mut out = ptr::null_mut();
    // SAFETY: `prefix` is `prefix.len()` readable bytes and `out` is a
    // local.
    let status = unsafe {
        parse_ssmc_prefix_u128(prefix.as_ptr(), prefix.len(), &raw mut out)
    };
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    let rust = parse_ssmc_prefix::<u128>(prefix).unwrap();
    // SAFETY: `out` came from the successful call above.
    unsafe { assert_parsed_matches_rust(out, &rust, lookup_u128, 2) };
    // SAFETY: `out` is freed once, here.
    unsafe { free_parsed_ssmc_u128(out) };
}

fn ffi_parse_prefix_u64(prefix: &[u8]) -> (FFIStatus, *mut FFIParsedSSMC<u64>) {
    let mut out = ptr::null_mut();
    // SAFETY: `prefix` is `prefix.len()` readable bytes and `out` is a
    // local.
    let status = unsafe {
        parse_ssmc_prefix_u64(prefix.as_ptr(), prefix.len(), &raw mut out)
    };
    (status, out)
}

#[test]
fn parse_ssmc_prefix_rejects_truncated_prefix() {
    let archive = rust_archive([1_u64, 2, 3], HASH_TYPE_XXH3_64);
    let prefix = &archive[..ffi_prefix_len(&archive)];

    let (status, out) = ffi_parse_prefix_u64(&prefix[..prefix.len() - 1]);
    assert_eq!(
        status_code(status),
        status_code(FFIStatus::SectionOutOfBounds)
    );
    assert!(out.is_null());
}

#[test]
fn parse_ssmc_prefix_rejects_other_format() {
    let mut archive = rust_archive([1_u64, 2, 3], HASH_TYPE_XXH3_64);
    // `format_id` is the little-endian u16 at bytes 20..22.
    archive[20] = 1;

    let (status, out) = ffi_parse_prefix_u64(&archive);
    assert_eq!(status_code(status), status_code(FFIStatus::InvalidHeader));
    assert!(out.is_null());
}

#[test]
fn parse_ssmc_prefix_rejects_null_arguments() {
    let archive = rust_archive([1_u64, 2, 3], HASH_TYPE_XXH3_64);
    let mut out_u64 = ptr::null_mut();
    let mut out_u128 = ptr::null_mut();

    let (bytes, len) = (archive.as_ptr(), archive.len());

    for (prefix, out) in
        [(ptr::null(), &raw mut out_u64), (bytes, ptr::null_mut())]
    {
        // SAFETY: one pointer is null, which the function checks before
        // reading anything; the other points into a live local.
        let status = unsafe { parse_ssmc_prefix_u64(prefix, len, out) };
        assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    }
    for (prefix, out) in
        [(ptr::null(), &raw mut out_u128), (bytes, ptr::null_mut())]
    {
        // SAFETY: one pointer is null, which the function checks before
        // reading anything; the other points into a live local.
        let status = unsafe { parse_ssmc_prefix_u128(prefix, len, out) };
        assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    }
    assert!(out_u64.is_null());
    assert!(out_u128.is_null());
}

#[test]
fn parse_format_data_rejects_null_arguments() {
    let bytes = [0_u8; 56];
    let mut format_data = SSMCFormatData::from_le_bytes(&bytes);

    let calls = [
        (ptr::null(), &raw mut format_data),
        (bytes.as_ptr(), ptr::null_mut()),
    ];

    for (data, out) in calls {
        // SAFETY: one pointer is null, which the function checks before
        // reading or writing anything.
        let status = unsafe { parse_format_data_ffi(data, bytes.len(), out) };
        assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    }
}

#[test]
fn lookup_chunk_location_u128_rejects_null_hash() {
    let archive = rust_archive([1_u128, 2, 3], HASH_TYPE_XXH3_128);
    let mut parsed = ptr::null_mut();
    // SAFETY: `archive` is `archive.len()` readable bytes and `parsed` is a
    // local.
    let status = unsafe {
        parse_ssmc_prefix_u128(archive.as_ptr(), archive.len(), &raw mut parsed)
    };
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

    // SAFETY: `parsed` came from the successful call above.
    let chunk_index = unsafe { (*parsed).chunk_index };
    let mut location = FFIChunkLocation { offset: 0, length: 0 };
    // SAFETY: `chunk_index` is live and the null hash is checked before
    // it's read.
    let status = unsafe {
        lookup_chunk_location_u128(chunk_index, ptr::null(), &raw mut location)
    };
    assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    // SAFETY: `parsed` is freed once, here.
    unsafe { free_parsed_ssmc_u128(parsed) };
}

#[test]
fn free_parsed_ssmc_accepts_null() {
    // SAFETY: `free_parsed_ssmc_u64` returns early on null.
    unsafe { free_parsed_ssmc_u64(ptr::null_mut()) };
    // SAFETY: `free_parsed_ssmc_u128` returns early on null.
    unsafe { free_parsed_ssmc_u128(ptr::null_mut()) };
}
