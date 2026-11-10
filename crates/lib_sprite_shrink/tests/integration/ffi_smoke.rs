//! Calls the C exports directly from Rust, the way a C host would, and
//! checks them against the Rust API.
//!
//! The callbacks for `verify_single_file_*` and `test_compression_*` hand
//! the library buffers from Rust's allocator, as their contract requires:
//! the library takes them back with `Vec::from_raw_parts`. The archive
//! builder instead copies the chunks and hands them back through
//! `free_chunks_cb`, so its callbacks lend the host's own buffers.

use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    fmt::{Debug, Display},
    hash::Hash,
    mem::{ManuallyDrop, MaybeUninit},
    ptr, slice,
    sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}},
};

use bitcode::{Encode, encode};
use dashmap::DashMap;
use sha2::{Digest, Sha512};
use sprite_shrink_test_support::assert_bytes_eq;
use sprite_shrink::{
    ALGORITHM_ZSTD, HASH_TYPE_XXH3_64, HASH_TYPE_XXH3_128, MAX_WINDOW_SIZE,
    MIN_WINDOW_SIZE, SSMC_UID, SUPPORTED_VERSION,
    ArchiveBuilder, ChunkLocation, CompressionResult, FileData, FileHeader,
    FileManifestParent, Hashable, ParsedSSMC, SSAChunkMeta, SSMCFormatData,
    SSMCTocEntry, SerializedData, SpriteShrinkError, build_ssmc_prefix,
    create_file_manifest_and_chunks, get_seek_chunks, parse_file_header,
    parse_ssmc_prefix, process_file_in_memory, serialize_uncompressed_data,
    test_compression,
    ffi::{
        ArchiveBuilderArgsU64, ArchiveBuilderArgsU128, ArchiveBuilderU64,
        ArchiveBuilderU128, CreateFileManifestAndChunksArgs,
        FFICallbackStatus, FFIChunkDataArray, FFIChunkLocation, FFICompData,
        FFIFileData, FFIFileManifestChunks,
        FFIFileManifestParent, FFIParsedSSMC, FFIProgress, FFIProgressType,
        FFISSAChunkMeta, FFISeekInfoArray,
        FFISerializedOutput, FFIStatus, FFITocEntry, FFIVecBytes,
        SerializeUncompressedDataArgsU64, SerializeUncompressedDataArgsU128,
        TestCompressionArgsU64, TestCompressionArgsU128, U128Bytes,
        VerifySingleFileArgsU64, VerifySingleFileArgsU128,
        archive_builder_build_u64, archive_builder_build_u128,
        archive_builder_free_u64,
        archive_builder_free_u128, archive_builder_new_u64,
        archive_builder_new_u128, archive_builder_set_c_progress_u64,
        archive_builder_set_c_progress_u128,
        archive_builder_set_compression_level_u64,
        archive_builder_set_compression_level_u128,
        archive_builder_set_dictionary_size_u64,
        archive_builder_set_dictionary_size_u128,
        archive_builder_set_optimize_dictionary_u64,
        archive_builder_set_optimize_dictionary_u128,
        archive_builder_set_worker_count_u64,
        archive_builder_set_worker_count_u128, archive_data_free,
        build_ssmc_prefix_u64, build_ssmc_prefix_u128,
        create_file_manifest_and_chunks_u64,
        create_file_manifest_and_chunks_u128, decompress_chunk_c,
        free_file_header, free_file_manifest_and_chunks_u64,
        free_file_manifest_and_chunks_u128,
        free_processed_file_data_ffi,
        free_parsed_ssmc_u64, free_parsed_ssmc_u128,
        free_seek_chunks_u64, free_seek_chunks_u128,
        free_serialized_output_u64, free_serialized_output_u128,
        free_ssmc_prefix, get_seek_chunks_u64, get_seek_chunks_u128,
        lookup_chunk_location_u64,
        lookup_chunk_location_u128, parse_file_header_ffi,
        parse_format_data_ffi, parse_ssmc_prefix_u64, parse_ssmc_prefix_u128,
        process_file_in_memory_ffi,
        serialize_uncompressed_data_u64, serialize_uncompressed_data_u128,
        test_compression_u64, test_compression_u128, verify_single_file_u64,
        verify_single_file_u128,
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

    serialize_uncompressed_data(&file_manifest)
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

/// Runs `serialize_uncompressed_data_u64` on `input`, after `edit` has
/// changed the arguments.
fn ffi_serialize_u64(
    input: &HostInput<u64>,
    edit: impl FnOnce(&mut SerializeUncompressedDataArgsU64),
) -> (FFIStatus, *mut FFISerializedOutput<u64>) {
    let mut args = SerializeUncompressedDataArgsU64 {
        manifest_array_ptr: input.manifests.as_ptr(),
        filenames: input.name_ptrs.as_ptr(),
        manifest_len: input.manifests.len(),
    };
    edit(&mut args);
    let mut out = ptr::null_mut();

    // SAFETY: `args` points into `input`, which outlives the call.
    let status = unsafe {
        serialize_uncompressed_data_u64(&raw const args, &raw mut out)
    };
    (status, out)
}

/// Runs `serialize_uncompressed_data_u128` on `input`, after `edit` has
/// changed the arguments.
fn ffi_serialize_u128(
    input: &HostInput<U128Bytes>,
    edit: impl FnOnce(&mut SerializeUncompressedDataArgsU128),
) -> (FFIStatus, *mut FFISerializedOutput<U128Bytes>) {
    let mut args = SerializeUncompressedDataArgsU128 {
        manifest_array_ptr: input.manifests.as_ptr(),
        filenames: input.name_ptrs.as_ptr(),
        manifest_len: input.manifests.len(),
    };
    edit(&mut args);
    let mut out = ptr::null_mut();

    // SAFETY: `args` points into `input`, which outlives the call.
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
/// each hash with `Into<H>`.
///
/// # Safety
///
/// `ptr` must point to `len` manifests the library wrote, not yet freed.
unsafe fn read_manifests<F, H>(
    ptr: *const FFIFileManifestParent<F>,
    len: usize,
) -> Vec<FileManifestParent<H>>
where
    F: Copy + Into<H>,
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
/// with `Into<H>`.
///
/// # Safety
///
/// `out` must come from a successful `serialize_uncompressed_data_*` call
/// and not have been freed.
unsafe fn read_output<F, H>(out: *const FFISerializedOutput<F>)
    -> SerializedData<H>
where
    F: Copy + Into<H>,
    H: Eq + Hash,
{
    // SAFETY: the caller guarantees `out` is valid.
    let out = unsafe { &*out };
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
        sorted_hashes: sorted_hashes.iter().map(|&hash| hash.into()).collect(),
    }
}

fn assert_matches_rust<H>(ffi: &SerializedData<H>, rust: &SerializedData<H>)
where
    H: Copy + Ord + Debug + Encode,
{
    assert_eq!(ffi.archive_toc, rust.archive_toc);
    assert_eq!(encode(&ffi.ser_file_manifest), encode(&rust.ser_file_manifest));
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

    let (status, out) = ffi_serialize_u64(&input, |_| {});
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

    let (status, out) = ffi_serialize_u128(&input, |_| {});
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

    let input = host_input::<u64, u64>(hashes);
    let (status, out) = ffi_serialize_u64(&input, |args| {
        args.filenames = ptr::null();
    });
    assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    assert!(out.is_null());

    let mut input = host_input::<u64, u64>(hashes);
    input.name_ptrs[1] = ptr::null();
    let (status, out) = ffi_serialize_u64(&input, |_| {});
    assert_eq!(status_code(status), status_code(FFIStatus::NullArgument));
    assert!(out.is_null());
}

#[test]
fn serialize_rejects_duplicate_filename() {
    let hashes = [1_u64, 2, 3];
    let mut input = host_input::<u64, u64>(hashes);
    input.name_ptrs[2] = input.name_ptrs[0];

    let (status, out) = ffi_serialize_u64(&input, |_| {});

    assert_eq!(status_code(status), status_code(FFIStatus::DuplicateFilename));
    assert!(out.is_null());
}

#[test]
fn serialize_rejects_filename_that_is_not_utf8() {
    let hashes = [1_u64, 2, 3];
    let invalid = CString::new([0xff, 0xfe]).unwrap();
    let mut input = host_input::<u64, u64>(hashes);
    input.name_ptrs[0] = invalid.as_ptr();

    let (status, out) = ffi_serialize_u64(&input, |_| {});

    assert_eq!(status_code(status), status_code(FFIStatus::InvalidString));
    assert!(out.is_null());
}

#[test]
fn free_serialized_output_accepts_null() {
    // SAFETY: `free_serialized_output_u64` returns early on null.
    unsafe { free_serialized_output_u64(ptr::null_mut()) };
    // SAFETY: `free_serialized_output_u128` returns early on null.
    unsafe { free_serialized_output_u128(ptr::null_mut()) };
}

fn header_bytes() -> [u8; 36] {
    FileHeader::build_file_header(
        3, ALGORITHM_ZSTD, HASH_TYPE_XXH3_64, SSMC_UID, 40,
    )
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

    // SAFETY: the function returns before writing through the null
    // `out_ptr`, and `bytes` is `bytes.len()` readable bytes.
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
    let mut header = FileHeader::build_file_header(
        3, ALGORITHM_ZSTD, HASH_TYPE_XXH3_64, SSMC_UID, 40,
    )
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
fn parse_file_header_rejects_unknown_hash_type() {
    // Which values are rejected is tested in `parsing.rs`. This checks that
    // the rejection reaches C as its own status, with nothing returned.
    let mut header = FileHeader::build_file_header(
        3, ALGORITHM_ZSTD, HASH_TYPE_XXH3_64, SSMC_UID, 40,
    )
        .unwrap();
    header.hash_type = 3;

    let (status, out) = ffi_parse_header(&header.to_le_bytes());
    assert_eq!(
        status_code(status),
        status_code(FFIStatus::UnsupportedHashType)
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
    let (status, ser) = ffi_serialize_u64(&input, |_| {});
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
    let (status, ser) = ffi_serialize_u128(&input, |_| {});
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
    // Encoded as pairs, which decode as the map the parser reads. The
    // parsers never read the data, so any contiguous locations will do.
    let chunk_index: Vec<(H, ChunkLocation)> = ser
        .sorted_hashes
        .iter()
        .zip(0_u64..)
        .map(|(&hash, i)| {
            (hash, ChunkLocation { offset: i * 10, compressed_length: 10 })
        })
        .collect();
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
fn parse_ssmc_prefix_rejects_unknown_hash_type() {
    let mut archive = rust_archive([1_u64, 2, 3], HASH_TYPE_XXH3_64);
    // `hash_type` is the byte at offset 18.
    archive[18] = 3;

    let (status, out) = ffi_parse_prefix_u64(&archive);
    assert_eq!(
        status_code(status),
        status_code(FFIStatus::UnsupportedHashType)
    );
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

/// Calls `process_file_in_memory_ffi` on `data` and returns its status. The
/// call must fail, so nothing is written to the output pointer.
fn ffi_process_failure(data: &[u8], window_size: u64) -> FFIStatus {
    let name = CString::new("test.bin").unwrap();
    let file = FFIFileData {
        filename: name.as_ptr(),
        file_data: data.as_ptr(),
        file_data_len: data.len(),
    };
    let mut processed = ptr::null_mut();
    // SAFETY: `file` points into `name` and `data`, which outlive the call,
    // and `processed` is a local the call may write to.
    let status = unsafe {
        process_file_in_memory_ffi(file, window_size, &raw mut processed)
    };
    assert!(processed.is_null(), "a failed call returned data");
    status
}

#[test]
fn process_file_in_memory_rejects_invalid_window_size() {
    for window_size in [MIN_WINDOW_SIZE - 1, MAX_WINDOW_SIZE + 1] {
        assert_eq!(
            status_code(ffi_process_failure(b"data", window_size)),
            status_code(FFIStatus::InvalidWindowSize),
            "window size {window_size}"
        );
    }
}

#[test]
fn process_file_in_memory_reports_empty_file() {
    assert_eq!(
        status_code(ffi_process_failure(b"", WINDOW_SIZE)),
        status_code(FFIStatus::NoDataToProcess)
    );
}

/// Chunk sizes for the round trip: 64 to 1024 bytes, averaging 256, so the
/// small test files still split into many chunks.
const WINDOW_SIZE: u64 = 256;
const DICTIONARY_SIZE: u64 = 1024;
const COMPRESSION_LEVEL: i32 = 3;
const WORKER_COUNT: usize = 2;

/// Text-like files for the round trip. `patched.bin` differs from
/// `base.bin` in ten bytes, so most of its chunks are shared.
fn round_trip_files() -> Vec<(String, Vec<u8>)> {
    let lines = |seed: u64, count: u64| -> Vec<u8> {
        (0..count)
            .flat_map(|i| {
                format!("entry {i:04}: value {}\n", (i * 7919 + seed) % 10007)
                    .into_bytes()
            })
            .collect()
    };
    let base = lines(0, 600);
    let mut patched = base.clone();
    patched[5000..5010].copy_from_slice(b"PATCHED!!!");

    vec![
        ("base.bin".to_string(), base),
        ("patched.bin".to_string(), patched),
        ("other.bin".to_string(), lines(3, 300)),
    ]
}

/// What the Rust API gives for `files` before the archive builder: the
/// serialized metadata and every chunk, keyed by hash.
fn rust_serialize_files<H>(
    files: &[(String, Vec<u8>)],
) -> (SerializedData<H>, ChunkStore<H>)
where
    H: Hashable + Ord,
{
    let file_manifest = DashMap::new();
    let mut store: ChunkStore<H> = HashMap::new();

    for (name, data) in files {
        let processed = process_file_in_memory(
            FileData { file_name: name.clone(), file_data: data.clone() },
            WINDOW_SIZE,
        )
        .unwrap()
        .unwrap();
        let (fmp, chunks) = create_file_manifest_and_chunks::<H>(
            &processed.file_data,
            &processed.chunks,
        );
        file_manifest.insert(name.clone(), fmp);
        store.extend(chunks);
    }

    (serialize_uncompressed_data(&file_manifest), store)
}

/// Builds the whole archive for `files` with the Rust API: the prefix,
/// then the compressed data.
fn rust_round_trip_archive<H: HashExports>(
    files: &[(String, Vec<u8>)],
    optimize_dictionary: bool,
) -> Vec<u8> {
    let (ser, store) = rust_serialize_files::<H>(files);

    let total_size = store.values().map(|chunk| chunk.len() as u64).sum();
    let store = Arc::new(store);
    let get_chunks = move |hashes: &[H]| {
        Ok::<_, SpriteShrinkError>(
            hashes.iter().map(|hash| store[hash].clone()).collect()
        )
    };

    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let mut builder = ArchiveBuilder::new(
        &ser.sorted_hashes,
        total_size,
        get_chunks,
        move |data: &[u8]| {
            sink.lock().unwrap().extend_from_slice(data);
            Ok::<_, SpriteShrinkError>(())
        },
    );
    builder
        .compression_level(COMPRESSION_LEVEL)
        .dictionary_size(DICTIONARY_SIZE)
        .worker_threads(WORKER_COUNT)
        .optimize_dictionary(optimize_dictionary);
    let comp = builder.build().unwrap();

    let mut archive = build_ssmc_prefix(
        &ser.archive_toc,
        &ser.ser_file_manifest,
        &comp,
        H::HASH_TYPE,
    )
    .unwrap();
    archive.extend_from_slice(&written.lock().unwrap());
    archive
}

/// What the test host keeps for the archive builder's callbacks: the chunks
/// it lends out, and the compressed data written back.
struct BuildHost<H> {
    chunks: ChunkStore<H>,
    written: Mutex<Vec<u8>>,
}

/// Lends the builder the host's chunks for `hashes` without copying them.
fn lend_chunks<H: Eq + Hash>(
    host: &BuildHost<H>,
    hashes: impl Iterator<Item = H>,
) -> FFIChunkDataArray {
    // The library only reads these and copies them before calling
    // `builder_free_chunks`, so they point into the host's store.
    let chunks: Vec<FFIVecBytes> = hashes
        .map(|hash| {
            let chunk = &host.chunks[&hash];
            FFIVecBytes {
                ptr: chunk.as_ptr().cast_mut(),
                len: chunk.len(),
                cap: chunk.len(),
            }
        })
        .collect();
    let (ptr, len, cap) = into_raw_parts(chunks);
    FFIChunkDataArray { ptr, len, cap }
}

unsafe extern "C" fn builder_get_chunks_u64(
    user_data: *mut c_void,
    hashes: *const u64,
    hashes_len: usize,
    out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    // SAFETY: the round trip passes a `BuildHost<u64>` as `user_data`.
    let host = unsafe { &*user_data.cast::<BuildHost<u64>>() };
    // SAFETY: the library passes `hashes_len` hashes.
    let hashes = unsafe { slice::from_raw_parts(hashes, hashes_len) };
    let chunks = lend_chunks(host, hashes.iter().copied());

    // SAFETY: the library passes a pointer that's valid for writes.
    unsafe { out_chunks.write(chunks) };
    FFICallbackStatus::CallbackOk
}

unsafe extern "C" fn builder_get_chunks_u128(
    user_data: *mut c_void,
    hashes: *const u8,
    hashes_len: usize,
    out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    // SAFETY: the round trip passes a `BuildHost<u128>` as `user_data`.
    let host = unsafe { &*user_data.cast::<BuildHost<u128>>() };
    // SAFETY: the library passes `hashes_len` 16-byte hashes.
    let hashes = unsafe { read_u128_hashes(hashes, hashes_len) };
    let chunks = lend_chunks(host, hashes.into_iter());

    // SAFETY: the library passes a pointer that's valid for writes.
    unsafe { out_chunks.write(chunks) };
    FFICallbackStatus::CallbackOk
}

unsafe extern "C" fn builder_free_chunks(
    _user_data: *mut c_void,
    chunks: FFIChunkDataArray,
) {
    // SAFETY: `chunks` is the array `lend_chunks` split with
    // `into_raw_parts`. Its entries point into the host's store, so only the
    // array itself is freed.
    drop(unsafe { Vec::from_raw_parts(chunks.ptr, chunks.len, chunks.cap) });
}

unsafe extern "C" fn builder_write<H>(
    user_data: *mut c_void,
    data: *const u8,
    data_len: usize,
) -> FFICallbackStatus {
    // SAFETY: the round trip passes a `BuildHost<H>` as `user_data`.
    let host = unsafe { &*user_data.cast::<BuildHost<H>>() };
    // SAFETY: the library passes `data_len` readable bytes at `data`.
    let data = unsafe { slice::from_raw_parts(data, data_len) };

    host.written.lock().unwrap().extend_from_slice(data);
    FFICallbackStatus::CallbackOk
}

/// Signatures of the exports that come in a u64 and a u128 form.
type CreateFn<F> = unsafe extern "C" fn(
    *const CreateFileManifestAndChunksArgs,
    *mut *mut FFIFileManifestChunks<F>,
) -> FFIStatus;
type FreeFn<T> = unsafe extern "C" fn(*mut T);
type ArgsFn<A, O> = unsafe extern "C" fn(*const A, *mut *mut O) -> FFIStatus;
type SetFn<B, V> = unsafe extern "C" fn(*mut B, V) -> FFIStatus;
type BuildFn<B> =
    unsafe extern "C" fn(*mut B, *mut *mut FFICompData) -> FFIStatus;

/// One hash width's C exports, implemented on the Rust hash type, so one
/// test body can drive either the u64 or the u128 path. Each export is an
/// associated constant holding a function pointer.
trait HashExports: Hashable + Ord + Debug + Display + Encode {
    /// How the exports pass this hash in structs: `u64` or `U128Bytes`.
    type Ffi: Copy + From<Self> + Into<Self>;
    type SerializeArgs;
    type BuilderArgs;
    type Builder;

    const HASH_TYPE: u8;
    const CREATE: CreateFn<Self::Ffi>;
    const FREE_CREATED: FreeFn<FFIFileManifestChunks<Self::Ffi>>;
    const SERIALIZE: ArgsFn<Self::SerializeArgs, FFISerializedOutput<Self::Ffi>>;
    const FREE_SERIALIZED: FreeFn<FFISerializedOutput<Self::Ffi>>;
    const BUILDER_NEW: ArgsFn<Self::BuilderArgs, Self::Builder>;
    const SET_LEVEL: SetFn<Self::Builder, i32>;
    const SET_DICTIONARY_SIZE: SetFn<Self::Builder, u64>;
    const SET_WORKERS: SetFn<Self::Builder, usize>;
    const BUILD: BuildFn<Self::Builder>;
    const BUILD_PREFIX: BuildPrefixFn<Self::Ffi>;

    fn serialize_args(
        manifests: &[FFIFileManifestParent<Self::Ffi>],
        names: &[*const c_char],
    ) -> Self::SerializeArgs;

    /// Arguments for a builder fed from `ser`, `hashes` and `host`.
    fn builder_args(
        ser: &FFISerializedOutput<Self::Ffi>,
        hashes: &[Self::Ffi],
        file_count: u32,
        host: *mut BuildHost<Self>,
    ) -> Self::BuilderArgs;
}

impl HashExports for u64 {
    type Ffi = u64;
    type SerializeArgs = SerializeUncompressedDataArgsU64;
    type BuilderArgs = ArchiveBuilderArgsU64;
    type Builder = ArchiveBuilderU64;

    const HASH_TYPE: u8 = HASH_TYPE_XXH3_64;
    const CREATE: CreateFn<u64> = create_file_manifest_and_chunks_u64;
    const FREE_CREATED: FreeFn<FFIFileManifestChunks<u64>> =
        free_file_manifest_and_chunks_u64;
    const SERIALIZE: ArgsFn<
        SerializeUncompressedDataArgsU64,
        FFISerializedOutput<u64>,
    > = serialize_uncompressed_data_u64;
    const FREE_SERIALIZED: FreeFn<FFISerializedOutput<u64>> =
        free_serialized_output_u64;
    const BUILDER_NEW: ArgsFn<ArchiveBuilderArgsU64, ArchiveBuilderU64> =
        archive_builder_new_u64;
    const SET_LEVEL: SetFn<ArchiveBuilderU64, i32> =
        archive_builder_set_compression_level_u64;
    const SET_DICTIONARY_SIZE: SetFn<ArchiveBuilderU64, u64> =
        archive_builder_set_dictionary_size_u64;
    const SET_WORKERS: SetFn<ArchiveBuilderU64, usize> =
        archive_builder_set_worker_count_u64;
    const BUILD: BuildFn<ArchiveBuilderU64> = archive_builder_build_u64;
    const BUILD_PREFIX: BuildPrefixFn<u64> = build_ssmc_prefix_u64;

    fn serialize_args(
        manifests: &[FFIFileManifestParent<u64>],
        names: &[*const c_char],
    ) -> SerializeUncompressedDataArgsU64 {
        SerializeUncompressedDataArgsU64 {
            manifest_array_ptr: manifests.as_ptr(),
            filenames: names.as_ptr(),
            manifest_len: manifests.len(),
        }
    }

    fn builder_args(
        ser: &FFISerializedOutput<u64>,
        hashes: &[u64],
        file_count: u32,
        host: *mut BuildHost<u64>,
    ) -> ArchiveBuilderArgsU64 {
        // SAFETY: the caller passes a live host.
        let total_size = unsafe { &*host }
            .chunks
            .values()
            .map(|chunk| chunk.len() as u64)
            .sum();
        ArchiveBuilderArgsU64 {
            manifest_array_ptr: ser.ser_manifest_ptr,
            manifest_len: ser.ser_manifest_len,
            sorted_hashes_array_ptr: hashes.as_ptr(),
            sorted_hashes_len: hashes.len(),
            file_count,
            total_size,
            user_data: host.cast(),
            get_chunks_cb: builder_get_chunks_u64,
            free_chunks_cb: builder_free_chunks,
            write_comp_data_cb: builder_write::<u64>,
        }
    }
}

impl HashExports for u128 {
    type Ffi = U128Bytes;
    type SerializeArgs = SerializeUncompressedDataArgsU128;
    type BuilderArgs = ArchiveBuilderArgsU128;
    type Builder = ArchiveBuilderU128;

    const HASH_TYPE: u8 = HASH_TYPE_XXH3_128;
    const CREATE: CreateFn<U128Bytes> = create_file_manifest_and_chunks_u128;
    const FREE_CREATED: FreeFn<FFIFileManifestChunks<U128Bytes>> =
        free_file_manifest_and_chunks_u128;
    const SERIALIZE: ArgsFn<
        SerializeUncompressedDataArgsU128,
        FFISerializedOutput<U128Bytes>,
    > = serialize_uncompressed_data_u128;
    const FREE_SERIALIZED: FreeFn<FFISerializedOutput<U128Bytes>> =
        free_serialized_output_u128;
    const BUILDER_NEW: ArgsFn<ArchiveBuilderArgsU128, ArchiveBuilderU128> =
        archive_builder_new_u128;
    const SET_LEVEL: SetFn<ArchiveBuilderU128, i32> =
        archive_builder_set_compression_level_u128;
    const SET_DICTIONARY_SIZE: SetFn<ArchiveBuilderU128, u64> =
        archive_builder_set_dictionary_size_u128;
    const SET_WORKERS: SetFn<ArchiveBuilderU128, usize> =
        archive_builder_set_worker_count_u128;
    const BUILD: BuildFn<ArchiveBuilderU128> = archive_builder_build_u128;
    const BUILD_PREFIX: BuildPrefixFn<U128Bytes> = build_ssmc_prefix_u128;

    fn serialize_args(
        manifests: &[FFIFileManifestParent<U128Bytes>],
        names: &[*const c_char],
    ) -> SerializeUncompressedDataArgsU128 {
        SerializeUncompressedDataArgsU128 {
            manifest_array_ptr: manifests.as_ptr(),
            filenames: names.as_ptr(),
            manifest_len: manifests.len(),
        }
    }

    fn builder_args(
        ser: &FFISerializedOutput<U128Bytes>,
        hashes: &[U128Bytes],
        file_count: u32,
        host: *mut BuildHost<u128>,
    ) -> ArchiveBuilderArgsU128 {
        // SAFETY: the caller passes a live host.
        let total_size = unsafe { &*host }
            .chunks
            .values()
            .map(|chunk| chunk.len() as u64)
            .sum();
        ArchiveBuilderArgsU128 {
            manifest_array_ptr: ser.ser_manifest_ptr,
            manifest_len: ser.ser_manifest_len,
            // `U128Bytes` is `#[repr(C)]` around `[u8; 16]`, so the hashes
            // are already the 16 little-endian bytes per hash the builder
            // reads.
            sorted_hashes_array_ptr: hashes.as_ptr().cast(),
            sorted_hashes_len: hashes.len(),
            file_count,
            total_size,
            user_data: host.cast(),
            get_chunks_cb: builder_get_chunks_u128,
            free_chunks_cb: builder_free_chunks,
            write_comp_data_cb: builder_write::<u128>,
        }
    }
}

/// Runs the part of a C host's archive path that comes before the builder:
/// process and chunk each file, then serialize. Hands `then` the serialized
/// output and every chunk, keyed by hash, and frees everything afterwards.
/// Nothing here calls zstd, so Miri can run it.
fn ffi_serialize_files<K: HashExports, R>(
    files: &[(String, Vec<u8>)],
    then: impl FnOnce(&FFISerializedOutput<K::Ffi>, ChunkStore<K>) -> R,
) -> R {
    let names: Vec<CString> = files
        .iter()
        .map(|(name, _)| CString::new(name.as_str()).unwrap())
        .collect();
    let mut chunks: ChunkStore<K> = HashMap::new();
    let mut file_chunks = Vec::new();

    for (name, (_, data)) in names.iter().zip(files) {
        let file = FFIFileData {
            filename: name.as_ptr(),
            file_data: data.as_ptr(),
            file_data_len: data.len(),
        };
        let mut processed = ptr::null_mut();
        // SAFETY: `file` points into `name` and `data`, which outlive the
        // call, and `processed` is a local.
        let status = unsafe {
            process_file_in_memory_ffi(file, WINDOW_SIZE, &raw mut processed)
        };
        assert_ok(status);

        // SAFETY: `processed` came from the successful call above.
        let processed_ref = unsafe { &*processed };
        let args = CreateFileManifestAndChunksArgs {
            file_name_ptr: name.as_ptr(),
            file_data_array_ptr: processed_ref.file_data,
            file_data_len: processed_ref.file_data_len,
            chunks_array_ptr: processed_ref.chunks,
            chunks_len: processed_ref.chunks_len,
        };
        let mut manifest_chunks = ptr::null_mut();
        // SAFETY: `args` points into `name` and `processed`, which are both
        // still live, and `manifest_chunks` is a local.
        let status = unsafe {
            K::CREATE(&raw const args, &raw mut manifest_chunks)
        };
        assert_ok(status);
        // SAFETY: `processed` came from `process_file_in_memory_ffi` and is
        // freed once, here.
        unsafe { free_processed_file_data_ffi(processed) };

        // SAFETY: `manifest_chunks` came from the successful call above.
        let manifest_chunks_ref = unsafe { &*manifest_chunks };
        // SAFETY: the library wrote `hashed_chunks_len` chunks at
        // `hashed_chunks`.
        let hashed_chunks = unsafe {
            slice::from_raw_parts(
                manifest_chunks_ref.hashed_chunks,
                manifest_chunks_ref.hashed_chunks_len,
            )
        };
        for chunk in hashed_chunks {
            // SAFETY: the library wrote `chunk_data_len` bytes at
            // `chunk_data` for each chunk.
            let bytes = unsafe {
                slice::from_raw_parts(chunk.chunk_data, chunk.chunk_data_len)
            };
            chunks.entry(chunk.hash.into()).or_insert_with(|| bytes.to_vec());
        }
        file_chunks.push(manifest_chunks);
    }

    let manifests: Vec<FFIFileManifestParent<K::Ffi>> = file_chunks
        .iter()
        .map(|&manifest_chunks| {
            // SAFETY: each pointer came from a successful `K::CREATE` call
            // and isn't freed yet.
            let fmp = unsafe { &(*manifest_chunks).fmp };
            FFIFileManifestParent {
                chunk_metadata: fmp.chunk_metadata,
                chunk_metadata_len: fmp.chunk_metadata_len,
                chunk_metadata_cap: fmp.chunk_metadata_cap,
            }
        })
        .collect();
    let name_ptrs: Vec<*const c_char> =
        names.iter().map(|name| name.as_ptr()).collect();
    let ser_args = K::serialize_args(&manifests, &name_ptrs);
    let mut ser = ptr::null_mut();
    // SAFETY: `ser_args` points into `manifests` and `name_ptrs`, which
    // outlive the call, and `ser` is a local.
    let status = unsafe { K::SERIALIZE(&raw const ser_args, &raw mut ser) };
    assert_ok(status);

    // SAFETY: `ser` came from the successful call above and is freed only
    // after `then` returns.
    let result = then(unsafe { &*ser }, chunks);

    // SAFETY: `ser` came from `K::SERIALIZE` and is freed once, here.
    unsafe { K::FREE_SERIALIZED(ser) };
    for manifest_chunks in file_chunks {
        // SAFETY: each pointer came from `K::CREATE` and is freed once,
        // here.
        unsafe { K::FREE_CREATED(manifest_chunks) };
    }
    result
}

/// Builds the whole archive for `files` through the C exports, the way a C
/// host would: process and chunk each file, serialize, compress, then build
/// the prefix and put it in front of the compressed data. `configure` runs
/// after the standard settings, just before the build.
///
/// The builder gets its own copy of the sorted hashes, which is overwritten
/// and freed as soon as the builder exists. The exports say a host may free
/// that array once `new` returns, so a builder that kept reading it would
/// build the wrong archive.
fn ffi_round_trip_archive<K: HashExports>(
    files: &[(String, Vec<u8>)],
    configure: impl FnOnce(*mut K::Builder),
) -> Vec<u8> {
    ffi_serialize_files::<K, _>(files, |ser, chunks| {
        let mut host = BuildHost { chunks, written: Mutex::new(Vec::new()) };
        let file_count = u32::try_from(files.len()).unwrap();
        // SAFETY: the library wrote `sorted_hashes_len` hashes at
        // `sorted_hashes_ptr`, and `ser` isn't freed until this closure
        // returns.
        let mut hashes = unsafe {
            slice::from_raw_parts(ser.sorted_hashes_ptr, ser.sorted_hashes_len)
        }
        .to_vec();
        let builder_args =
            K::builder_args(ser, &hashes, file_count, &raw mut host);
        let mut builder = ptr::null_mut();
        // SAFETY: `builder_args` points into `ser`, `hashes` and `host`. All
        // three are live for the call, and `ser` and `host` outlive the
        // builder, which is all the exports ask. `builder` is a local.
        assert_ok(unsafe {
            K::BUILDER_NEW(&raw const builder_args, &raw mut builder)
        });
        // Every entry becomes the first hash, so a builder still reading
        // this array would compress one chunk over and over.
        let first = hashes[0];
        hashes.fill(first);
        drop(hashes);

        // SAFETY: `builder` came from `K::BUILDER_NEW` and hasn't been built
        // or freed.
        assert_ok(unsafe { K::SET_LEVEL(builder, COMPRESSION_LEVEL) });
        // SAFETY: `builder` came from `K::BUILDER_NEW` and hasn't been built
        // or freed.
        assert_ok(unsafe { K::SET_DICTIONARY_SIZE(builder, DICTIONARY_SIZE) });
        // SAFETY: `builder` came from `K::BUILDER_NEW` and hasn't been built
        // or freed.
        assert_ok(unsafe { K::SET_WORKERS(builder, WORKER_COUNT) });
        configure(builder);

        let mut comp = ptr::null_mut();
        // SAFETY: `builder` came from `K::BUILDER_NEW` and hasn't been built
        // or freed; building consumes it. `comp` is a local.
        assert_ok(unsafe { K::BUILD(builder, &raw mut comp) });

        let mut prefix = ptr::null_mut();
        // SAFETY: the TOC and manifests in `ser` both hold `toc_len`
        // elements, `comp` came from the successful build above, and
        // `prefix` is a local.
        assert_ok(unsafe {
            K::BUILD_PREFIX(
                ser.toc_ptr,
                ser.ser_manifest_ptr,
                ser.toc_len,
                comp,
                &raw mut prefix,
            )
        });

        // SAFETY: `prefix` came from the successful call above.
        let mut archive = unsafe { take_prefix(prefix) };
        archive.extend_from_slice(&host.written.lock().unwrap());

        // SAFETY: `comp` came from `K::BUILD` and is freed once, here.
        unsafe { archive_data_free(comp) };
        archive
    })
}

/// Rebuilds every file in `archive` through the C read exports, the way a
/// C host would, in table of contents order.
fn ffi_extract(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
    let (status, parsed) =
        ffi_parse_prefix_u64(&archive[..ffi_prefix_len(archive)]);
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));
    // SAFETY: `parsed` came from the successful call above.
    let parsed_ref = unsafe { &*parsed };

    // SAFETY: the library wrote `toc_len` entries with null-terminated names
    // at `toc_ptr`, and `parsed` hasn't been freed.
    let toc = unsafe { read_toc(parsed_ref.toc_ptr, parsed_ref.toc_len) };
    // SAFETY: the library wrote `manifests_len` manifests at
    // `manifests_ptr`, and `parsed` hasn't been freed.
    let manifests: Vec<FileManifestParent<u64>> = unsafe {
        read_manifests(parsed_ref.manifests_ptr, parsed_ref.manifests_len)
    };
    // SAFETY: the library wrote `dictionary_len` bytes at `dictionary_ptr`,
    // and `parsed` hasn't been freed.
    let dictionary = unsafe {
        slice::from_raw_parts(
            parsed_ref.dictionary_ptr,
            parsed_ref.dictionary_len,
        )
    };

    let files = toc
        .into_iter()
        .zip(manifests)
        .map(|(entry, manifest)| {
            let size = usize::try_from(entry.uncompressed_size).unwrap();
            let mut file = vec![0; size];

            for meta in manifest.chunk_metadata {
                let (status, location) =
                    lookup_u64(parsed_ref.chunk_index, meta.hash);
                assert_eq!(
                    status_code(status),
                    status_code(FFIStatus::StatusOk)
                );

                let start = usize::try_from(location.offset).unwrap();
                let end = start + usize::try_from(location.length).unwrap();
                let compressed = &archive[start..end];
                let out_start = usize::try_from(meta.offset).unwrap();
                let out_end = out_start + usize::try_from(meta.length).unwrap();
                let out = &mut file[out_start..out_end];

                // SAFETY: each pointer covers its slice's length, and `out`
                // is valid for writes.
                let status = unsafe {
                    decompress_chunk_c(
                        compressed.as_ptr(),
                        compressed.len(),
                        dictionary.as_ptr(),
                        dictionary.len(),
                        out.as_mut_ptr(),
                        out.len(),
                    )
                };
                assert_eq!(
                    status_code(status),
                    status_code(FFIStatus::StatusOk)
                );
            }
            (entry.filename, file)
        })
        .collect();

    // SAFETY: `parsed` came from `parse_ssmc_prefix_u64` and is freed once,
    // here.
    unsafe { free_parsed_ssmc_u64(parsed) };
    files
}

fn assert_round_trip_matches_rust<K: HashExports>() {
    let files = round_trip_files();

    assert_bytes_eq(
        &ffi_round_trip_archive::<K>(&files, |_| {}),
        &rust_round_trip_archive::<K>(&files, false),
        "archive built through the C exports",
    );
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn ffi_round_trip_archive_u64_matches_rust_api() {
    assert_round_trip_matches_rust::<u64>();
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn ffi_round_trip_archive_u128_matches_rust_api() {
    assert_round_trip_matches_rust::<u128>();
}

/// Checks the C path up to the builder against the Rust API. This is the
/// part of the round trip Miri can run.
fn assert_serialized_files_match_rust<K: HashExports>() {
    let files = round_trip_files();
    let (rust, rust_chunks) = rust_serialize_files::<K>(&files);

    ffi_serialize_files::<K, _>(&files, |ser, chunks| {
        // SAFETY: `ser` came from a successful serialize call and is freed
        // only after this closure returns.
        let ffi: SerializedData<K> = unsafe { read_output(ptr::from_ref(ser)) };
        assert_eq!(ffi.archive_toc, rust.archive_toc);
        assert_eq!(
            encode(&ffi.ser_file_manifest),
            encode(&rust.ser_file_manifest)
        );
        assert_eq!(ffi.sorted_hashes, rust.sorted_hashes);
        assert_eq!(chunks, rust_chunks);
    });
}

#[test]
fn ffi_serialized_files_u64_match_rust_api() {
    assert_serialized_files_match_rust::<u64>();
}

#[test]
fn ffi_serialized_files_u128_match_rust_api() {
    assert_serialized_files_match_rust::<u128>();
}

#[test]
fn u128_bytes_round_trip_as_little_endian() {
    for value in [0, 1, 1 << 64, u128::MAX, 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100]
    {
        let bytes = U128Bytes::from(value);
        assert_eq!(bytes.bytes, value.to_le_bytes(), "{value:#x}");
        assert_eq!(u128::from(bytes), value, "{value:#x}");
    }
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn ffi_round_trip_extracts_original_files() {
    let mut files = round_trip_files();
    let archive = ffi_round_trip_archive::<u64>(&files, |_| {});

    // The table of contents is sorted by name.
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let extracted = ffi_extract(&archive);
    let names = |set: &[(String, Vec<u8>)]| -> Vec<String> {
        set.iter().map(|(name, _)| name.clone()).collect()
    };
    assert_eq!(names(&extracted), names(&files));
    for ((name, data), (_, original)) in extracted.iter().zip(&files) {
        assert_bytes_eq(data, original, name);
    }
}

// Seeking

const SEEK_HASHES_U64: [u64; 3] = [1, u64::MAX, 7];
const SEEK_HASHES_U128: [u128; 3] = [1, u128::MAX, 7];

/// `get_seek_chunks` as a C host calls it.
type GetSeek<F> = unsafe extern "C" fn(
    *const FFIFileManifestParent<F>,
    u64,
    u64,
    *mut *mut FFISeekInfoArray<F>,
) -> FFIStatus;

/// The first entry of `FILES` is a 4-byte chunk then a 3-byte chunk.
fn assert_seek_matches_rust<F, H>(
    hashes: [H; 3],
    get_seek: GetSeek<F>,
    free: unsafe extern "C" fn(*mut FFISeekInfoArray<F>),
)
where
    F: Copy + From<H>,
    H: Copy + Debug + Eq + Hash + From<F>,
{
    let input = host_input::<H, F>(hashes);
    let chunk_metadata = chunk_metadata(hashes, FILES[0].1);
    let manifest = FileManifestParent {
        chunk_count: chunk_metadata.len() as u64,
        chunk_metadata,
    };

    // Whole file, inside a chunk, across chunks, the last chunk, empty
    // reads at the start and at the end.
    for (offset, length) in [(0, 7), (1, 2), (3, 2), (4, 3), (0, 0), (7, 0)] {
        let mut out = ptr::null_mut();
        // SAFETY: `input` owns the manifest for the whole call and `out` is
        // a local.
        let status = unsafe {
            get_seek(&raw const input.manifests[0], offset, length, &raw mut out)
        };
        assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));

        // SAFETY: `out` came from the successful call above.
        let array = unsafe { &*out };
        // SAFETY: the library wrote `chunks_len` entries at `chunks`, which
        // is non-null even for an empty result.
        let ffi = unsafe { slice::from_raw_parts(array.chunks, array.chunks_len) };
        let ffi: Vec<(H, u64, u64)> = ffi
            .iter()
            .map(|c| (H::from(c.hash), c.read_start, c.read_end))
            .collect();
        let rust: Vec<(H, u64, u64)> =
            get_seek_chunks(&manifest, offset, length)
                .unwrap()
                .into_iter()
                .map(|c| (c.hash, c.start_offset, c.end_offset))
                .collect();
        assert_eq!(ffi, rust, "{length} bytes at {offset}");

        // SAFETY: `out` came from the call above and is freed once.
        unsafe { free(out) };
    }
}

#[test]
fn get_seek_chunks_u64_matches_rust_api() {
    assert_seek_matches_rust::<u64, u64>(
        SEEK_HASHES_U64,
        get_seek_chunks_u64,
        free_seek_chunks_u64,
    );
}

#[test]
fn get_seek_chunks_u128_matches_rust_api() {
    assert_seek_matches_rust::<U128Bytes, u128>(
        SEEK_HASHES_U128,
        get_seek_chunks_u128,
        free_seek_chunks_u128,
    );
}

#[test]
fn get_seek_chunks_rejects_a_read_past_the_end() {
    let input = host_input::<u64, u64>(SEEK_HASHES_U64);

    for (offset, length) in [(5, 3), (8, 0), (u64::MAX, 2)] {
        let mut out = ptr::null_mut();
        // SAFETY: `input` owns the manifest for the whole call and `out` is
        // a local.
        let status = unsafe {
            get_seek_chunks_u64(
                &raw const input.manifests[0],
                offset,
                length,
                &raw mut out,
            )
        };
        assert_eq!(status_code(status), status_code(FFIStatus::SeekOutOfBounds));
        assert!(out.is_null(), "nothing to free after {offset}+{length}");
    }
}

#[test]
fn free_seek_chunks_accepts_null() {
    // SAFETY: `free_seek_chunks_u64` returns early for a null pointer.
    unsafe { free_seek_chunks_u64(ptr::null_mut()) };
    // SAFETY: `free_seek_chunks_u128` returns early for a null pointer.
    unsafe { free_seek_chunks_u128(ptr::null_mut()) };
}

// Callback hosts that follow the ownership contract

/// Copies the chunks for `hashes` out of `store` the way the
/// `verify_single_file_*` and `test_compression_*` contract requires: the
/// array and every buffer come from Rust's allocator, and the library frees
/// them.
fn owned_chunks<H: Eq + Hash>(
    store: &ChunkStore<H>,
    hashes: impl Iterator<Item = H>,
) -> FFIChunkDataArray {
    let chunks: Vec<FFIVecBytes> = hashes
        .map(|hash| {
            let (ptr, len, cap) = into_raw_parts(store[&hash].clone());
            FFIVecBytes { ptr, len, cap }
        })
        .collect();
    let (ptr, len, cap) = into_raw_parts(chunks);
    FFIChunkDataArray { ptr, len, cap }
}

/// Reads the 16-byte little-endian hashes the u128 exports pass callbacks.
///
/// # Safety
///
/// `hashes` must point to `len` hashes, `16 * len` readable bytes.
unsafe fn read_u128_hashes(hashes: *const u8, len: usize) -> Vec<u128> {
    // SAFETY: the caller guarantees `16 * len` readable bytes.
    let bytes = unsafe { slice::from_raw_parts(hashes, len * 16) };
    let (hashes, _) = bytes.as_chunks::<16>();
    hashes.iter().map(|hash| u128::from_le_bytes(*hash)).collect()
}

// Verifying a file

struct VerifyHost<H> {
    chunks: ChunkStore<H>,
    progress: AtomicU64,
}

impl<H: Copy + Eq + Hash> VerifyHost<H> {
    fn new(hashes: [H; 3]) -> Self {
        Self {
            chunks: hashes
                .into_iter()
                .zip(CHUNKS)
                .map(|(hash, chunk)| (hash, chunk.to_vec()))
                .collect(),
            progress: AtomicU64::new(0),
        }
    }
}

unsafe extern "C" fn verify_get_chunks_u64(
    user_data: *mut c_void,
    hashes: *const u64,
    hashes_len: usize,
) -> FFIChunkDataArray {
    // SAFETY: these tests pass a `VerifyHost<u64>` as `user_data`.
    let host = unsafe { &*user_data.cast::<VerifyHost<u64>>() };
    // SAFETY: the library passes `hashes_len` hashes.
    let hashes = unsafe { slice::from_raw_parts(hashes, hashes_len) };
    owned_chunks(&host.chunks, hashes.iter().copied())
}

unsafe extern "C" fn verify_get_chunks_u128(
    user_data: *mut c_void,
    hashes: *const u8,
    hashes_len: usize,
) -> FFIChunkDataArray {
    // SAFETY: these tests pass a `VerifyHost<u128>` as `user_data`.
    let host = unsafe { &*user_data.cast::<VerifyHost<u128>>() };
    // SAFETY: the library passes `hashes_len` 16-byte hashes.
    let hashes = unsafe { read_u128_hashes(hashes, hashes_len) };
    owned_chunks(&host.chunks, hashes.into_iter())
}

unsafe extern "C" fn verify_progress<H>(user_data: *mut c_void, bytes: u64) {
    // SAFETY: these tests pass a `VerifyHost<H>` as `user_data`.
    let host = unsafe { &*user_data.cast::<VerifyHost<H>>() };
    host.progress.fetch_add(bytes, Ordering::Relaxed);
}

/// The bytes of `FILES[index]` and their verification hash.
fn file_and_hash(index: usize) -> (Vec<u8>, [u8; 64]) {
    let data: Vec<u8> = FILES[index].1
        .iter()
        .flat_map(|&i| CHUNKS[i].iter().copied())
        .collect();
    let hash = Sha512::digest(&data).into();
    (data, hash)
}

/// Verifies `FILES[index]` against `veri_hash` and returns the status and
/// the progress reported.
fn ffi_verify_u64(index: usize, veri_hash: &[u8; 64]) -> (i32, u64) {
    let input = host_input::<u64, u64>(SEEK_HASHES_U64);
    let host = VerifyHost::new(SEEK_HASHES_U64);
    let args = VerifySingleFileArgsU64 {
        filename: input.name_ptrs[index],
        file_manifest_parent: &raw const input.manifests[index],
        veri_hash_array_ptr: veri_hash.as_ptr(),
        user_data: ptr::from_ref(&host).cast_mut().cast(),
        get_chunks_cb: verify_get_chunks_u64,
        progress_cb: verify_progress::<u64>,
    };
    // SAFETY: `args` points into `input`, `veri_hash` and `host`, which all
    // outlive the call.
    let status = unsafe { verify_single_file_u64(&raw const args) };
    (status_code(status), host.progress.load(Ordering::Relaxed))
}

/// The u128 form of [`ffi_verify_u64`].
fn ffi_verify_u128(index: usize, veri_hash: &[u8; 64]) -> (i32, u64) {
    let input = host_input::<u128, U128Bytes>(SEEK_HASHES_U128);
    let host = VerifyHost::new(SEEK_HASHES_U128);
    let args = VerifySingleFileArgsU128 {
        filename: input.name_ptrs[index],
        file_manifest_parent: &raw const input.manifests[index],
        veri_hash_array_ptr: veri_hash.as_ptr(),
        user_data: ptr::from_ref(&host).cast_mut().cast(),
        get_chunks_cb: verify_get_chunks_u128,
        progress_cb: verify_progress::<u128>,
    };
    // SAFETY: `args` points into `input`, `veri_hash` and `host`, which all
    // outlive the call.
    let status = unsafe { verify_single_file_u128(&raw const args) };
    (status_code(status), host.progress.load(Ordering::Relaxed))
}

#[test]
fn verify_single_file_accepts_the_right_hash() {
    for index in [0, 1, 2] {
        let (data, hash) = file_and_hash(index);
        let ok = status_code(FFIStatus::StatusOk);
        let len = data.len() as u64;

        assert_eq!(ffi_verify_u64(index, &hash), (ok, len), "u64 file {index}");
        assert_eq!(ffi_verify_u128(index, &hash), (ok, len), "u128 file {index}");
    }
}

#[test]
fn verify_single_file_rejects_the_wrong_hash() {
    let (_, mut hash) = file_and_hash(0);
    hash[63] ^= 1;
    let mismatch = status_code(FFIStatus::VerificationHashMismatch);

    assert_eq!(ffi_verify_u64(0, &hash).0, mismatch);
    assert_eq!(ffi_verify_u128(0, &hash).0, mismatch);
}

// Estimating compression

/// The unique chunks of `round_trip_files`, their sorted hashes and their
/// total size.
fn compression_input<H>() -> (ChunkStore<H>, Vec<H>, u64)
where
    H: Hashable + Ord,
{
    let mut store = HashMap::new();
    for (name, data) in round_trip_files() {
        let processed = process_file_in_memory(
            FileData { file_name: name, file_data: data },
            WINDOW_SIZE,
        )
        .unwrap()
        .unwrap();
        let (_, chunks) = create_file_manifest_and_chunks::<H>(
            &processed.file_data,
            &processed.chunks,
        );
        store.extend(chunks);
    }
    let mut hashes: Vec<H> = store.keys().copied().collect();
    hashes.sort_unstable();
    let total = store.values().map(|chunk| chunk.len() as u64).sum();
    (store, hashes, total)
}

/// What `test_compression` returns for the same input.
fn rust_estimate<H>(store: ChunkStore<H>, hashes: &[H], total: u64) -> u64
where
    H: Hashable + Encode,
{
    let store = Arc::new(store);
    let get_chunks = move |wanted: &[H]| {
        Ok::<_, SpriteShrinkError>(
            wanted.iter().map(|hash| store[hash].clone()).collect()
        )
    };
    test_compression(
        hashes,
        total,
        WORKER_COUNT,
        usize::try_from(DICTIONARY_SIZE).unwrap(),
        COMPRESSION_LEVEL,
        get_chunks,
    )
    .unwrap() as u64
}

unsafe extern "C" fn estimate_get_chunks_u64(
    user_data: *mut c_void,
    hashes: *const u64,
    hashes_len: usize,
    out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    // SAFETY: these tests pass a `ChunkStore<u64>` as `user_data`.
    let store = unsafe { &*user_data.cast::<ChunkStore<u64>>() };
    // SAFETY: the library passes `hashes_len` hashes.
    let hashes = unsafe { slice::from_raw_parts(hashes, hashes_len) };
    let chunks = owned_chunks(store, hashes.iter().copied());
    // SAFETY: the library passes a pointer that's valid for writes.
    unsafe { out_chunks.write(chunks) };
    FFICallbackStatus::CallbackOk
}

unsafe extern "C" fn estimate_get_chunks_u128(
    user_data: *mut c_void,
    hashes: *const u8,
    hashes_len: usize,
    out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    // SAFETY: these tests pass a `ChunkStore<u128>` as `user_data`.
    let store = unsafe { &*user_data.cast::<ChunkStore<u128>>() };
    // SAFETY: the library passes `hashes_len` 16-byte hashes.
    let hashes = unsafe { read_u128_hashes(hashes, hashes_len) };
    let chunks = owned_chunks(store, hashes.into_iter());
    // SAFETY: the library passes a pointer that's valid for writes.
    unsafe { out_chunks.write(chunks) };
    FFICallbackStatus::CallbackOk
}

unsafe extern "C" fn chunks_fail<T>(
    _user_data: *mut c_void,
    _hashes: *const T,
    _hashes_len: usize,
    _out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    FFICallbackStatus::Error
}

unsafe extern "C" fn chunks_cancel<T>(
    _user_data: *mut c_void,
    _hashes: *const T,
    _hashes_len: usize,
    _out_chunks: *mut FFIChunkDataArray,
) -> FFICallbackStatus {
    FFICallbackStatus::Cancelled
}

/// Runs `test_compression_u64` with `get_chunks_cb` over `store`.
fn ffi_estimate_u64(
    store: &mut ChunkStore<u64>,
    hashes: &[u64],
    total: u64,
    get_chunks_cb: unsafe extern "C" fn(
        *mut c_void,
        *const u64,
        usize,
        *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
) -> (i32, u64) {
    let args = TestCompressionArgsU64 {
        total_data_size: total,
        sorted_hashes_array_ptr: hashes.as_ptr(),
        sorted_hashes_len: hashes.len(),
        worker_count: WORKER_COUNT,
        dictionary_size: usize::try_from(DICTIONARY_SIZE).unwrap(),
        compression_level: COMPRESSION_LEVEL,
        user_data: ptr::from_mut(store).cast(),
        get_chunks_cb,
    };
    let mut size = 0;
    // SAFETY: `args` points into `hashes` and `store`, which outlive the
    // call, and `size` is a local.
    let status = unsafe { test_compression_u64(&raw const args, &raw mut size) };
    (status_code(status), size)
}

/// The u128 form of [`ffi_estimate_u64`].
fn ffi_estimate_u128(
    store: &mut ChunkStore<u128>,
    hashes: &[u128],
    total: u64,
    get_chunks_cb: unsafe extern "C" fn(
        *mut c_void,
        *const u8,
        usize,
        *mut FFIChunkDataArray,
    ) -> FFICallbackStatus,
) -> (i32, u64) {
    let hash_bytes: Vec<u8> =
        hashes.iter().flat_map(|hash| hash.to_le_bytes()).collect();
    let args = TestCompressionArgsU128 {
        total_data_size: total,
        sorted_hashes_array_ptr: hash_bytes.as_ptr(),
        sorted_hashes_len: hashes.len(),
        worker_count: WORKER_COUNT,
        dictionary_size: usize::try_from(DICTIONARY_SIZE).unwrap(),
        compression_level: COMPRESSION_LEVEL,
        user_data: ptr::from_mut(store).cast(),
        get_chunks_cb,
    };
    let mut size = 0;
    // SAFETY: `args` points into `hash_bytes` and `store`, which outlive the
    // call, and `size` is a local.
    let status = unsafe { test_compression_u128(&raw const args, &raw mut size) };
    (status_code(status), size)
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn test_compression_u64_matches_rust_api() {
    let (mut store, hashes, total) = compression_input::<u64>();
    let expected = rust_estimate(store.clone(), &hashes, total);

    let (status, size) =
        ffi_estimate_u64(&mut store, &hashes, total, estimate_get_chunks_u64);

    assert_eq!(status, status_code(FFIStatus::StatusOk));
    assert_eq!(size, expected);
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn test_compression_u128_matches_rust_api() {
    let (mut store, hashes, total) = compression_input::<u128>();
    let expected = rust_estimate(store.clone(), &hashes, total);

    let (status, size) =
        ffi_estimate_u128(&mut store, &hashes, total, estimate_get_chunks_u128);

    assert_eq!(status, status_code(FFIStatus::StatusOk));
    assert_eq!(size, expected);
}

#[test]
fn test_compression_reports_a_failing_callback() {
    let mut store: ChunkStore<u64> = HashMap::new();
    let (status, _) = ffi_estimate_u64(&mut store, &[1, 2], 8, chunks_fail::<u64>);
    assert_eq!(status, status_code(FFIStatus::CallbackError));

    let mut store: ChunkStore<u128> = HashMap::new();
    let (status, _) = ffi_estimate_u128(&mut store, &[1, 2], 8, chunks_fail::<u8>);
    assert_eq!(status, status_code(FFIStatus::CallbackError));
}

#[test]
fn test_compression_reports_a_cancelling_callback() {
    let mut store: ChunkStore<u64> = HashMap::new();
    let (status, _) = ffi_estimate_u64(&mut store, &[1, 2], 8, chunks_cancel::<u64>);
    assert_eq!(status, status_code(FFIStatus::StatusCancelled));

    let mut store: ChunkStore<u128> = HashMap::new();
    let (status, _) =
        ffi_estimate_u128(&mut store, &[1, 2], 8, chunks_cancel::<u8>);
    assert_eq!(status, status_code(FFIStatus::StatusCancelled));
}

// Configuring and releasing a builder

unsafe extern "C" fn free_nothing(
    _user_data: *mut c_void,
    _chunks: FFIChunkDataArray,
) {}

unsafe extern "C" fn write_nothing(
    _user_data: *mut c_void,
    _data: *const u8,
    _data_len: usize,
) -> FFICallbackStatus {
    FFICallbackStatus::Error
}

extern "C" fn ignore_progress(_progress: FFIProgress, _user_data: *mut c_void) {}

fn assert_ok(status: FFIStatus) {
    assert_eq!(status_code(status), status_code(FFIStatus::StatusOk));
}

/// Arguments for a u64 builder that's configured but never built, so its
/// callbacks don't run.
fn unbuilt_builder_args_u64(
    input: &HostInput<u64>,
    hashes: &[u64],
) -> ArchiveBuilderArgsU64 {
    ArchiveBuilderArgsU64 {
        manifest_array_ptr: input.manifests.as_ptr(),
        manifest_len: input.manifests.len(),
        sorted_hashes_array_ptr: hashes.as_ptr(),
        sorted_hashes_len: hashes.len(),
        file_count: u32::try_from(input.manifests.len()).unwrap(),
        total_size: 12,
        user_data: ptr::null_mut(),
        get_chunks_cb: chunks_fail::<u64>,
        free_chunks_cb: free_nothing,
        write_comp_data_cb: write_nothing,
    }
}

/// The u128 form of [`unbuilt_builder_args_u64`]. `hashes` holds 16
/// little-endian bytes per hash.
fn unbuilt_builder_args_u128(
    input: &HostInput<U128Bytes>,
    hashes: &[u8],
) -> ArchiveBuilderArgsU128 {
    ArchiveBuilderArgsU128 {
        manifest_array_ptr: input.manifests.as_ptr(),
        manifest_len: input.manifests.len(),
        sorted_hashes_array_ptr: hashes.as_ptr(),
        sorted_hashes_len: hashes.len() / size_of::<u128>(),
        file_count: u32::try_from(input.manifests.len()).unwrap(),
        total_size: 12,
        user_data: ptr::null_mut(),
        get_chunks_cb: chunks_fail::<u8>,
        free_chunks_cb: free_nothing,
        write_comp_data_cb: write_nothing,
    }
}

#[test]
fn a_u64_builder_can_be_configured_and_freed_without_building() {
    let input = host_input::<u64, u64>(SEEK_HASHES_U64);
    let hashes = SEEK_HASHES_U64;
    let args = unbuilt_builder_args_u64(&input, &hashes);
    let mut builder: *mut ArchiveBuilderU64 = ptr::null_mut();

    // SAFETY: `args` points into `input` and `hashes`, which outlive the
    // builder, and `builder` is a local.
    assert_ok(unsafe {
        archive_builder_new_u64(&raw const args, &raw mut builder)
    });
    // SAFETY: `builder` came from `archive_builder_new_u64` and isn't freed
    // until the end of this test.
    assert_ok(unsafe { archive_builder_set_compression_level_u64(builder, 3) });
    // SAFETY: `builder` came from `archive_builder_new_u64` and isn't freed
    // until the end of this test.
    assert_ok(unsafe { archive_builder_set_dictionary_size_u64(builder, 1024) });
    // SAFETY: `builder` came from `archive_builder_new_u64` and isn't freed
    // until the end of this test.
    assert_ok(unsafe { archive_builder_set_worker_count_u64(builder, 2) });
    // SAFETY: `builder` came from `archive_builder_new_u64` and isn't freed
    // until the end of this test.
    assert_ok(unsafe { archive_builder_set_optimize_dictionary_u64(builder, true) });
    // SAFETY: `builder` came from `archive_builder_new_u64` and isn't freed
    // until the end of this test. `ignore_progress` never reads the null
    // `user_data`, and this builder is never built, so it isn't called.
    assert_ok(unsafe {
        archive_builder_set_c_progress_u64(
            builder,
            ignore_progress,
            ptr::null_mut(),
        )
    });
    // SAFETY: `builder` hasn't been built, and is freed once, here.
    unsafe { archive_builder_free_u64(builder) };
}

#[test]
fn a_u128_builder_can_be_configured_and_freed_without_building() {
    let input = host_input::<u128, U128Bytes>(SEEK_HASHES_U128);
    let hashes: Vec<u8> = SEEK_HASHES_U128
        .iter()
        .flat_map(|hash| hash.to_le_bytes())
        .collect();
    let args = unbuilt_builder_args_u128(&input, &hashes);
    let mut builder: *mut ArchiveBuilderU128 = ptr::null_mut();

    // SAFETY: `args` points into `input` and `hashes`, which outlive the
    // builder, and `builder` is a local.
    assert_ok(unsafe {
        archive_builder_new_u128(&raw const args, &raw mut builder)
    });
    // SAFETY: `builder` came from `archive_builder_new_u128` and isn't freed
    // until the end of this test.
    assert_ok(unsafe { archive_builder_set_compression_level_u128(builder, 3) });
    // SAFETY: `builder` came from `archive_builder_new_u128` and isn't freed
    // until the end of this test.
    assert_ok(unsafe { archive_builder_set_dictionary_size_u128(builder, 1024) });
    // SAFETY: `builder` came from `archive_builder_new_u128` and isn't freed
    // until the end of this test.
    assert_ok(unsafe { archive_builder_set_worker_count_u128(builder, 2) });
    // SAFETY: `builder` came from `archive_builder_new_u128` and isn't freed
    // until the end of this test.
    assert_ok(unsafe { archive_builder_set_optimize_dictionary_u128(builder, true) });
    // SAFETY: `builder` came from `archive_builder_new_u128` and isn't freed
    // until the end of this test. `ignore_progress` never reads the null
    // `user_data`, and this builder is never built, so it isn't called.
    assert_ok(unsafe {
        archive_builder_set_c_progress_u128(
            builder,
            ignore_progress,
            ptr::null_mut(),
        )
    });
    // SAFETY: `builder` hasn't been built, and is freed once, here.
    unsafe { archive_builder_free_u128(builder) };
}

#[test]
fn archive_builder_free_accepts_null() {
    // SAFETY: `archive_builder_free_u64` ignores a null pointer.
    unsafe { archive_builder_free_u64(ptr::null_mut()) };
    // SAFETY: `archive_builder_free_u128` ignores a null pointer.
    unsafe { archive_builder_free_u128(ptr::null_mut()) };
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn the_optimize_dictionary_setter_changes_the_build_like_the_rust_api() {
    let files = round_trip_files();

    let archive = ffi_round_trip_archive::<u64>(&files, |builder| {
        // SAFETY: `builder` is the live builder the round trip passes in.
        assert_ok(unsafe {
            archive_builder_set_optimize_dictionary_u64(builder, true)
        });
    });

    assert_bytes_eq(
        &archive,
        &rust_round_trip_archive::<u64>(&files, true),
        "archive built with an optimized dictionary",
    );
}

/// `(progress type, total chunks)` for each progress report.
type ProgressLog = Mutex<Vec<(i32, u64)>>;

extern "C" fn record_progress(progress: FFIProgress, user_data: *mut c_void) {
    // SAFETY: the test below passes a `ProgressLog` that outlives the build.
    let log = unsafe { &*user_data.cast::<ProgressLog>() };
    log.lock().unwrap().push((progress.ty as i32, progress.total_chunks));
}

#[test]
#[cfg_attr(miri, ignore = "zstd is C code, which Miri can't run")]
fn the_progress_callback_reports_each_stage_of_a_build() {
    let log: ProgressLog = Mutex::new(Vec::new());

    ffi_round_trip_archive::<u64>(&round_trip_files(), |builder| {
        // SAFETY: `builder` is the live builder the round trip passes in,
        // and `log` outlives the build.
        assert_ok(unsafe {
            archive_builder_set_c_progress_u64(
                builder,
                record_progress,
                ptr::from_ref(&log).cast_mut().cast(),
            )
        });
    });

    let log = log.into_inner().unwrap();
    let stage = |ty: FFIProgressType| ty as i32;
    let of = |wanted: i32| log.iter().filter(|(ty, _)| *ty == wanted).count();
    let compressing = log.iter()
        .find(|(ty, _)| *ty == stage(FFIProgressType::Compressing))
        .expect("a Compressing report");

    assert_eq!(log[0].0, stage(FFIProgressType::GeneratingDictionary));
    assert_eq!(log[1].0, stage(FFIProgressType::DictionaryDone));
    assert_eq!(log[2], *compressing);
    assert!(compressing.1 > 0);
    assert_eq!(
        of(stage(FFIProgressType::ChunkCompressed)) as u64,
        compressing.1
    );
    assert_eq!(log[log.len() - 1].0, stage(FFIProgressType::Finalizing));
}

// Null arguments
//
// A case table builds each struct with `Args { field: ptr::null(), ..args }`.
// The struct isn't `Copy`, but `..args` only reads the remaining fields, and
// those are all `Copy` pointers, so `args` stays usable for the next row.

/// A null `chunk_metadata` is rejected at both of these lengths. A slice
/// can't be built from a null pointer even for a length of 0.
const NULL_METADATA_LENGTHS: [usize; 2] = [0, 3];

fn manifest_without_metadata<F>(len: usize) -> FFIFileManifestParent<F> {
    FFIFileManifestParent {
        chunk_metadata: ptr::null(),
        chunk_metadata_len: len,
        chunk_metadata_cap: len,
    }
}

fn assert_null_argument(what: &str, status: FFIStatus) {
    assert_eq!(
        status_code(status),
        status_code(FFIStatus::NullArgument),
        "null {what}"
    );
}

/// Checks an export taking only an arguments struct: a null struct, then
/// each row of `cases`, which has one field set to null.
fn assert_args_rejected<A>(
    export: unsafe extern "C" fn(*const A) -> FFIStatus,
    cases: &[(&str, A)],
) {
    // SAFETY: the export checks the struct pointer before reading it.
    assert_null_argument("args", unsafe { export(ptr::null()) });

    for (what, args) in cases {
        // SAFETY: `args` is live for the call, and the export returns at its
        // null field without reading through any null pointer.
        assert_null_argument(what, unsafe { export(ptr::from_ref(args)) });
    }
}

/// Checks an export taking an arguments struct and an out-pointer: each of
/// those null, then each row of `cases`, which has one field set to null.
fn assert_args_and_out_rejected<A, O>(
    export: unsafe extern "C" fn(*const A, *mut O) -> FFIStatus,
    valid: &A,
    cases: &[(&str, A)],
) {
    // Uninitialized is enough: every call here has to return before writing
    // to it, and it's never read.
    let mut out_slot = MaybeUninit::<O>::uninit();
    let out = out_slot.as_mut_ptr();

    // SAFETY: the export checks the struct pointer before reading it, and
    // `out` is a local that it doesn't reach.
    assert_null_argument("args", unsafe { export(ptr::null(), out) });
    // SAFETY: `valid` is live for the call, and the export checks the
    // out-pointer before using anything in it.
    assert_null_argument("out_ptr", unsafe {
        export(ptr::from_ref(valid), ptr::null_mut())
    });

    for (what, args) in cases {
        // SAFETY: `args` is live for the call, and the export returns at its
        // null field without writing to `out`.
        assert_null_argument(what, unsafe { export(ptr::from_ref(args), out) });
    }
}

/// Checks a builder setter with a null handle.
fn assert_setter_rejects_null<B, V>(
    what: &str,
    set: unsafe extern "C" fn(*mut B, V) -> FFIStatus,
    value: V,
) {
    // SAFETY: the setter checks the handle before using it.
    assert_null_argument(what, unsafe { set(ptr::null_mut(), value) });
}

#[test]
fn create_file_manifest_and_chunks_rejects_each_null_pointer() {
    let name = c"file.bin";
    let data = [0_u8; 4];
    // Never read: every call here returns before looking at the chunks.
    let args = CreateFileManifestAndChunksArgs {
        file_name_ptr: name.as_ptr(),
        file_data_array_ptr: data.as_ptr(),
        file_data_len: data.len(),
        chunks_array_ptr: ptr::dangling(),
        chunks_len: 0,
    };
    let cases = [
        (
            "file name",
            CreateFileManifestAndChunksArgs {
                file_name_ptr: ptr::null(),
                ..args
            },
        ),
        (
            "file data",
            CreateFileManifestAndChunksArgs {
                file_data_array_ptr: ptr::null(),
                ..args
            },
        ),
        (
            "chunks",
            CreateFileManifestAndChunksArgs {
                chunks_array_ptr: ptr::null(),
                ..args
            },
        ),
    ];

    assert_args_and_out_rejected(
        create_file_manifest_and_chunks_u64,
        &args,
        &cases,
    );
    assert_args_and_out_rejected(
        create_file_manifest_and_chunks_u128,
        &args,
        &cases,
    );
}

#[test]
fn process_file_in_memory_rejects_each_null_pointer() {
    let name = c"file.bin";
    let data = [0_u8; 4];
    let file = FFIFileData {
        filename: name.as_ptr(),
        file_data: data.as_ptr(),
        file_data_len: data.len(),
    };
    let mut out_slot = ptr::null_mut();
    let out = &raw mut out_slot;

    let calls = [
        ("filename", FFIFileData { filename: ptr::null(), ..file }, out),
        ("file data", FFIFileData { file_data: ptr::null(), ..file }, out),
        ("out_ptr", FFIFileData { ..file }, ptr::null_mut()),
    ];
    for (what, file, out) in calls {
        // SAFETY: one pointer is null, which the function checks before
        // reading anything; the others point into live locals.
        let status =
            unsafe { process_file_in_memory_ffi(file, WINDOW_SIZE, out) };
        assert_null_argument(what, status);
    }
    assert!(out_slot.is_null());
}

#[test]
fn verify_single_file_u64_rejects_each_null_pointer() {
    let input = host_input::<u64, u64>(SEEK_HASHES_U64);
    let host = VerifyHost::new(SEEK_HASHES_U64);
    let (_, veri_hash) = file_and_hash(0);
    let args = VerifySingleFileArgsU64 {
        filename: input.name_ptrs[0],
        file_manifest_parent: &raw const input.manifests[0],
        veri_hash_array_ptr: veri_hash.as_ptr(),
        user_data: ptr::from_ref(&host).cast_mut().cast(),
        get_chunks_cb: verify_get_chunks_u64,
        progress_cb: verify_progress::<u64>,
    };
    let no_metadata = NULL_METADATA_LENGTHS.map(manifest_without_metadata);

    assert_args_rejected(
        verify_single_file_u64,
        &[
            ("filename", VerifySingleFileArgsU64 {
                filename: ptr::null(),
                ..args
            }),
            ("manifest", VerifySingleFileArgsU64 {
                file_manifest_parent: ptr::null(),
                ..args
            }),
            ("hash", VerifySingleFileArgsU64 {
                veri_hash_array_ptr: ptr::null(),
                ..args
            }),
            ("chunk metadata, empty", VerifySingleFileArgsU64 {
                file_manifest_parent: &raw const no_metadata[0],
                ..args
            }),
            ("chunk metadata", VerifySingleFileArgsU64 {
                file_manifest_parent: &raw const no_metadata[1],
                ..args
            }),
        ],
    );
    assert_eq!(host.progress.load(Ordering::Relaxed), 0);
}

#[test]
fn verify_single_file_u128_rejects_each_null_pointer() {
    let input = host_input::<u128, U128Bytes>(SEEK_HASHES_U128);
    let host = VerifyHost::new(SEEK_HASHES_U128);
    let (_, veri_hash) = file_and_hash(0);
    let args = VerifySingleFileArgsU128 {
        filename: input.name_ptrs[0],
        file_manifest_parent: &raw const input.manifests[0],
        veri_hash_array_ptr: veri_hash.as_ptr(),
        user_data: ptr::from_ref(&host).cast_mut().cast(),
        get_chunks_cb: verify_get_chunks_u128,
        progress_cb: verify_progress::<u128>,
    };
    let no_metadata = NULL_METADATA_LENGTHS.map(manifest_without_metadata);

    assert_args_rejected(
        verify_single_file_u128,
        &[
            ("filename", VerifySingleFileArgsU128 {
                filename: ptr::null(),
                ..args
            }),
            ("manifest", VerifySingleFileArgsU128 {
                file_manifest_parent: ptr::null(),
                ..args
            }),
            ("hash", VerifySingleFileArgsU128 {
                veri_hash_array_ptr: ptr::null(),
                ..args
            }),
            ("chunk metadata, empty", VerifySingleFileArgsU128 {
                file_manifest_parent: &raw const no_metadata[0],
                ..args
            }),
            ("chunk metadata", VerifySingleFileArgsU128 {
                file_manifest_parent: &raw const no_metadata[1],
                ..args
            }),
        ],
    );
    assert_eq!(host.progress.load(Ordering::Relaxed), 0);
}

#[test]
fn test_compression_rejects_each_null_pointer() {
    let hashes = SEEK_HASHES_U64;
    let args = TestCompressionArgsU64 {
        total_data_size: 12,
        sorted_hashes_array_ptr: hashes.as_ptr(),
        sorted_hashes_len: hashes.len(),
        worker_count: WORKER_COUNT,
        dictionary_size: 1024,
        compression_level: COMPRESSION_LEVEL,
        user_data: ptr::null_mut(),
        get_chunks_cb: chunks_fail::<u64>,
    };
    assert_args_and_out_rejected(
        test_compression_u64,
        &args,
        &[("hashes", TestCompressionArgsU64 {
            sorted_hashes_array_ptr: ptr::null(),
            ..args
        })],
    );

    let hash_bytes = [0_u8; 16];
    let args = TestCompressionArgsU128 {
        total_data_size: 12,
        sorted_hashes_array_ptr: hash_bytes.as_ptr(),
        sorted_hashes_len: 1,
        worker_count: WORKER_COUNT,
        dictionary_size: 1024,
        compression_level: COMPRESSION_LEVEL,
        user_data: ptr::null_mut(),
        get_chunks_cb: chunks_fail::<u8>,
    };
    assert_args_and_out_rejected(
        test_compression_u128,
        &args,
        &[("hashes", TestCompressionArgsU128 {
            sorted_hashes_array_ptr: ptr::null(),
            ..args
        })],
    );
}

fn assert_get_seek_rejects_null<F>(
    get_seek: GetSeek<F>,
    input: &HostInput<F>,
) {
    let mut out_slot = ptr::null_mut();
    let out = &raw mut out_slot;
    let manifest = &raw const input.manifests[0];

    let calls = [
        ("manifest", ptr::null(), out),
        ("out_ptr", manifest, ptr::null_mut()),
    ];
    for (what, manifest, out) in calls {
        // SAFETY: one pointer is null, which the function checks before
        // using either; the other points into `input` or a local.
        assert_null_argument(what, unsafe { get_seek(manifest, 0, 1, out) });
    }

    for len in NULL_METADATA_LENGTHS {
        let manifest = manifest_without_metadata::<F>(len);
        // SAFETY: `manifest` and `out` are locals. The function rejects the
        // null `chunk_metadata` before reading through it.
        let status = unsafe { get_seek(&raw const manifest, 0, 1, out) };
        assert_null_argument("chunk metadata", status);
    }
    assert!(out_slot.is_null());
}

#[test]
fn get_seek_chunks_rejects_each_null_pointer() {
    assert_get_seek_rejects_null(
        get_seek_chunks_u64,
        &host_input::<u64, u64>(SEEK_HASHES_U64),
    );
    assert_get_seek_rejects_null(
        get_seek_chunks_u128,
        &host_input::<u128, U128Bytes>(SEEK_HASHES_U128),
    );
}

#[test]
fn lookup_chunk_location_rejects_each_null_pointer() {
    let map: HashMap<u64, ChunkLocation> = HashMap::new();
    let map = ptr::from_ref(&map).cast::<c_void>();
    let key = [0_u8; 16];
    let mut location = FFIChunkLocation { offset: 0, length: 0 };
    let out = &raw mut location;

    for (what, map, out) in [
        ("map", ptr::null(), out),
        ("location", map, ptr::null_mut()),
    ] {
        // SAFETY: one pointer is null, which the function checks before
        // using either.
        let status = unsafe { lookup_chunk_location_u64(map, 1, out) };
        assert_null_argument(what, status);

        // SAFETY: one of `map` and `out` is null, which the function checks
        // before using either, and `key` is 16 readable bytes.
        let status = unsafe {
            lookup_chunk_location_u128(map, key.as_ptr(), out)
        };
        assert_null_argument(what, status);
    }
}

#[test]
fn decompress_chunk_rejects_each_null_pointer() {
    let compressed = [0_u8; 4];
    let dictionary = [0_u8; 4];
    let mut decompressed = [0_u8; 4];
    let (comp, dict) = (compressed.as_ptr(), dictionary.as_ptr());
    let out = decompressed.as_mut_ptr();

    let calls = [
        ("compressed data", ptr::null(), dict, out),
        ("dictionary", comp, ptr::null(), out),
        ("output buffer", comp, dict, ptr::null_mut()),
    ];
    for (what, comp, dict, out) in calls {
        // SAFETY: one pointer is null, which the function checks before
        // reading anything; the others point to 4 bytes in live locals.
        let status = unsafe { decompress_chunk_c(comp, 4, dict, 4, out, 4) };
        assert_null_argument(what, status);
    }
}

#[test]
fn serialize_rejects_each_null_pointer() {
    let input = host_input::<u64, u64>(SEEK_HASHES_U64);
    let args = SerializeUncompressedDataArgsU64 {
        manifest_array_ptr: input.manifests.as_ptr(),
        filenames: input.name_ptrs.as_ptr(),
        manifest_len: input.manifests.len(),
    };
    assert_args_and_out_rejected(
        serialize_uncompressed_data_u64,
        &args,
        &[
            ("manifests", SerializeUncompressedDataArgsU64 {
                manifest_array_ptr: ptr::null(),
                ..args
            }),
            ("filenames", SerializeUncompressedDataArgsU64 {
                filenames: ptr::null(),
                ..args
            }),
        ],
    );

    let input = host_input::<u128, U128Bytes>(SEEK_HASHES_U128);
    let args = SerializeUncompressedDataArgsU128 {
        manifest_array_ptr: input.manifests.as_ptr(),
        filenames: input.name_ptrs.as_ptr(),
        manifest_len: input.manifests.len(),
    };
    assert_args_and_out_rejected(
        serialize_uncompressed_data_u128,
        &args,
        &[
            ("manifests", SerializeUncompressedDataArgsU128 {
                manifest_array_ptr: ptr::null(),
                ..args
            }),
            ("filenames", SerializeUncompressedDataArgsU128 {
                filenames: ptr::null(),
                ..args
            }),
        ],
    );
}

#[test]
fn serialize_rejects_null_chunk_metadata() {
    for len in NULL_METADATA_LENGTHS {
        let mut input = host_input::<u64, u64>(SEEK_HASHES_U64);
        input.manifests[1] = manifest_without_metadata(len);
        let (status, out) = ffi_serialize_u64(&input, |_| {});
        assert_null_argument("chunk metadata", status);
        assert!(out.is_null());

        let mut input = host_input::<u128, U128Bytes>(SEEK_HASHES_U128);
        input.manifests[1] = manifest_without_metadata(len);
        let (status, out) = ffi_serialize_u128(&input, |_| {});
        assert_null_argument("chunk metadata", status);
        assert!(out.is_null());
    }
}

#[test]
fn build_ssmc_prefix_rejects_null_chunk_metadata() {
    for len in NULL_METADATA_LENGTHS {
        let mut input = host_input::<u64, u64>(SEEK_HASHES_U64);
        input.manifests[1] = manifest_without_metadata(len);
        let (status, out) =
            ffi_build_prefix(build_ssmc_prefix_u64, &input, |_, _| {});
        assert_null_argument("chunk metadata", status);
        assert!(out.is_null());

        let mut input = host_input::<u128, U128Bytes>(SEEK_HASHES_U128);
        input.manifests[1] = manifest_without_metadata(len);
        let (status, out) =
            ffi_build_prefix(build_ssmc_prefix_u128, &input, |_, _| {});
        assert_null_argument("chunk metadata", status);
        assert!(out.is_null());
    }
}

#[test]
fn archive_builder_new_rejects_each_null_pointer() {
    let input = host_input::<u64, u64>(SEEK_HASHES_U64);
    let args = unbuilt_builder_args_u64(&input, &SEEK_HASHES_U64);
    assert_args_and_out_rejected(
        archive_builder_new_u64,
        &args,
        &[
            ("manifests", ArchiveBuilderArgsU64 {
                manifest_array_ptr: ptr::null(),
                ..args
            }),
            ("hashes", ArchiveBuilderArgsU64 {
                sorted_hashes_array_ptr: ptr::null(),
                ..args
            }),
        ],
    );

    let input = host_input::<u128, U128Bytes>(SEEK_HASHES_U128);
    let hash_bytes = [0_u8; 16];
    let args = unbuilt_builder_args_u128(&input, &hash_bytes);
    assert_args_and_out_rejected(
        archive_builder_new_u128,
        &args,
        &[
            ("manifests", ArchiveBuilderArgsU128 {
                manifest_array_ptr: ptr::null(),
                ..args
            }),
            ("hashes", ArchiveBuilderArgsU128 {
                sorted_hashes_array_ptr: ptr::null(),
                ..args
            }),
        ],
    );
}

#[test]
fn archive_builder_setters_reject_a_null_handle() {
    assert_setter_rejects_null(
        "level",
        archive_builder_set_compression_level_u64,
        3,
    );
    assert_setter_rejects_null(
        "level",
        archive_builder_set_compression_level_u128,
        3,
    );
    assert_setter_rejects_null(
        "dictionary size",
        archive_builder_set_dictionary_size_u64,
        1024,
    );
    assert_setter_rejects_null(
        "dictionary size",
        archive_builder_set_dictionary_size_u128,
        1024,
    );
    assert_setter_rejects_null(
        "workers",
        archive_builder_set_worker_count_u64,
        2,
    );
    assert_setter_rejects_null(
        "workers",
        archive_builder_set_worker_count_u128,
        2,
    );
    assert_setter_rejects_null(
        "optimize",
        archive_builder_set_optimize_dictionary_u64,
        true,
    );
    assert_setter_rejects_null(
        "optimize",
        archive_builder_set_optimize_dictionary_u128,
        true,
    );

    // SAFETY: the setter checks the handle before storing the callback.
    let status = unsafe {
        archive_builder_set_c_progress_u64(
            ptr::null_mut(),
            ignore_progress,
            ptr::null_mut(),
        )
    };
    assert_null_argument("progress", status);
    // SAFETY: the setter checks the handle before storing the callback.
    let status = unsafe {
        archive_builder_set_c_progress_u128(
            ptr::null_mut(),
            ignore_progress,
            ptr::null_mut(),
        )
    };
    assert_null_argument("progress", status);
}

#[test]
fn archive_builder_build_rejects_each_null_pointer_and_keeps_the_builder() {
    let input = host_input::<u64, u64>(SEEK_HASHES_U64);
    let args = unbuilt_builder_args_u64(&input, &SEEK_HASHES_U64);
    let mut builder: *mut ArchiveBuilderU64 = ptr::null_mut();
    // SAFETY: `args` points into `input`, which outlives the builder, and
    // `builder` is a local.
    assert_ok(unsafe {
        archive_builder_new_u64(&raw const args, &raw mut builder)
    });
    let mut out = ptr::null_mut();
    // SAFETY: the null handle is checked before anything else, and `out` is
    // a local.
    let status = unsafe {
        archive_builder_build_u64(ptr::null_mut(), &raw mut out)
    };
    assert_null_argument("handle", status);
    // SAFETY: `builder` is live, and the null out-pointer is checked before
    // the handle is consumed.
    let status = unsafe { archive_builder_build_u64(builder, ptr::null_mut()) };
    assert_null_argument("out_ptr", status);
    // SAFETY: the rejected build didn't consume `builder`, so it's freed
    // once, here. Miri reports a double free if the build did consume it.
    unsafe { archive_builder_free_u64(builder) };

    let input = host_input::<u128, U128Bytes>(SEEK_HASHES_U128);
    let hash_bytes = [0_u8; 16];
    let args = unbuilt_builder_args_u128(&input, &hash_bytes);
    let mut builder: *mut ArchiveBuilderU128 = ptr::null_mut();
    // SAFETY: `args` points into `input` and `hash_bytes`, which outlive the
    // builder, and `builder` is a local.
    assert_ok(unsafe {
        archive_builder_new_u128(&raw const args, &raw mut builder)
    });
    // SAFETY: the null handle is checked before anything else, and `out` is
    // a local.
    let status = unsafe {
        archive_builder_build_u128(ptr::null_mut(), &raw mut out)
    };
    assert_null_argument("handle", status);
    // SAFETY: `builder` is live, and the null out-pointer is checked before
    // the handle is consumed.
    let status = unsafe { archive_builder_build_u128(builder, ptr::null_mut()) };
    assert_null_argument("out_ptr", status);
    // SAFETY: the rejected build didn't consume `builder`, so it's freed
    // once, here. Miri reports a double free if the build did consume it.
    unsafe { archive_builder_free_u128(builder) };
    assert!(out.is_null());
}

#[test]
fn remaining_frees_accept_null() {
    // SAFETY: `free_file_manifest_and_chunks_u64` returns early on null.
    unsafe { free_file_manifest_and_chunks_u64(ptr::null_mut()) };
    // SAFETY: `free_file_manifest_and_chunks_u128` returns early on null.
    unsafe { free_file_manifest_and_chunks_u128(ptr::null_mut()) };
    // SAFETY: `free_processed_file_data_ffi` returns early on null.
    unsafe { free_processed_file_data_ffi(ptr::null_mut()) };
    // SAFETY: `archive_data_free` returns early on null.
    unsafe { archive_data_free(ptr::null_mut()) };
}
