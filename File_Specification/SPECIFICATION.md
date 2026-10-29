# SpriteShrink Archive Specification

## 1. Overview

This document specifies the binary layout for the SpriteShrink archive formats. SpriteShrink supports two distinct format types optimized for different use cases:

   1.  **SpriteShrink Multi-Cart (SSMC):** Optimized for collections of discrete files (e.g., ROM sets) with high redundancy.
   2.  **SpriteShrink Multi-Disc (SSMD):** Optimized for optical disc images (CD-ROM based), handling raw sector data, audio tracks, and error correction data.

Both formats use a common **Header** and **Table of Contents (TOC)** structure, diverging at the **Format Data** block.

### Logical Layout
+-----------------------------------+
|          Header (Fixed)           | 36 Bytes
+-----------------------------------+
|      Table of Contents (Var)      | Compressed
+-----------------------------------+
|      Format Data Block (Fixed)    | Size depends on Format ID
+-----------------------------------+
|        ... Data Sections ...      | Pointed to by Format Data
| (Manifests, Indexes, Dictionaries)|
+-----------------------------------+
|          Data Blob(s)             | Compressed chunks/streams
+-----------------------------------+

## 2. Common Structures

### 2.1. File Header (Fixed Size: 36 Bytes)

The header appears at offset 0. It identifies the file type, version, and the format used (SSMC vs. SSMD). It points to the Table of Contents and the Format Data block.

| Offset | Type | Field | Description |
| :--- | :--- | :--- | :--- |
| 0 | `[u8; 8]` | `magic_num` | Magic bytes. Default: `b"SSARCHV1"` |
| 8 | `u32` | `file_version` | Archive format version (e.g., `0x00020000`). |
| 12 | `u32` | `file_count` | Total number of files/discs in the archive. |
| 16 | `u16` | `algorithm` | Compresson algorithm ID (e.g., 98 = Zstd). |
| 18 | `u8` | `hash_type` | Hash type ID (e.g., 1 = xxhash3_64, 2 = xxhash3_128). |
| 19 | `u8` | `_pad1` | Padding (Zero). |
| 20 | `u16` | `format_id` | **Format Identifier**: <br>`0x0000` = SSMC (Multi-Cart) <br>`0xd541` = SSMD (Multi-Disc) |
| 22 | `[u8; 2]` | `_pad2` | Padding (Zero). |
| 24 | `u32` | `enc_toc_offset` | Offset to the start of the compressed TOC. |
| 28 | `u32` | `enc_toc_length` | Length of the compressed TOC in bytes. |
| 32 | `u32` | `format_data_offset` | Offset to the start of the **Format Data Block**. |

### 2.2. File Region

A helper struct used within the Format Data blocks to define absolute positions of sections.

| Type | Field | Description |
| :--- | :--- | :--- |
| `u64` | `offset` | Absolute byte offset. |
| `u64` | `length` | Length in bytes. |
   
---

## 3. Format Data Blocks

The `format_data_offset` in the header points to one of the following structures, depending on the `format_id`.

### 3.1. SSMC Format Data (`format_id = 0x0000`)

Used for standard file archives.

| Type | Field | Description |
| :--- | :--- | :--- |
| `FileRegion` | `enc_manifest` | Location of the compressed File Manifest. |
| `FileRegion` | `data_dictionary` | Location of the compression dictionary. |
| `FileRegion` | `enc_chunk_index` | Location of the compressed Chunk Index. |
| `u64` | `data_offset` | Absolute offst where the Chunk Data Blob begins. |

### 3.2. SSMD Format Data (`format_id = 0xd541`)

Used for optical disc images.

| Type | Field | Description |
| :--- | :--- | :--- |
| `FileRegion` | `enc_disc_manifest` | Location of the compressed Disc Manifest. |
| `FileRegion` | `data_dictionary` | Location of the compression dictionary. |
| `FileRegion` | `enc_chunk_index` | Location of the Data Chunk Index (for data tracks). |
| `FileRegion` | `enc_audio_index` | Location of the Audio Block Index (for CD-DA). |
| `FileRegion` | `enc_excep_index` | Location of the Exception Index (ECC/EDC overrides). |
| `FileRegion` | `subheader_table` | Location of the Subheader dedup table. |
| `FileRegion` | `exception_data` | Location of raw exception data blob. |
| `FileRegion` | `audio_data` | Location of FLAC-compressed audio blob. |
| `u64` | `data_offset` | Absolute offset where the Data Chunk Blob begins. |

