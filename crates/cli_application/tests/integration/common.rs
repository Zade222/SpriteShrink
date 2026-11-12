//! Helpers shared by the CLI's integration tests.

use std::{
    path::Path,
    process::{Command, Output},
};

/// The CLI binary, with its config, cache and data folders inside
/// `target/`, so a test run never touches the user's own. Even with
/// `--ignore-config` and `--disable-logging`, a run creates the cache
/// folder and prunes old logs from the data folder.
///
/// The `directories` crate reads these variables on Linux only. Elsewhere
/// the CLI uses the account's usual folders.
pub fn cli_command() -> Command {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli-home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_sprite-shrink"));
    command
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"));
    command
}

/// Runs the CLI with the flags every test uses: no config file, no log
/// files and one thread, so runs don't depend on the machine.
pub fn cli_output(args: &[&str]) -> Output {
    cli_command()
        .args(["--ignore-config", "--disable-logging", "--threads", "1"])
        .args(args)
        .output()
        .unwrap()
}

/// Whether the CLI reported an error, which it does through a non-zero
/// exit status.
pub fn reported_error(output: &Output) -> bool {
    !output.status.success()
}

/// Runs the CLI and fails the test if it reports an error.
pub fn run_cli(args: &[&str]) {
    let output = cli_output(args);

    assert!(
        !reported_error(&output),
        "sprite-shrink {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
