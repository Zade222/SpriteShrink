use std::{
    thread,
};

use crate::{
    lib_error_handling::SpriteShrinkCDError,
};

use sprite_shrink::Hashable;


pub fn spawn_data_fetcher<H, D, E>(
    hash_list: Vec<H>,
    get_data: D,
    tx: flume::Sender<Result<Vec<Vec<u8>>, SpriteShrinkCDError>>,
    batch_size: usize,
) -> thread::JoinHandle<()>
where
    H: Hashable,
    D: Fn(&[H]) -> Result<Vec<Vec<u8>>, E> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    thread::spawn(move || {
        for hashes in hash_list.chunks(batch_size) {
            match get_data(hashes) {
                Ok(data) => {
                    if tx.send(Ok(data)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Data fetcher callback error: {}", e);
                    let _ = tx.send(Err(
                        SpriteShrinkCDError::External(Box::new(e))
                    ));
                    break;
                }
            }
        }
    })
}