---

## 4. Section Details

### 4.1. Table of Contents (TOC)

A serialized vector containing basic metadata for listing archive contents without decoding full manifests.

*   **SSMC:** `Vec<SSMCTocEntry>`
  struct SSMCTocEntry {
      filename: String,
      uncompressed_size: u64,
  }
*   **SSMD:** `Vec<SSMDTocEntry>`
  struct SSMDTocEntry {
      filename: String,
      collection_id: u8, // Group ID for multi-disc sets
      uncompressed_size: u64,
  }


### 4.2. SSMC Manifests & Indexes

*   **File Manifest:** Serialized `Vec<FileManifestParent<H>>`.
  struct FileManifestParent<H> {
      chunk_count: u64,
      chunk_metadata: Vec<SSAChunkMeta<H>>,
  }

  struct SSAChunkMeta<H> {
      hash: H,
      offset: u64,
      length: u32,
  }
*   **Chunk Index:** Serialized `HashMap<H, ChunkLocation>`.
  struct ChunkLocation {
      offset: u64,           // Relative to data_offset
      compressed_length: u32,
  }

### 4.3. SSMD Disc Manifests

SSMD uses a complex manifest to reconstruct CD sectors.

*   **Disc Manifest:** Serialized `Vec<DiscManifest<H>>`.
  struct DiscManifest<H> {
      lba_map: Vec<(u32, u32)>,           // Logical Block Address mapping
      rle_sector_map: RleSectorMap,       // Run-Length Encoded Sector Types
      audio_block_map: Vec<ContentBlock<H>>, // CD-DA Audio tracks
      data_stream_layout: Vec<DataChunkLayout<H>>, // Mode1/Mode2 Data tracks
      subheader_index: Vec<SubHeaderEntry>, // CD-XA Subheaders
      disc_exception_index: Vec<(u32, u32)>, // Index into Exception table
      integrity_hash: u64,
  }


*   **Sector Types:**
    Common types include `Audio` (2352 bytes), `Mode1` (2048 bytes data), `Mode2Form1`, `Mode2Form2`.

*   **Streams vs Blocks:**
    *   **Data (Mode1/Mode2):** Stored as a deduplicated stream of chunks (`data_stream_layout`), reconstructed using the `chunk_index`.
    *   **Audio:** Stored as "blocks" (typically full tracks or large segments) compressed with FLAC, referenced by `audio_block_map` and `enc_audio_index`.

## 5. Read Logic Summary

1.  **Read Header (0-36 bytes)**.
2.  Check `format_id`.
3.  Read **Format Data** at `format_data_offset`. Cast to `SSMCFormatData` or `SSMDFormatData` based on ID.
4.  **To List Files:**
    *   Go to `enc_toc_offset`, read `enc_toc_length` bytes.
    *   Decompress/Deserialize to get file list.
5.  **To Extract File (SSMC):**
    *   Read **Format Data** at `format_data_offset`. Cast to `SSMCFormatData`.
    *   Read `enc_manifest`, `data_dictionary`, `enc_chunk_index` using offsets/lengths from Format Data.
    *   Reconstruct file by iterating manifest chunks and fetching from `data_offset`.
6.  **To Extract Disc (SSMD):**
    *   Read **Format Data** at `format_data_offset`. Cast to `SSMDFormatData`.
    *   Read `enc_disc_manifest`, `enc_audio_index`, etc.
    *   Reconstruct CD sectors by traversing `rle_sector_map`.
    *   If Sector is Data: Fetch chunks from `data_offset`.
    *   If Sector is Audio: Fetch block from `audio_data`, decode FLAC.
    *   Apply Exceptions (ECC/EDC) from `exception_data` if needed.
