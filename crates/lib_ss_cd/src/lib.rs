mod analyze;
pub use analyze::{
    FileSizeProvider, analyze_cue_sheets, resolve_file_sector_counts
};

mod cue_parser;

mod decode;
pub use decode::{
    decode_ssmd_format_data, decode_ssmd_meta_data
};

mod ecc;

mod flac;
pub use flac::{
    compress_audio_block, compress_audio_blocks, decompress_flac_block
};

mod lib_error_handling;
pub use lib_error_handling::SpriteShrinkCDError;

mod lib_structs;
pub use lib_structs::{
    SSMD_UID,
    CueSheet, ContentBlock, DataChunkLayout, DiscManifest,
    ExceptionInfo, ExceptionRegistry, ExceptionType, RleSectorMap, SectorMap,
    SectorType, SSMDFormatData, SSMDTocEntry, SubHeaderEntry, SubheaderRegistry
};

mod mapper;
pub use mapper::{
    SectorDataProvider, analyze_and_map_disc, rle_encode_map
};

mod reconstruction;
pub use reconstruction::{
    ReconstructionError,
    expand_exception_index, expand_sector_map, expand_subheader_map,
    rebuild_sector_bitwise, rebuild_sector_simd, verify_disc_integrity
};

mod stream;
pub use stream::{
    MultiBinStream, SectorRegionStream, UserDataStream
};

mod util;
