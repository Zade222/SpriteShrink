// `#[cfg(test)]` on each module lets the clippy.toml test allowances cover
// helper functions too, not only `#[test]` functions.

#[cfg(test)]
mod baseline_archives;
