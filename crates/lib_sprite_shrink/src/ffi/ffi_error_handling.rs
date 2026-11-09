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
    IndexDecodeError = -24,
    TocDecodeError = -25,
    InvalidFormatData = -26,
    IoError = -27,
    ArchiveEncodeError = -28,
    ArchiveTooLarge = -29,
    CallbackError = -30,
}


pub type FFIResult = FFIStatus;


// These matches list every variant instead of ending in `_`, so adding an
// error doesn't compile until it has been given a status.
impl From<SpriteShrinkError> for FFIStatus {
    fn from(err: SpriteShrinkError) -> Self {
        match err {
            SpriteShrinkError::Archive(e) => e.into(),
            SpriteShrinkError::Parsing(e) => e.into(),
            SpriteShrinkError::Processing(e) => e.into(),
            SpriteShrinkError::Serialization(e) => e.into(),
            SpriteShrinkError::Io(_) => FFIStatus::IoError,
            SpriteShrinkError::External(_) => FFIStatus::CallbackError,
            SpriteShrinkError::Cancelled => FFIStatus::StatusCancelled,
        }
    }
}

impl From<ArchiveError> for FFIStatus {
    fn from(err: ArchiveError) -> Self {
        match err {
            ArchiveError::Io(_) => FFIStatus::IoError,
            ArchiveError::CompressionError(_) | ArchiveError::ZstdError(_) => {
                FFIStatus::CompressionError
            }
            ArchiveError::Dictionary(_) => FFIStatus::DictionaryError,
            ArchiveError::ThreadPool(_) | ArchiveError::ThreadPoolError(_) => {
                FFIStatus::ThreadPoolError
            }
            ArchiveError::ManifestEncodeError(_)
            | ArchiveError::IndexEncodeError(_) => {
                FFIStatus::ArchiveEncodeError
            }
            ArchiveError::WorkerError(_) => FFIStatus::InternalError,
            ArchiveError::ProcessingError(e) => e.into(),
            ArchiveError::External(_) => FFIStatus::CallbackError,
            ArchiveError::Cancelled => FFIStatus::StatusCancelled,
        }
    }
}

impl From<ParsingError> for FFIStatus {
    fn from(err: ParsingError) -> Self {
        match err {
            ParsingError::IndexDecodeError(_) => FFIStatus::IndexDecodeError,
            ParsingError::InvalidFileVersion() => {
                FFIStatus::UnsupportedVersion
            }
            ParsingError::InvalidHeader(_) => FFIStatus::InvalidHeader,
            ParsingError::InvalidFormatData(_) => FFIStatus::InvalidFormatData,
            ParsingError::ManifestDecodeError(_) => {
                FFIStatus::ManifestDecodeError
            }
            ParsingError::SectionOutOfBounds(_) => {
                FFIStatus::SectionOutOfBounds
            }
            ParsingError::TOCDecodeError(_) => FFIStatus::TocDecodeError,
            ParsingError::UnsupportedHashType(_) => {
                FFIStatus::UnsupportedHashType
            }
        }
    }
}

impl From<ProcessingError> for FFIStatus {
    fn from(err: ProcessingError) -> Self {
        match err {
            ProcessingError::SampleGeneration(_)
            | ProcessingError::DictionaryError(_) => {
                FFIStatus::DictionaryError
            }
            ProcessingError::HashMismatchError(_) => {
                FFIStatus::VerificationHashMismatch
            }
            ProcessingError::InternalError(_)
            | ProcessingError::ThreadPanic(_) => FFIStatus::InternalError,
            ProcessingError::SeekOutOfBounds(_) => FFIStatus::SeekOutOfBounds,
            ProcessingError::InvalidWindowSize(_) => {
                FFIStatus::InvalidWindowSize
            }
            ProcessingError::VerificationError(_) => {
                FFIStatus::VerificationMissingChunk
            }
            ProcessingError::External(_) => FFIStatus::CallbackError,
            ProcessingError::Cancelled => FFIStatus::StatusCancelled,
        }
    }
}

impl From<SerializationError> for FFIStatus {
    fn from(err: SerializationError) -> Self {
        match err {
            SerializationError::ArchiveTooLarge(_) => {
                FFIStatus::ArchiveTooLarge
            }
            SerializationError::MissingChunk(_) => {
                FFIStatus::SerializationMissingChunk
            }
            SerializationError::External(_) => FFIStatus::CallbackError,
            SerializationError::Cancelled => FFIStatus::StatusCancelled,
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
            (ParsingError::IndexDecodeError(String::new()).into(), -24),
            (ParsingError::TOCDecodeError(String::new()).into(), -25),
            (ParsingError::InvalidFormatData(String::new()).into(), -26),
            (ArchiveError::Dictionary(String::new()).into(), -6),
            (ArchiveError::CompressionError(String::new()).into(), -7),
            (ArchiveError::ZstdError(String::new()).into(), -7),
            (ArchiveError::ThreadPool(String::new()).into(), -8),
            (ArchiveError::ThreadPoolError(String::new()).into(), -8),
            (ArchiveError::Io(io::Error::other("disk full")).into(), -27),
            (ArchiveError::ManifestEncodeError(String::new()).into(), -28),
            (ArchiveError::IndexEncodeError(String::new()).into(), -28),
            (ArchiveError::External(String::new()).into(), -30),
            (ProcessingError::SampleGeneration(String::new()).into(), -6),
            (ProcessingError::DictionaryError(String::new()).into(), -6),
            (ProcessingError::HashMismatchError(String::new()).into(), -9),
            (ProcessingError::VerificationError(String::new()).into(), -10),
            (ProcessingError::SeekOutOfBounds(String::new()).into(), -14),
            (ProcessingError::InvalidWindowSize(0).into(), -22),
            (ProcessingError::External(String::new()).into(), -30),
            (SerializationError::MissingChunk(String::new()).into(), -12),
            (SerializationError::ArchiveTooLarge(String::new()).into(), -29),
            (SerializationError::External(String::new()).into(), -30),
            (io::Error::other("disk full").into(), -27),
            (SpriteShrinkError::External("host failed".into()), -30),
            (SpriteShrinkError::Cancelled, -23),
        ];

        for (error, expected) in cases {
            let message = error.to_string();
            assert_eq!(code(error), expected, "{message}");
        }
    }

    #[test]
    fn only_faults_in_the_library_are_internal_errors() {
        let cases: [SpriteShrinkError; 3] = [
            ArchiveError::WorkerError(String::new()).into(),
            ProcessingError::InternalError(String::new()).into(),
            ProcessingError::ThreadPanic(String::new()).into(),
        ];

        for error in cases {
            let message = error.to_string();
            assert_eq!(code(error), -5, "{message}");
        }
    }

    #[test]
    fn a_processing_error_inside_an_archive_error_keeps_its_status() {
        let wrapped = |e| SpriteShrinkError::from(ArchiveError::ProcessingError(e));

        assert_eq!(
            code(wrapped(ProcessingError::HashMismatchError(String::new()))),
            -9
        );
        assert_eq!(code(wrapped(ProcessingError::Cancelled)), -23);
    }

    #[test]
    fn a_cancellation_from_any_module_reaches_c_as_cancelled() {
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

        let cancelled: Result<(), SpriteShrinkError> =
            FFICallbackStatus::Cancelled.into();
        assert_eq!(code(cancelled.unwrap_err()), -23);

        let failed: Result<(), SpriteShrinkError> = FFICallbackStatus::Error.into();
        let error = failed.unwrap_err();
        assert!(!error.is_cancelled());
        assert_eq!(code(error), -30);
    }
}
