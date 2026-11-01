//! Asks a photo what the tools would see at a point, the way the Develop tools do on a click.

use anyhow::{Result, bail};
use clap::Args;
use std::path::PathBuf;
use unlight_engine::{Engine, GpuContext, Precision, Session};
use unlight_recipe::Recipe;

#[derive(Args)]
pub struct ProbeArgs {
    file: PathBuf,
    /// Normalised source position, as X,Y.
    #[arg(long, value_delimiter = ',', required = true)]
    at: Vec<f32>,
    /// Spot radius as a share of the long edge, for the healing source.
    #[arg(long, default_value_t = 0.02)]
    radius: f32,
}

pub fn run(args: &ProbeArgs) -> Result<()> {
    let &[x, y] = args.at.as_slice() else {
        bail!("--at takes X,Y");
    };
    let raw = unlight_raw::load(&args.file)?;
    let recipe = Recipe::default();
    let engine = Engine::new(GpuContext::new()?, Precision::F16)?;
    let mut session = Session::open(engine, &raw, &recipe)?;
    let at = [x, y];
    println!("colour       {:?}", session.scene_color(&recipe, at)?);
    println!("heal source  {:?}", session.find_heal_source(&recipe, at, args.radius)?);
    Ok(())
}
