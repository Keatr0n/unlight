//! The list of models Unlight knows how to fetch. It ships with the program, so a download can
//! only ever be a file whose exact hash was reviewed in the repository.

use crate::error::{Error, Result};
use serde::Deserialize;

const BUNDLED: &str = include_str!("../models.toml");
const SHA256_HEX_LENGTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    Subject,
    Sky,
    Denoise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputScale {
    /// Raw scores that still need a sigmoid.
    Logits,
    Probability,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SegmentationSpec {
    pub input_size: u32,
    pub mean: [f32; 3],
    pub std: [f32; 3],
    pub output: OutputScale,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RestorationSpec {
    /// The network's tile size, which must suit its downsampling.
    pub tile: u32,
    /// How far neighbouring tiles share pixels, so their joins can be blended away.
    pub overlap: u32,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Network {
    Segmentation(SegmentationSpec),
    Restoration(RestorationSpec),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Model {
    pub name: String,
    pub title: String,
    pub task: Task,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub licence: String,
    pub licence_url: String,
    pub network: Network,
}

impl Model {
    /// What to ask before fetching this model. Nothing is downloaded without an explicit yes.
    pub fn consent_prompt(&self) -> String {
        format!(
            "This tool needs a {} MB model, {} licence. It is downloaded once and then runs entirely on this computer. Download?",
            self.size_in_megabytes(),
            self.licence
        )
    }

    pub(crate) fn segmentation(&self) -> Result<&SegmentationSpec> {
        match &self.network {
            Network::Segmentation(spec) => Ok(spec),
            Network::Restoration(_) => Err(self.wrong_network()),
        }
    }

    pub(crate) fn restoration(&self) -> Result<&RestorationSpec> {
        match &self.network {
            Network::Restoration(spec) => Ok(spec),
            Network::Segmentation(_) => Err(self.wrong_network()),
        }
    }

    fn wrong_network(&self) -> Error {
        Error::Manifest(format!("{} can't do this job", self.name))
    }

    pub fn size_in_megabytes(&self) -> u64 {
        (self.size + 500_000) / 1_000_000
    }

    fn validate(&self) -> Result<()> {
        let invalid = |problem: &str| Err(Error::Manifest(format!("{}: {problem}", self.name)));
        let safe_name = !self.name.is_empty()
            && self.name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !safe_name {
            return invalid("the name must be lowercase letters, digits and hyphens");
        }
        if !self.url.starts_with("https://") {
            return invalid("the URL must use https");
        }
        if self.size == 0 {
            return invalid("the size must not be zero");
        }
        if self.sha256.len() != SHA256_HEX_LENGTH
            || !self.sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return invalid("the SHA-256 must be 64 lowercase hex digits");
        }
        if self.licence.is_empty() || self.licence_url.is_empty() {
            return invalid("the licence must be stated");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Manifest {
    #[serde(rename = "model")]
    pub models: Vec<Model>,
}

impl Manifest {
    pub fn bundled() -> Result<Self> {
        Self::parse(BUNDLED)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let manifest: Self = toml::from_str(text).map_err(|error| Error::Manifest(error.to_string()))?;
        for (index, model) in manifest.models.iter().enumerate() {
            model.validate()?;
            if manifest.models[..index].iter().any(|earlier| earlier.name == model.name) {
                return Err(Error::Manifest(format!("{} is listed twice", model.name)));
            }
        }
        Ok(manifest)
    }

    pub fn find(&self, name: &str) -> Result<&Model> {
        self.models
            .iter()
            .find(|model| model.name == name)
            .ok_or_else(|| Error::UnknownModel(name.to_owned()))
    }

    pub fn for_task(&self, task: Task) -> Option<&Model> {
        self.models.iter().find(|model| model.task == task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
        [[model]]
        name = "tiny-1"
        title = "Tiny"
        task = "subject"
        url = "https://example.com/tiny.onnx"
        size = 1500000
        sha256 = "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333"
        licence = "MIT"
        licence_url = "https://example.com/LICENSE"

        [model.network]
        kind = "segmentation"
        input_size = 64
        mean = [0.5, 0.5, 0.5]
        std = [0.5, 0.5, 0.5]
        output = "logits"
    "#;

    fn expect_invalid(text: String, reason: &str) {
        match Manifest::parse(&text) {
            Err(Error::Manifest(message)) => assert!(message.contains(reason), "{message}"),
            other => panic!("expected a manifest error about {reason}, got {other:?}"),
        }
    }

    #[test]
    fn the_bundled_manifest_is_valid_and_covers_every_selection_task() {
        let manifest = Manifest::bundled().unwrap();
        assert!(manifest.for_task(Task::Subject).is_some());
        assert!(manifest.for_task(Task::Sky).is_some());
        assert!(manifest.for_task(Task::Denoise).is_some());
    }

    #[test]
    fn parses_every_field() {
        let manifest = Manifest::parse(VALID).unwrap();
        let model = manifest.find("tiny-1").unwrap();
        assert_eq!(model.task, Task::Subject);
        assert_eq!(model.size, 1_500_000);
        let spec = model.segmentation().unwrap();
        assert_eq!(spec.input_size, 64);
        assert_eq!(spec.output, OutputScale::Logits);
    }

    #[test]
    fn the_prompt_states_size_and_licence_before_anything_is_fetched() {
        let manifest = Manifest::parse(VALID).unwrap();
        let prompt = manifest.find("tiny-1").unwrap().consent_prompt();
        assert!(prompt.contains("2 MB"), "{prompt}");
        assert!(prompt.contains("MIT licence"), "{prompt}");
    }

    #[test]
    fn a_name_that_could_escape_the_model_directory_is_rejected() {
        expect_invalid(VALID.replace("tiny-1", "../../evil"), "name");
    }

    #[test]
    fn plain_http_is_rejected() {
        expect_invalid(VALID.replace("https://example.com/tiny", "http://example.com/tiny"), "https");
    }

    #[test]
    fn a_malformed_hash_is_rejected() {
        expect_invalid(VALID.replace("56000243", "ZZ000243"), "SHA-256");
        expect_invalid(VALID.replace("5600024376f572a5", ""), "SHA-256");
    }

    #[test]
    fn a_missing_licence_is_rejected() {
        expect_invalid(VALID.replace("licence = \"MIT\"", "licence = \"\""), "licence");
        expect_invalid(VALID.replace("licence = \"MIT\"\n", ""), "licence");
    }

    #[test]
    fn duplicate_names_are_rejected() {
        expect_invalid(format!("{VALID}\n{VALID}"), "listed twice");
    }

    #[test]
    fn an_unknown_model_is_an_error() {
        let manifest = Manifest::parse(VALID).unwrap();
        assert!(matches!(manifest.find("nope"), Err(Error::UnknownModel(_))));
    }
}
