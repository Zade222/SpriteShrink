use std::io::{Cursor, Read, Write};

use flac_codec::{
    byteorder::LittleEndian,
    decode::FlacByteReader,
    encode::{FlacByteWriter, Options},
};
use thiserror::Error;

use crate::{
    lib_error_handling::SpriteShrinkCDError,
};

use sprite_shrink::{ChunkLocation, IsCancelled, compress_chunks_with};


#[derive(Error, Debug)]
pub enum FlacError {
    #[error("Flac write buffer error.")]
    Buffer,

    #[error("Flac decode error: {0}")]
    Decode(String),

    #[error("Flac finalization error: {0}")]
    Finalization(String),

    #[error("Flac init error: {0}")]
    Init(String),
}


pub fn compress_audio_block(
    audio_block: &[u8],
    best_comp: bool
) -> Result<Vec<u8>, FlacError> {
    let mut flac_dest = Cursor::new(Vec::with_capacity(
        (audio_block.len() * 3) / 5
    ));

    let options = if best_comp {
        Options::best()
    } else {
        Options::fast()
    };

    let mut writer = FlacByteWriter::<_, LittleEndian>::new_cdda(
        &mut flac_dest,
        options,
        Some(audio_block.len() as u64)
    ).map_err(|e| FlacError::Init(e.to_string()))?;

    writer.write_all(audio_block).map_err(|_| FlacError::Buffer)?;
    writer.finalize().map_err(|e| FlacError::Finalization(e.to_string()))?;

    Ok(flac_dest.into_inner())
}


pub fn decompress_flac_block(
    flac_block: &[u8]
) -> Result<Vec<u8>, FlacError> {
    let mut reader = FlacByteReader::endian(
        flac_block,
        LittleEndian
    ).map_err(|e| FlacError::Init(e.to_string()))?;

    let mut audio_bytes = Vec::new();

    reader.read_to_end(&mut audio_bytes)
        .map_err(|e| FlacError::Decode(e.to_string()))?;

    Ok(audio_bytes)
}


/// Compresses CD audio blocks with FLAC, in parallel, and writes them in
/// order.
///
/// Uses the same pipeline as the data chunks of an archive
/// (`sprite_shrink::compress_chunks_with`), with each block compressed by
/// `compress_audio_block` at its best setting.
///
/// # Arguments
///
/// * `audio_block_hashes`: The blocks to compress, in the order to write
///   them.
/// * `get_audio_blocks_cb`: Reads the PCM data for a set of hashes, in the
///   same order.
/// * `com_block_write_cb`: Writes each compressed block, in order.
/// * `worker_count`: The number of worker threads. `0` uses the available
///   parallelism.
///
/// # Returns
///
/// Each hash of `audio_block_hashes`, in order, with the offset and length
/// of its compressed block in the written output.
///
/// # Errors
///
/// `SpriteShrinkCDError::SpriteShrink` wrapping:
/// - `SpriteShrinkError::Cancelled` if either callback reports a
///   cancellation.
/// - `SpriteShrinkError::Archive` for any other callback error, a read
///   callback that returns a different number of blocks than it was asked
///   for, or a block FLAC can't compress (the `FlacError` message is kept).
pub fn compress_audio_blocks<E, H, R, W>(
    audio_block_hashes: &[H],
    get_audio_blocks_cb: R,
    com_block_write_cb: W,
    worker_count: usize,
) -> Result<Vec<(H, ChunkLocation)>, SpriteShrinkCDError>
where
    E: std::error::Error + IsCancelled,
    H: Copy,
    R: Fn(&[H]) -> Result<Vec<Vec<u8>>, E>,
    W: FnMut(&[u8]) -> Result<(), E>,
{
    /*FLAC keeps no state between blocks, so every worker's compressor is
    the same stateless function.*/
    let new_compressor = || Ok::<_, FlacError>(
        |block: &[u8]| compress_audio_block(block, true)
    );

    Ok(compress_chunks_with(
        worker_count,
        audio_block_hashes,
        new_compressor,
        get_audio_blocks_cb,
        com_block_write_cb,
    )?)
}


