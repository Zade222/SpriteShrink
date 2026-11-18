# Fuzz targets

Each target feeds generated input to code that reads untrusted data and
checks that it never panics. Errors are expected; panics, hangs and
runaway memory use are bugs.

| Target | Input | Code under test |
|---|---|---|
| `ssmc_prefix` | archive bytes | `parse_ssmc_prefix` (u64 and u128 hashes) |
| `ssmc_read` | archive bytes | the prefix, then `get_seek_chunks` and `decompress_chunk_checked` for each file |
| `ssmd_prefix` | archive bytes | `parse_ssmd_prefix` (u64 and u128 hashes) |
| `ssmd_read` | archive bytes | the prefix, then `DiscReconstructor::get_sector_data` and `write_disc` for each disc |
| `cue_sheet` | 8-byte file size, then text | `analyze_cue_sheets`, including the CUE parser |
| `data_sector` | one 2352-byte sector | `analyze_data_sector` for each data sector type |

`analyze_data_sector` isn't part of the public API. `cargo fuzz` builds
with `--cfg fuzzing`, which exports it for this crate only.

## Running

The targets need nightly and `cargo-fuzz`. From this folder:

```sh
cargo +nightly fuzz run ssmd_read -- -max_len=131072
```

`-max_len` raises libFuzzer's default 4 KiB input limit so whole archives
fit. Add `-max_total_time=<seconds>` to stop after a while.

The archive targets do best when seeded with real archives. The corpus
folders are not committed; seed them from the test fixtures:

```sh
for t in ssmc_prefix ssmc_read ssmd_prefix ssmd_read; do
    mkdir -p corpus/$t
    cp ../crates/cli_application/tests/fixtures/baseline/* corpus/$t/
done
```

## When a target finds a crash

libFuzzer saves the input under `artifacts/<target>/`. Reproduce it with:

```sh
cargo +nightly fuzz run <target> artifacts/<target>/<file>
```

Once it's fixed, add the input to the crate's unit or integration tests as
a regression test, so the normal test suite keeps checking it.

`ssmd_read` currently stops on panics inside `flac-codec` 1.3.3, which
decodes the archive's audio blocks and panics on some damaged ones instead
of returning an error. Until a fixed release is out, run it in fork mode so
it keeps going after a crash, then sort the saved inputs by where they
panic:

```sh
cargo +nightly fuzz run ssmd_read -- -fork=8 -ignore_crashes=1 -max_len=131072
```
