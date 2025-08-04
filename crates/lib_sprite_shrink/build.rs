extern crate cbindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR env var is not set");
    let crate_path = PathBuf::from(crate_dir);

    let config_path = crate_path.join("../../cbindgen.toml");
    let config = cbindgen::Config::from_file(&config_path)
        .unwrap_or_else(|_| panic!("Failed to load cbindgen.toml from: {:?}", config_path));

    let package_name = env::var("CARGO_PKG_NAME").unwrap();
    let output_file = target_dir()
        .join(format!("{}.h", package_name));

    cbindgen::Builder::new()
      .with_crate(crate_path)
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