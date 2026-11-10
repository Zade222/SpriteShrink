use sprite_shrink::{IsCancelled, SpriteShrinkError};
use thiserror::Error;

use crate::{
    analyze::AnalysisError,
    cue_parser::ParseError,
    ecc::EccError,
    flac::FlacError,
    mapper::MapperError,
    decode::DecodeError,
    reconstruction::ReconstructionError,
    util::UtilError,
};

#[derive(Error, Debug)]
pub enum SpriteShrinkCDError {
    #[error("An error occurred during CD bin analysis: {0}")]
    Analysis(#[from] AnalysisError),

    #[error("An decode error occurred: {0}")]
    Decode(#[from] DecodeError),

    #[error("An ECC error occurred: {0}")]
    Ecc(#[from] EccError),

    #[error("A flac error occurred. {0}")]
    Flac(#[from] FlacError),

    #[error("An external error occurred: {0}")]
    External(Box<dyn std::error::Error + Send + Sync>),

    #[error("An I/O error occurred: {0}")]
    Io(#[from] std::io::Error),

    #[error("A parse error occurred: {0}")]
    Parse(#[from] ParseError),

    #[error("A mapper error occurred: {0}")]
    Mapper(#[from] MapperError),

    #[error("Reconstruction error: {0}")]
    Reconstruction(#[from] ReconstructionError),

    #[error("A sprite_shrink error occurred: {0}")]
    SpriteShrink(#[from] SpriteShrinkError),

    #[error("Library utility error: {0}")]
    Util(#[from] UtilError),
}

/// A cancellation from `sprite_shrink`, such as a read or write callback
/// reporting one while FLAC blocks are compressed, counts as cancelled.
/// No other variant carries a cancellation.
impl IsCancelled for SpriteShrinkCDError {
    fn is_cancelled(&self) -> bool {
        matches!(self, Self::SpriteShrink(err) if err.is_cancelled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_library_cancellation_is_cancelled() {
        let err = SpriteShrinkCDError::from(SpriteShrinkError::Cancelled);
        assert!(err.is_cancelled());
    }

    #[test]
    fn other_errors_are_not_cancelled() {
        let library = SpriteShrinkCDError::from(SpriteShrinkError::External(
            "read failed".into(),
        ));
        let io = SpriteShrinkCDError::from(std::io::Error::other("disk"));
        assert!(!library.is_cancelled());
        assert!(!io.is_cancelled());
    }
}
