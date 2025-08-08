extern crate cbindgen;

use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let package_name = env::var("CARGO_PKG_NAME").unwrap();
    let output_file = target_dir()
        .join(format!("{package_name}.h"))
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
    let mut path = if let Ok(target_dir) = env::var("CARGO_TARGET_DIR") {
        PathBuf::from(target_dir)
    } else {
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../target")
    };

    if let Ok(target_triple) = env::var("TARGET") {
        path = path.join(target_triple);
    }

    if let Ok(profile) = env::var("PROFILE") {
        path = path.join(profile);
    }

    path
}