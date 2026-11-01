use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");

    let library = pkg_config::Config::new()
        .atleast_version("0.22")
        .probe("libraw_r")
        .expect("libraw_r not found, enter the dev shell with `nix develop`");

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_var("LIBRAW_.*")
        .allowlist_type("LibRaw_.*")
        .prepend_enum_name(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for path in &library.include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    builder
        .generate()
        .expect("bindgen failed on libraw.h")
        .write_to_file(out)
        .expect("failed to write LibRaw bindings");
}
