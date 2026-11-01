//! Shows which camera profile a photo resolves to, and why a `.dcp` might not be in use.

use anyhow::Result;
use std::path::Path;
use unlight_color::{PhotoColor, ProfileLibrary, profile_directory};
use unlight_recipe::Recipe;

pub fn report(file: &Path, recipe: &Recipe) -> Result<()> {
    let raw = unlight_raw::load(file)?;
    let camera = raw.metadata.camera();
    let library = ProfileLibrary::installed();
    println!("camera       {camera}");
    if let Some(directory) = profile_directory() {
        println!("directory    {}", directory.display());
    }
    let profile = PhotoColor::new(&raw).profile(recipe.camera_profile.as_deref(), &library);
    match profile.rendering() {
        Some(rendering) => println!("profile      {}", rendering.name),
        None => println!("profile      built-in"),
    }
    for installed in library.for_camera(&camera) {
        println!("installed    {} ({})", installed.name, installed.path.display());
    }
    for (path, error) in &library.rejected {
        println!("rejected     {}: {error}", path.display());
    }
    Ok(())
}
