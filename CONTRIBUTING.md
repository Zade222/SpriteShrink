# Contributing to SpriteShrink

We welcome contributions to SpriteShrink! This document outlines the guidelines for contributing to the project, from setting up your development environment to submitting pull requests.

## Table of Contents

1. [Getting Started](#1-getting-started)
   * [Prerequisites](#prerequisites)
   * [Cloning the Repository](#cloning-the-repository)
   * [Building the Project](#building-the-project)
   * [Running Tests](#running-tests)
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

SpriteShrink is a Cargo workspace with two main crates: `cli_application` (the executable) and `lib_sprite_shrink` (the library). You can build the entire project using a single Cargo command:

```bash
cargo build --release
```

This will compile both the CLI application and the library in release mode.

### Running Tests

It is crucial to run all tests before submitting a pull request to ensure your changes haven't introduced any regressions.

You can run the full test suite for the entire project from the root directory:

```bash
cargo test
```

This command will compile all crates and run all unit and integration tests. All tests must pass successfully.

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

When creating your PR, please ensure you fill out the provided Pull Request template completely. This template includes a checklist to help you confirm that you've followed all project guidelines (e.g., run tests, updated documentation, followed formatting rules).

## 3. Coding Standards

### Rustfmt

All source code merged into the `main` branch must adhere to the standard Rust style guidelines enforced by the `rustfmt` tool.

You can check and format your code locally using:

```bash
cargo fmt --check # To check for formatting issues
cargo fmt         # To automatically format your code
```

Our Continuous Integration (CI) pipeline will automatically run `cargo fmt --check` for every submitted pull request, and it must pass before your PR can be merged.

### Documentation

All public items (functions, structs, enums, etc.) in the `lib_sprite_shrink` library must be documented in accordance with standard `rustdoc` conventions. This includes:

* A clear description of the item's purpose.
* Explanations for all parameters.
* Details on the return value.
* At least one working usage example (where applicable).

The build process is configured to enforce this, and missing documentation for public items will cause the build to fail.

### Testing

Maintain a high and consistent level of automated test coverage for all primary application and library code.

* `lib_sprite_shrink` library: Aim for at least 85% line coverage.
* `cli_application` executable: Aim for at least 80% line coverage.

A failure to meet this threshold in a proposed code change will cause the automated build to fail. Please write unit and integration tests for your new features or bug fixes.

### Error Handling

The `lib_sprite_shrink` library defines a single, public error enum (`LibError`) that represents all possible failure conditions. All public, fallible functions in the library must use this error type in their returned `Result`.

When implementing new functionality, ensure that errors are handled gracefully and propagated using this `LibError` enum where appropriate.

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
