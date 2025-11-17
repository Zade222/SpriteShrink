use crate::lib_error_handling::SpriteShrinkError;
use crate::archive::ArchiveError;
use crate::parsing::ParsingError;
use crate::processing::ProcessingError;
use crate::serialization::SerializationError;

/// Represents the status of an FFI operation.
#[repr(C)]
#[must_use]
pub enum FFIStatus {
    StatusOk = 0,
    NullArgument = -1,
    InvalidHeader = -2,
    UnsupportedVersion = -3,
    ManifestDecodeError = -4,
    InternalError = -5,
    DictionaryError = -6,
    CompressionError = -7,
    ThreadPoolError = -8,
    VerificationHashMismatch = -9,
    VerificationMissingChunk = -10,
    InvalidString = -11,
    SerializationMissingChunk = -12,
    InvalidMagicNumber = -13,
    SeekOutOfBounds = -14,
    IncorrectArrayLength = -15,
    NoDataToProcess = -16
}


pub type FFIResult = FFIStatus;


impl From<SpriteShrinkError> for FFIStatus {
    fn from(err: SpriteShrinkError) -> Self {
        match err {
            //Parsing errors
            SpriteShrinkError::Parsing(
                ParsingError::InvalidHeader(_)
            ) => FFIStatus::InvalidHeader,
            SpriteShrinkError::Parsing(
                ParsingError::InvalidFileVersion()
            ) => FFIStatus::UnsupportedVersion,
            SpriteShrinkError::Parsing(
                ParsingError::ManifestDecodeError(_)
            ) => FFIStatus::ManifestDecodeError,

            //Archive errors
            SpriteShrinkError::Archive(
                ArchiveError::Dictionary(_)
            ) => FFIStatus::DictionaryError,
            SpriteShrinkError::Archive(
                ArchiveError::CompressionError(_)
            ) => FFIStatus::CompressionError,
            SpriteShrinkError::Archive(
                ArchiveError::ThreadPoolError(_)
            ) => FFIStatus::ThreadPoolError,

            //Processing errors
            SpriteShrinkError::Processing(
                ProcessingError::HashMismatchError(_)
            ) => FFIStatus::VerificationHashMismatch,
            SpriteShrinkError::Processing(
                ProcessingError::SeekOutOfBounds(_)
            ) => FFIStatus::SeekOutOfBounds,
            SpriteShrinkError::Processing(
                ProcessingError::VerificationError(_)
            ) => FFIStatus::VerificationMissingChunk,

            //Serialization errors
            SpriteShrinkError::Serialization(
                SerializationError::MissingChunk(_)
            ) => FFIStatus::SerializationMissingChunk,

            //Catch all for any other error.
            _ => FFIStatus::InternalError,
        }
    }
}


#[repr(C)]
pub enum FFICallbackStatus {
    CallbackOk = 0,
    Error = -1,
    Cancelled = -2,
}

impl From<FFICallbackStatus> for Result<(), SpriteShrinkError> {
    fn from(status: FFICallbackStatus) -> Self {
        match status {
            FFICallbackStatus::CallbackOk => Ok(()),
            FFICallbackStatus::Cancelled => Err(SpriteShrinkError::Cancelled),
            FFICallbackStatus::Error => Err(ArchiveError::External(
                "An error occurred in a host callback.".to_string(),
            )
            .into()),
        }
    }
}
