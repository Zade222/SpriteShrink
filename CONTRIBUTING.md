# Contributing to SpriteShrink

We welcome contributions to SpriteShrink! This document outlines the guidelines for contributing to the project, from setting up your development environment to submitting pull requests.

## Table of Contents

1. [Getting Started](#1-getting-started)
   * [Prerequisites](#prerequisites)
   * [Cloning the Repository](#cloning-the-repository)
   * [Building the Project](#building-the-project)
2. [Contribution Workflow](#2-contribution-workflow)
   * [Forking the Repository](#forking-the-repository)
   * [Creating a Branch](#creating-a-branch)
   * [Making Changes](#making-changes)
   * [Commit Messages](#commit-messages)
   * [Submitting a Pull Request](#submitting-a-pull-request)
3. [Coding Standards](#3-coding-standards)
   * [Rustfmt](#rustfmt)
   * [Documentation](#documentation)
   * [Testing](#testing)
   * [Error Handling](#error-handling)
4. [Reporting Issues](#4-reporting-issues)
   * [Bug Reports](#bug-reports)
   * [Feature Requests](#feature-requests)

## 1. Getting Started

### Prerequisites

To build and contribute to SpriteShrink, you will need:

* **Rust Toolchain:** Install Rust and Cargo using `rustup`. Follow the instructions on the [official Rust website](https://www.rust-lang.org/tools/install).
* **Platform-specific Build Tools:**
  * **Linux/Unix-like systems (including WSL):** `build-essential` (Debian/Ubuntu), "Development Tools" group (RHEL-based systems), or `base-devel` (Arch Linux), which provide a C compiler (GCC or Clang) and associated build utilities.
  * **macOS:** Xcode and its command-line tools.
  * **Windows:** Microsoft Visual C++ Build Tools (available as part of Visual Studio or standalone).

### Cloning the Repository

First, clone the SpriteShrink repository to your local machine:

```bash
git clone https://github.com/Zade222/SpriteShrink.git
cd SpriteShrink
```

### Building the Project

SpriteShrink is a Cargo workspace with three crates:

* `cli_application`: the `sprite-shrink` executable.
* `lib_sprite_shrink`: the core library (`sprite_shrink`), which handles chunking, deduplication, compression and the archive format.
* `lib_ss_cd`: the CD image library (`sprite_shrink_cd`), which handles CUE parsing, sector analysis and reconstruction, and FLAC audio compression for the `optical` mode.

You can build the entire project using a single Cargo command:

```bash
cargo build --release
```

This will compile the CLI application and both libraries in release mode.

## 2. Contribution Workflow

### Forking the Repository

If you plan to contribute, the first step is to fork the SpriteShrink repository on GitHub. This creates a copy of the repository under your GitHub account.

### Creating a Branch

After forking and cloning, create a new branch for your changes. Use a descriptive branch name (e.g., `feature/add-lzma-compression`, `fix/invalid-path-error`).

```bash
git checkout -b your-branch-name
```

### Making Changes

Implement your changes in your new branch. Remember to adhere to the [Coding Standards](#3-coding-standards) outlined below.

### Commit Messages

Write clear, concise, and descriptive commit messages. Follow the [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) specification where applicable.

Examples:

* `feat: add LZMA compression algorithm`
* `fix: resolve invalid path error on Windows`
* `docs: update contributing guidelines`
* `refactor: improve chunking logic performance`

### Submitting a Pull Request

Once your changes are complete, tested, and formatted, push your branch to your forked repository:

```bash
git push origin your-branch-name
```

Then, go to the original SpriteShrink GitHub repository and open a new Pull Request (PR) from your branch.

When creating your PR, please ensure you fill out the provided Pull Request template completely. This template includes a checklist to help you confirm that you've followed all project guidelines (e.g., tested, updated documentation, followed formatting rules).

## 3. Coding Standards

### Rustfmt

The codebase isn't formatted with `rustfmt` yet. Its style, such as aligned struct fields and wrapped comments, needs `rustfmt` options that are only available on nightly, so CI doesn't check formatting for now.

Until that changes:

* Match the style of the surrounding code, and keep lines within 80 characters.
* Don't run `cargo fmt` on whole files or crates. It reformats unrelated code, which makes a pull request hard to review.

### Documentation

All public items (functions, structs, enums, etc.) in the `lib_sprite_shrink` library must be documented in accordance with standard `rustdoc` conventions. This includes:

* A clear description of the item's purpose.
* Explanations for all parameters.
* Details on the return value.
* At least one working usage example (where applicable).

### Testing

Run the full test suite before submitting a pull request:

```bash
cargo test --workspace
```

CI runs the tests on Linux and Windows, in both debug and release mode, for every push and pull request. It also checks that the generated C header compiles, and runs the `lib_ss_cd` tests under QEMU on an emulated CPU without AVX2 so the non-AVX2 SIMD path is covered.

To run only part of the suite, filter by crate, test binary or module path:

```bash
cargo test -p sprite_shrink --test integration format_layout::
```

**Where tests go:**

* **Unit tests** go in a `#[cfg(test)] mod tests` at the bottom of the file they test. Use them for private functions, and to check which error variant a function returns.
* **Integration tests** go in `crates/<crate>/tests/integration/`. Each crate builds a single integration test binary from `main.rs`, which only declares modules, so add a new file there as a module. Integration tests use only the public API.
* Name tests `<unit>_<condition>_<expected>`, for example `parse_file_header_wrong_magic_returns_invalid_header`.

**Known bugs:** a test for a confirmed bug that isn't fixed yet is marked `#[ignore = "<what currently fails>"]`. Run these with `cargo test -- --ignored`. When you fix the bug, remove the `#[ignore]`.

**Lints:** `clippy.toml` allows `unwrap`, `expect`, indexing, `panic!` and `dbg!` in test code. Clippy only treats `#[test]` functions and `#[cfg(test)]` modules as test code, so declare each integration test module in `main.rs` with `#[cfg(test)]` to cover its helper functions too. For any other exception, use `#[expect(lint, reason = "...")]` rather than `#[allow]`.

**Baseline archives:** `crates/cli_application/tests/fixtures/baseline/` holds archives the CLI wrote from fixed generated input. The `baseline_archives` tests recompress that input and require identical bytes. If a change is meant to alter the archive bytes, regenerate the fixtures with `SPRITE_SHRINK_BLESS=1 cargo test -p sprite_shrink_application --test integration baseline_archives::` and commit them with the change.

**Benchmarks:** if a change touches sector reconstruction or the ECC code, compare its performance before and after:

```bash
cargo bench -p sprite_shrink_cd --bench sector_rebuild -- --save-baseline before
# apply your change, then:
cargo bench -p sprite_shrink_cd --bench sector_rebuild -- --baseline before
```

Differences of 1-2% are within run-to-run noise. For a smaller change, run the two builds alternately and pin them to one core (`taskset -c 2` on Linux).

To test the non-AVX2 path locally on Linux, install QEMU user-mode emulation and run:

```bash
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="qemu-x86_64 -cpu Westmere" cargo test -p sprite_shrink_cd
```

### Error Handling

The `lib_sprite_shrink` library defines a single, public error enum (`SpriteShrinkError`) and module specific enums that represents all possible failure conditions. All public, fallible functions in the library must use this error type in their returned `Result`.

When implementing new functionality, ensure that errors are handled gracefully and propagated using this `SpriteShrinkError` or module specific enum where appropriate.

## 4. Reporting Issues

### Bug Reports

If you find a bug, please open a new issue on GitHub. When reporting a bug, please use the provided Bug Report template and include:

* **Steps to reproduce:** Clear and concise instructions to replicate the issue.
* **Expected behavior:** What you expected to happen.
* **Actual behavior:** What actually happened, including any error messages or stack traces.
* **System information:** Your operating system, Rust version, and SpriteShrink version (if applicable).

### Feature Requests

If you have an idea for a new feature or enhancement, please open a new issue on GitHub. When requesting a feature, please use the provided Feature Request template and include:

* **Description of the problem:** What problem does this feature solve?
* **Proposed solution:** A clear description of how you envision the feature working.
* **Use case examples:** How would this feature be used in practice?
