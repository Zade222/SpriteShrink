extern crate cbindgen;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let package_name = env::var("CARGO_PKG_NAME").unwrap();
    let output_file = target_dir()
        .join(format!("{package_name}.h"))
        .to_str()
        .unwrap()
        .to_string();

    let config = cbindgen::Config::from_file("cbindgen.toml")
        .expect("Failed to load cbindgen.toml");

    cbindgen::Builder::new()
      .with_crate(&crate_dir)
      .with_config(config)
      .generate()
      .expect("Unable to generate C bindings")
      .write_to_file(&output_file);

    /* The following is put in place to work around a problem with cbindgen not
     * generating the FFIProgressType enum correctly. */
    let original_content = fs::read_to_string(&output_file)
        .expect("Failed to read generated header file for patching");

    let modified_content = original_content.replace(
        "enum FFIProgressType {",
        "enum {",
    );

    if original_content == modified_content {
        println!("cargo:warning=Could not find 'enum FFIProgressType {{' to patch in {}. The cbindgen output may have changed.", output_file);
    }

    fs::write(&output_file, modified_content)
        .expect("Failed to write patched header file");
    //end workaround
}

fn target_dir() -> PathBuf {
    let mut path = if let Ok(target_dir) = env::var("CARGO_TARGET_DIR") {
        PathBuf::from(target_dir)
    } else {
        PathBuf::from(env::var("CARGO_MANIFEST_DIR")
            .unwrap())
            .join("../../target")
    };

    if let Ok(target_triple) = env::var("TARGET") {
        path = path.join(target_triple);
    }

    if let Ok(profile) = env::var("PROFILE") {
        path = path.join(profile);
    }

    path
}
