use thiserror::Error;

use crate::{
    analyze::AnalysisError,
    cue_parser::ParseError,
    ecc::EccError,
    mapper::MapperError,
    reconstruction::ReconstructionError,
};

#[derive(Error, Debug)]
pub enum SpriteShrinkCDError {
    #[error("An error occurred during CD bin analysis: {0}")]
    Analysis(#[from] AnalysisError),

    #[error("An ECC error occurred.")]
    Ecc(#[from] EccError),

    #[error("An external error occurred: {0}")]
    External(Box<dyn std::error::Error + Send + Sync>),

    #[error("An I/O error occurred")]
    Io(#[from] std::io::Error),

    #[error("A parse error occurred.")]
    Parse(#[from] ParseError),

    #[error("A mapper error occurred.")]
    Mapper(#[from] MapperError),

    #[error("Reconstruction error: {0}")]
    Reconstruction(#[from] ReconstructionError),
}
