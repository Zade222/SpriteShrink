use thiserror::Error;

use crate::{
    analyze::AnalysisError,
};

#[derive(Error, Debug)]
pub enum SpriteShrinkCDError {
    #[error("An error occurred during CD bin analysis.")]
    Analysis(#[from] AnalysisError),

    #[error("An I/O error occurred")]
    Io(#[from] std::io::Error),
}
