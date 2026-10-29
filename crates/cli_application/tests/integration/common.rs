//! Helpers shared by the CLI's integration tests.

use std::process::{Command, Output};

/// Runs the CLI with the flags every test uses: no config file, no log
/// files and one thread, so runs don't depend on the machine.
pub fn cli_output(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sprite-shrink"))
        .args(["--ignore-config", "--disable-logging", "--threads", "1"])
        .args(args)
        .output()
        .unwrap()
}

/// Whether the CLI reported an error. The CLI exits 0 even when it fails,
/// so the output is checked as well as the status.
pub fn reported_error(output: &Output) -> bool {
    !output.status.success()
        || String::from_utf8_lossy(&output.stdout).contains("encountered an error")
        || String::from_utf8_lossy(&output.stderr).contains("encountered an error")
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
