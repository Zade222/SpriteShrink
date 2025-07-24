pub mod archive_ffi;
pub use archive_ffi::*;

pub mod ffi_structs;
pub use ffi_structs::*;

pub mod parsing_ffi;
pub use parsing_ffi::*;

pub mod processing_ffi;
pub use processing_ffi::*;

pub mod serialization_ffi;
pub use serialization_ffi::*;

/// Represents the status of an FFI operation.
#[repr(C)]
pub enum FFIStatus {
    Ok = 0,
    NullArgument = -1,
    InvalidHeader = -2,
    UnsupportedVersion = -3,
    ManifestDecodeError = -4,
    InternalError = -5,
}