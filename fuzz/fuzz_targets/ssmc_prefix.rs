//! Parses arbitrary bytes as the start of an SSMC archive. Parsing may fail,
//! but must never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sprite_shrink::parse_ssmc_prefix;

fuzz_target!(|data: &[u8]| {
    // Only a panic is a failure, so the results are dropped.
    drop(parse_ssmc_prefix::<u64>(data));
    drop(parse_ssmc_prefix::<u128>(data));
});
