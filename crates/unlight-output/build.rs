use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("avif.*|Jxl.*")
        .allowlist_type("avif.*|Jxl.*")
        .allowlist_var("AVIF_.*|JXL_.*")
        .prepend_enum_name(false)
        .derive_default(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for name in ["libavif", "libjxl", "libjxl_threads"] {
        let library = pkg_config::probe_library(name)
            .unwrap_or_else(|error| panic!("{name} not found, enter `nix develop`: {error}"));
        for path in &library.include_paths {
            builder = builder.clang_arg(format!("-I{}", path.display()));
        }
    }

    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("bindings.rs");
    builder
        .generate()
        .expect("bindgen failed on the libavif and libjxl headers")
        .write_to_file(out)
        .expect("failed to write bindings");
}
