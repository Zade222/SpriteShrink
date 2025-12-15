use thiserror::Error;

use crate::{
    analyze::AnalysisError, cue_parser::ParseError,
};

#[derive(Error, Debug)]
pub enum SpriteShrinkCDError {
    #[error("An error occurred during CD bin analysis.")]
    Analysis(#[from] AnalysisError),

    #[error("An I/O error occurred")]
    Io(#[from] std::io::Error),

    #[error("A parse error occurred.")]
    Parse(#[from] ParseError),
}
