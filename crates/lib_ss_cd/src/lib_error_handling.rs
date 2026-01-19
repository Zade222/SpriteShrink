use thiserror::Error;

use crate::{
    analyze::AnalysisError,
    cue_parser::ParseError,
    ecc::EccError,
    flac::FlacError,
    mapper::MapperError,
    decode::DecodeError,
    reconstruction::ReconstructionError,
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
}
