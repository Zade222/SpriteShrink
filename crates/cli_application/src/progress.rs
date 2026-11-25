//! Progress bars and the summary printed after compression.
//!
//! Bars are drawn with `--progress`, unless `--quiet` is set. When they
//! aren't drawn, the functions here return hidden bars, which take the same
//! calls and draw nothing, so callers don't check the flags themselves.

use std::{
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use sprite_shrink::Progress;
use tracing::debug;

use crate::arg_handling::Args;

/// A bar counting bytes, with the elapsed time and an estimate of the time
/// left.
const BYTES_TEMPLATE: &str =
    "[{elapsed_precise}] {bytes}/{total_bytes} ({eta}) {msg}\n[{bar:40}]";

/// A bar counting items, such as chunks or audio blocks.
const COUNT_TEMPLATE: &str =
    "[{elapsed_precise}] {pos}/{len} (eta {eta}) {msg}\n[{bar:40}]";

/// Whether `args` asks for progress bars.
pub fn shows_progress(args: &Args) -> bool {
    args.progress && !args.quiet
}

/// A bar of `total` units drawn with `template`, or a hidden one if `show`
/// is false.
fn bar(show: bool, total: u64, template: &str, message: &'static str) -> ProgressBar {
    if !show {
        return ProgressBar::hidden();
    }
    /*The templates are constants that parse. Should one ever fail, the
    plain default bar is drawn instead.*/
    let style = ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("#>-");
    let bar = ProgressBar::new(total).with_style(style);
    bar.set_message(message);
    bar
}

/// A bar of `total` bytes, or a hidden one if `show` is false.
pub fn byte_bar(show: bool, total: u64, message: &'static str) -> ProgressBar {
    bar(show, total, BYTES_TEMPLATE, message)
}

/// A bar of `total` items, or a hidden one if `show` is false.
pub fn count_bar(show: bool, total: u64, message: &'static str) -> ProgressBar {
    bar(show, total, COUNT_TEMPLATE, message)
}

/// One bar per read of the discs in optical analysis, drawn together. The
/// discs are analysed in parallel and each goes through the three reads on
/// its own, so at any time each read may have discs in it.
#[derive(Clone, Debug)]
pub struct AnalysisBars {
    /// Reading each disc to hash it.
    pub hashing: ProgressBar,
    /// Reading each sector to find its type.
    pub classifying: ProgressBar,
    /// Reading the sectors again to cut them into chunks.
    pub chunking: ProgressBar,
}

impl AnalysisBars {
    /// Three bars of `total` bytes each, or hidden ones if `show` is false.
    pub fn new(show: bool, total: u64) -> Self {
        let bars = Self {
            hashing: byte_bar(show, total, "Hashing discs..."),
            classifying: byte_bar(show, total, "Classifying sectors..."),
            chunking: byte_bar(show, total, "Chunking discs..."),
        };
        if show {
            /*The bars draw through the `MultiProgress` from here on; it
            can be dropped, as each bar keeps what it needs.*/
            let multi = MultiProgress::new();
            for bar in [&bars.hashing, &bars.classifying, &bars.chunking] {
                multi.add(bar.clone());
            }
        }
        bars
    }

    /// Finishes the three bars once every disc is analysed.
    pub fn finish(&self) {
        self.hashing.finish_with_message("Discs hashed.");
        self.classifying.finish_with_message("Sectors classified.");
        self.chunking.finish_with_message("Disc chunking complete.");
    }
}

/// Shows `ArchiveBuilder`'s progress: a spinner while the dictionary is
/// trained, then a bar of chunks compressed.
#[derive(Debug)]
pub struct CompressionProgress {
    show: bool,
    /// The spinner or bar being drawn. The builder reports from its own
    /// threads, so the callback and `finish` share it.
    current: Arc<Mutex<Option<ProgressBar>>>,
}

impl CompressionProgress {
    pub fn new(args: &Args) -> Self {
        Self { show: shows_progress(args), current: Arc::default() }
    }

    /// The callback to pass to `ArchiveBuilder::with_progress`.
    pub fn callback(&self) -> impl Fn(Progress) + Send + Sync + 'static {
        let show = self.show;
        let current = Arc::clone(&self.current);
        move |progress| {
            match progress {
                Progress::GeneratingDictionary => {
                    debug!("Generating compression dictionary...");
                }
                Progress::DictionaryDone => debug!("Dictionary created."),
                Progress::Finalizing => debug!("Finalizing archive..."),
                Progress::Compressing { .. } | Progress::ChunkCompressed => {}
            }
            if !show {
                return;
            }

            /*A panic elsewhere while the lock was held leaves at worst a
            half-drawn bar, so a poisoned lock is still used.*/
            let mut current = current.lock().unwrap_or_else(PoisonError::into_inner);
            match progress {
                Progress::GeneratingDictionary => {
                    *current = Some(dictionary_spinner());
                }
                Progress::DictionaryDone => {
                    if let Some(spinner) = current.take() {
                        spinner.finish_with_message("Dictionary created.");
                    }
                }
                Progress::Compressing { total_chunks } => {
                    *current = Some(count_bar(
                        true,
                        total_chunks,
                        "Compressing chunks...",
                    ));
                }
                Progress::ChunkCompressed => {
                    if let Some(bar) = current.as_ref() {
                        bar.inc(1);
                    }
                }
                Progress::Finalizing => {}
            }
        }
    }

    /// Finishes the chunk bar once the builder returns.
    pub fn finish(&self) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(bar) = current.take() {
            bar.finish_with_message("Compression complete.");
        }
    }
}

/// A spinner shown while the compression dictionary is trained, which
/// reports no progress of its own.
fn dictionary_spinner() -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(Duration::from_millis(500));
    spinner.set_style(
        ProgressStyle::with_template("{msg} {spinner}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_strings(&["   ", ".  ", ".. ", "...", " ..", "  .", "   "]),
    );
    spinner.set_message("Generating compression dictionary");
    spinner
}

/// The line printed after compression: the archive, the input and archive
/// sizes, and the archive's size as a percentage of the input, to one
/// decimal place.
pub fn compression_summary(archive: &Path, input: u64, output: u64) -> String {
    let percentage = if input > 0 {
        /*Tenths of a percent, rounded, in integers: u128 can't overflow
        from u64 sizes, and floats would need casts that lose precision.*/
        let tenths = (u128::from(output) * 1000 + u128::from(input) / 2)
            / u128::from(input);
        format!(" ({}.{}% of the input)", tenths / 10, tenths % 10)
    } else {
        String::new()
    };
    format!(
        "Wrote {}: {} from {}{percentage}",
        archive.display(),
        HumanBytes(output),
        HumanBytes(input)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_gives_the_archive_as_a_percentage_of_the_input() {
        let archive = Path::new("out/ff7.ssmd");
        assert_eq!(
            compression_summary(archive, 2_000, 855),
            "Wrote out/ff7.ssmd: 855 B from 1.95 KiB (42.8% of the input)"
        );
        // An archive larger than its input, and rounding up to the next
        // tenth.
        assert_eq!(
            compression_summary(archive, 3, 4),
            "Wrote out/ff7.ssmd: 4 B from 3 B (133.3% of the input)"
        );
        assert_eq!(
            compression_summary(archive, 3, 2),
            "Wrote out/ff7.ssmd: 2 B from 3 B (66.7% of the input)"
        );
        // Nothing to divide by.
        assert_eq!(
            compression_summary(archive, 0, 120),
            "Wrote out/ff7.ssmd: 120 B from 0 B"
        );
    }
}