#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        f64::consts::TAU,
        sync::mpsc,
        time::Duration,
    };

    use sprite_shrink::SpriteShrinkError;

    use super::*;

    /// Bytes in one CD sector of audio: 588 stereo 16-bit frames.
    const SECTOR: usize = 2352;

    /// `frames` stereo frames of 16-bit little-endian PCM, each sample
    /// given by `sample(frame, channel)`.
    fn pcm(frames: usize, sample: impl Fn(usize, usize) -> i16) -> Vec<u8> {
        (0..frames)
            .flat_map(|frame| [sample(frame, 0), sample(frame, 1)])
            .flat_map(i16::to_le_bytes)
            .collect()
    }

    fn silence(frames: usize) -> Vec<u8> {
        pcm(frames, |_, _| 0)
    }

    /// A 441 Hz tone at 44.1 kHz, a quarter of full scale, with the right
    /// channel half a cycle behind the left.
    fn sine(frames: usize) -> Vec<u8> {
        pcm(frames, |frame, channel| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "frame and channel are small"
            )]
            let phase = (frame as f64 / 100.0 + channel as f64 / 2.0) * TAU;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the value is within ±8192"
            )]
            let value = (phase.sin() * 8192.0).round() as i16;
            value
        })
    }

    /// Full-scale white noise from a fixed-seed xorshift, which FLAC can't
    /// shrink.
    fn noise(frames: usize) -> Vec<u8> {
        let mut state = 0x5EED_F1AC_u64;
        (0..frames * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    #[test]
    fn audio_round_trips_through_flac() {
        let frames = 10 * SECTOR / 4;
        for (name, audio) in [
            ("silence", silence(frames)),
            ("sine", sine(frames)),
            ("noise", noise(frames)),
        ] {
            for best in [true, false] {
                let flac = compress_audio_block(&audio, best).unwrap();
                assert_eq!(
                    decompress_flac_block(&flac).unwrap(),
                    audio,
                    "{name}, best {best}"
                );
            }
        }
    }

    #[test]
    fn silence_and_a_tone_compress() {
        let frames = 10 * SECTOR / 4;
        for (name, audio) in [("silence", silence(frames)), ("sine", sine(frames))] {
            let flac = compress_audio_block(&audio, true).unwrap();
            assert!(
                flac.len() < audio.len() / 2,
                "{name}: {} bytes from {}",
                flac.len(),
                audio.len()
            );
        }
    }

    #[test]
    fn an_empty_block_is_an_error() {
        // flac_codec rejects a total length of 0. Audio blocks are whole
        // sectors, so the CLI never asks for one.
        assert!(matches!(
            compress_audio_block(&[], true),
            Err(FlacError::Init(_))
        ));
    }

    #[test]
    fn a_block_that_is_not_whole_frames_is_an_error() {
        let audio = sine(SECTOR / 4);
        for cut in 1..4 {
            let partial = &audio[..audio.len() - cut];
            assert!(
                compress_audio_block(partial, true).is_err(),
                "{} bytes",
                partial.len()
            );
        }
    }

    #[test]
    fn garbage_does_not_decode() {
        decompress_flac_block(&[]).unwrap_err();
        decompress_flac_block(&noise(100)).unwrap_err();
    }

    type ReadResult = Result<Vec<Vec<u8>>, SpriteShrinkError>;
    type BlockIndex = Vec<(u64, ChunkLocation)>;

    /// `count` blocks of one to four sectors of a tone, each with a
    /// different length, stored under the hashes `0..count`.
    fn block_store(count: u64) -> (Vec<u64>, HashMap<u64, Vec<u8>>) {
        let blocks = (0..count)
            .map(|i| {
                let sectors = 1 + usize::try_from(i % 4).unwrap();
                (i, sine(sectors * SECTOR / 4))
            })
            .collect();
        ((0..count).collect(), blocks)
    }

    /// Runs `compress_audio_blocks` on another thread, failing the test if
    /// it takes longer than 30 seconds: the old pipeline hung forever when
    /// the read callback returned too few blocks. Returns the result and
    /// each block passed to the write callback.
    fn compress_blocks(
        workers: usize,
        hashes: Vec<u64>,
        read: impl Fn(&[u64]) -> ReadResult + Send + 'static,
    ) -> (Result<BlockIndex, SpriteShrinkCDError>, Vec<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut written = Vec::new();
            let result = compress_audio_blocks(
                &hashes,
                read,
                |block: &[u8]| -> Result<(), SpriteShrinkError> {
                    written.push(block.to_vec());
                    Ok(())
                },
                workers,
            );
            /*The send fails only if the receiver timed out and was
            dropped, and that already failed the test.*/
            let _ignored = tx.send((result, written));
        });
        rx.recv_timeout(Duration::from_secs(30))
            .expect("compress_audio_blocks didn't return within 30 seconds")
    }

    /// A read callback that returns each requested block from `blocks`.
    fn read_from(
        blocks: HashMap<u64, Vec<u8>>,
    ) -> impl Fn(&[u64]) -> ReadResult + Send + 'static {
        move |hashes| {
            Ok(hashes.iter().map(|hash| blocks[hash].clone()).collect())
        }
    }

    #[test]
    fn blocks_are_written_in_order_with_contiguous_offsets() {
        // More than one 500-block batch, so the queue is refilled.
        let (hashes, blocks) = block_store(700);
        let (result, written) =
            compress_blocks(4, hashes.clone(), read_from(blocks.clone()));
        let index = result.unwrap();

        let indexed: Vec<u64> = index.iter().map(|(hash, _)| *hash).collect();
        assert_eq!(indexed, hashes);
        assert_eq!(written.len(), hashes.len());

        let mut offset = 0;
        for ((hash, location), flac) in index.iter().zip(&written) {
            assert_eq!(location.offset, offset, "block {hash}");
            assert_eq!(
                location.compressed_length as usize,
                flac.len(),
                "block {hash}"
            );
            offset += u64::from(location.compressed_length);
            assert_eq!(
                decompress_flac_block(flac).unwrap(),
                blocks[hash],
                "block {hash}"
            );
        }
    }

    #[test]
    fn a_read_returning_too_few_or_too_many_blocks_is_an_error() {
        let (hashes, blocks) = block_store(20);
        for change in [-1_isize, 1] {
            let blocks = blocks.clone();
            let read = move |hashes: &[u64]| -> ReadResult {
                let mut read: Vec<Vec<u8>> =
                    hashes.iter().map(|hash| blocks[hash].clone()).collect();
                if change < 0 {
                    read.pop();
                } else {
                    read.push(silence(SECTOR / 4));
                }
                Ok(read)
            };

            let (result, written) = compress_blocks(2, hashes.clone(), read);
            assert!(
                matches!(
                    &result,
                    Err(SpriteShrinkCDError::SpriteShrink(
                        SpriteShrinkError::Archive(_)
                    ))
                ),
                "change {change}: {result:?}"
            );
            assert_eq!(written.len(), 0, "change {change}");
        }
    }

    #[test]
    fn a_cancelled_read_is_reported_as_cancelled() {
        let (result, _written) = compress_blocks(
            2,
            vec![1, 2, 3],
            |_: &[u64]| -> ReadResult { Err(SpriteShrinkError::Cancelled) },
        );
        assert!(
            matches!(
                result,
                Err(SpriteShrinkCDError::SpriteShrink(
                    SpriteShrinkError::Cancelled
                ))
            ),
            "{result:?}"
        );
    }

    #[test]
    fn a_block_flac_rejects_fails_with_its_message() {
        // A block of 3 bytes isn't a whole stereo frame.
        let (result, _written) = compress_blocks(
            2,
            vec![1, 2, 3],
            |hashes: &[u64]| -> ReadResult {
                Ok(hashes.iter().map(|_| vec![0; 3]).collect())
            },
        );
        let Err(SpriteShrinkCDError::SpriteShrink(error)) = result else {
            panic!("expected a sprite_shrink error, got {result:?}");
        };
        assert!(error.to_string().contains("Flac"), "{error}");
    }
}
