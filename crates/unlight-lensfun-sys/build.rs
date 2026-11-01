use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");

    let library = pkg_config::Config::new()
        .atleast_version("0.3.4")
        .probe("lensfun")
        .expect("lensfun not found, enter the dev shell with `nix develop`");
    let data = pkg_config::get_variable("lensfun", "datadir")
        .expect("lensfun.pc has no datadir, so the lens database can't be located");
    println!("cargo:rustc-env=UNLIGHT_LENSFUN_DATADIR={data}/lensfun/version_1");

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("lf_.*")
        .allowlist_type("lf.*")
        .allowlist_var("LF_.*")
        .prepend_enum_name(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for path in &library.include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    builder
        .generate()
        .expect("bindgen failed on lensfun.h")
        .write_to_file(out)
        .expect("failed to write lensfun bindings");
}
