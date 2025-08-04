extern crate cbindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let package_name = env::var("CARGO_PKG_NAME").unwrap();
    let output_file = target_dir()
        .join(format!("{}.h", package_name))
        .to_str()
        .unwrap()
        .to_string();

    let config = cbindgen::Config::from_file("cbindgen.toml").expect("Failed to load cbindgen.toml");

    cbindgen::Builder::new()
      .with_crate(&crate_dir)
      .with_config(config)
      .generate()
      .expect("Unable to generate C bindings")
      .write_to_file(&output_file);
}

fn target_dir() -> PathBuf {
    if let Ok(target) = env::var("CARGO_TARGET_DIR") {
        PathBuf::from(target)
    } else {
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../target")
    }
}