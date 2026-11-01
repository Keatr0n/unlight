#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]
#![allow(unsafe_op_in_unsafe_fn, clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

/// The database shipped with the lensfun this crate was built against. Nix keeps it in the
/// runtime closure, because the linked library's store path contains it.
pub const DATABASE_DIR: &str = env!("UNLIGHT_LENSFUN_DATADIR");
