use anyhow::Result;
use std::{env, path::PathBuf};

fn main() -> Result<()> {
    let paths: Vec<PathBuf> = env::args_os().skip(1).map(PathBuf::from).collect();
    unlight_app::run(&paths)
}
