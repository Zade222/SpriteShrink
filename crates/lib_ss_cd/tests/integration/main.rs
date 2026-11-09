// `#[cfg(test)]` on each module lets the clippy.toml test allowances cover
// helper functions too, not only `#[test]` functions.

#[cfg(test)]
mod disc_circuit;

#[cfg(test)]
mod format_layout;

#[cfg(test)]
mod ssmd_prefix;
