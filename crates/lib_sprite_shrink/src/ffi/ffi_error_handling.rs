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
    NoDataToProcess = -16,
    BufferTooSmall = -17,
    HashNotFound = -18,
    DuplicateFilename = -19,
    SectionOutOfBounds = -20,
    UnsupportedHashType = -21,
    InvalidWindowSize = -22,
    StatusCancelled = -23,
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
            SpriteShrinkError::Parsing(
                ParsingError::SectionOutOfBounds(_)
            ) => FFIStatus::SectionOutOfBounds,
            SpriteShrinkError::Parsing(
                ParsingError::UnsupportedHashType(_)
            ) => FFIStatus::UnsupportedHashType,

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
            SpriteShrinkError::Processing(
                ProcessingError::InvalidWindowSize(_)
            ) => FFIStatus::InvalidWindowSize,

            //Serialization errors
            SpriteShrinkError::Serialization(
                SerializationError::MissingChunk(_)
            ) => FFIStatus::SerializationMissingChunk,

            //A host callback asked to stop.
            SpriteShrinkError::Cancelled => FFIStatus::StatusCancelled,

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

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::lib_error_handling::IsCancelled;

    /// The C status code `error` maps to.
    fn code(error: SpriteShrinkError) -> i32 {
        FFIStatus::from(error) as i32
    }

    /// C hosts compare against these numbers, so each one is written out
    /// rather than read from the enum.
    #[test]
    fn each_error_maps_to_its_documented_status_code() {
        let cases: Vec<(SpriteShrinkError, i32)> = vec![
            (ParsingError::InvalidHeader(String::new()).into(), -2),
            (ParsingError::InvalidFileVersion().into(), -3),
            (ParsingError::ManifestDecodeError(String::new()).into(), -4),
            (ParsingError::SectionOutOfBounds(String::new()).into(), -20),
            (ParsingError::UnsupportedHashType(0).into(), -21),
            (ArchiveError::Dictionary(String::new()).into(), -6),
            (ArchiveError::CompressionError(String::new()).into(), -7),
            (ArchiveError::ThreadPoolError(String::new()).into(), -8),
            (ProcessingError::HashMismatchError(String::new()).into(), -9),
            (ProcessingError::VerificationError(String::new()).into(), -10),
            (ProcessingError::SeekOutOfBounds(String::new()).into(), -14),
            (ProcessingError::InvalidWindowSize(0).into(), -22),
            (SerializationError::MissingChunk(String::new()).into(), -12),
            (SpriteShrinkError::Cancelled, -23),
        ];

        for (error, expected) in cases {
            let message = error.to_string();
            assert_eq!(code(error), expected, "{message}");
        }
    }

    #[test]
    fn errors_without_their_own_status_are_internal_errors() {
        let cases: [SpriteShrinkError; 5] = [
            io::Error::other("disk full").into(),
            SpriteShrinkError::External("host failed".into()),
            ArchiveError::WorkerError(String::new()).into(),
            ParsingError::IndexDecodeError(String::new()).into(),
            ProcessingError::InternalError(String::new()).into(),
        ];

        for error in cases {
            let message = error.to_string();
            assert_eq!(code(error), -5, "{message}");
        }
    }

    #[test]
    fn a_cancellation_from_any_module_reaches_c_as_cancelled() {
        // It used to fall through to InternalError, so a C host couldn't
        // tell a cancellation from a failure.
        let cancelled = [
            SpriteShrinkError::from(ArchiveError::Cancelled),
            SpriteShrinkError::from(ProcessingError::Cancelled),
            SpriteShrinkError::from(SerializationError::Cancelled),
        ];

        for error in cancelled {
            assert_eq!(code(error), -23);
        }
    }

    #[test]
    fn callback_statuses_become_the_matching_errors() {
        let ok: Result<(), SpriteShrinkError> = FFICallbackStatus::CallbackOk.into();
        ok.unwrap();

        // A host that cancels through a callback gets StatusCancelled back.
        let cancelled: Result<(), SpriteShrinkError> =
            FFICallbackStatus::Cancelled.into();
        assert_eq!(code(cancelled.unwrap_err()), -23);

        let failed: Result<(), SpriteShrinkError> = FFICallbackStatus::Error.into();
        let error = failed.unwrap_err();
        assert!(!error.is_cancelled());
        assert_eq!(code(error), -5);
    }
}
