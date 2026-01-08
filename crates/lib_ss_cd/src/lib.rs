mod analyze;
pub use analyze::{
    FileSizeProvider, analyze_cue_sheets, resolve_file_sector_counts
};

mod cue_parser;


mod ecc;

mod lib_error_handling;
pub use lib_error_handling::SpriteShrinkCDError;

mod lib_structs;
pub use lib_structs::{
    CueSheet, ContentBlock, DataChunkLayout, DiscManifest, ExceptionInfo,
    ExceptionType, RleSectorMap, SectorMap, SectorType,
};

mod mapper;
pub use mapper::{
    SectorDataProvider, analyze_and_map_disc, rle_encode_map
};

mod reconstruction;
pub use reconstruction::{
    verify_disc_integrity
};

mod stream;
pub use stream::{
    MultiBinStream, SectorRegionStream, UserDataStream
};
