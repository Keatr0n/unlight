//! Soft proofing's settings. They belong to the viewing session, not the photo: nothing here
//! reaches a recipe, a history step or an export.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use unlight_engine::{OutputSpace, Proof, ProofIntent, ProofLut, ProofTarget};

/// The spaces built in, in the order they're listed.
pub const BUILT_IN_TARGETS: [OutputSpace; 3] = [OutputSpace::Srgb, OutputSpace::DisplayP3, OutputSpace::AdobeRgb];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProofChoice {
    Space(OutputSpace),
    Icc(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IccProfile {
    pub path: PathBuf,
    pub name: String,
}

/// One table to bake: a profile can be rendered either way.
pub type BakeKey = (PathBuf, ProofIntent);

#[derive(Clone, Debug, PartialEq)]
pub struct ProofState {
    pub enabled: bool,
    pub choice: ProofChoice,
    pub intent: ProofIntent,
    pub gamut_warning: bool,
    /// ICC profiles to choose from, found on disk or added by hand.
    pub profiles: Vec<IccProfile>,
    /// Whether the folders have been looked through yet.
    pub searched: bool,
    baked: HashMap<BakeKey, Arc<ProofLut>>,
    baking: Option<BakeKey>,
    refused: HashMap<BakeKey, String>,
}

impl Default for ProofState {
    fn default() -> Self {
        Self {
            enabled: false,
            choice: ProofChoice::Space(OutputSpace::Srgb),
            intent: ProofIntent::default(),
            gamut_warning: true,
            profiles: Vec::new(),
            searched: false,
            baked: HashMap::new(),
            baking: None,
            refused: HashMap::new(),
        }
    }
}

impl ProofState {
    fn wanted(&self) -> Option<BakeKey> {
        match &self.choice {
            ProofChoice::Icc(path) if self.enabled => Some((path.clone(), self.intent)),
            _ => None,
        }
    }

    /// What the view should proof against. An ICC profile has nothing to offer until its table
    /// is ready, and the photo stays as it is until then.
    pub fn view(&self) -> Option<Proof> {
        if !self.enabled {
            return None;
        }
        let target = match &self.choice {
            ProofChoice::Space(space) => ProofTarget::Space(*space),
            ProofChoice::Icc(path) => ProofTarget::Table(self.baked.get(&(path.clone(), self.intent))?.clone()),
        };
        Some(Proof { target, gamut_warning: self.gamut_warning })
    }

    /// The table to start baking now, if there is one. Only one runs at a time.
    pub fn take_bake(&mut self) -> Option<BakeKey> {
        let key = self.wanted().filter(|key| !self.baked.contains_key(key) && !self.refused.contains_key(key))?;
        if self.baking.is_some() {
            return None;
        }
        self.baking = Some(key.clone());
        Some(key)
    }

    pub fn finish_bake(&mut self, key: BakeKey, outcome: Result<Arc<ProofLut>, String>) {
        self.baking = None;
        match outcome {
            Ok(table) => {
                self.baked.insert(key, table);
            }
            Err(message) => {
                self.refused.insert(key, message);
            }
        }
    }

    pub fn is_baking(&self) -> bool {
        self.baking.is_some()
    }

    /// Why the chosen profile can't be used.
    pub fn refusal(&self) -> Option<&str> {
        self.refused.get(&self.wanted()?).map(String::as_str)
    }

    /// Adds a profile, keeping what's listed in name order.
    pub fn add_profile(&mut self, profile: IccProfile) {
        self.profiles.retain(|existing| existing.path != profile.path);
        self.profiles.push(profile);
        self.profiles.sort_by_key(|profile| profile.name.to_lowercase());
    }

    pub fn name_of(&self, choice: &ProofChoice) -> String {
        match choice {
            ProofChoice::Space(space) => space.name().to_owned(),
            ProofChoice::Icc(path) => self
                .profiles
                .iter()
                .find(|profile| profile.path == *path)
                .map_or_else(|| file_name(path), |profile| profile.name.clone()),
        }
    }

    pub fn chosen_name(&self) -> String {
        self.name_of(&self.choice)
    }
}

fn file_name(path: &Path) -> String {
    path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(id: u64) -> Arc<ProofLut> {
        Arc::new(ProofLut { id, size: 1, entries: vec![[0.0; 4]] })
    }

    fn with_icc() -> ProofState {
        ProofState { enabled: true, choice: ProofChoice::Icc("/p/printer.icc".into()), ..ProofState::default() }
    }

    #[test]
    fn off_means_no_proof_whatever_is_chosen() {
        let state = ProofState::default();
        assert_eq!(state.view(), None);
    }

    #[test]
    fn a_built_in_space_needs_no_baking() {
        let mut state = ProofState {
            enabled: true,
            choice: ProofChoice::Space(OutputSpace::AdobeRgb),
            ..ProofState::default()
        };
        assert_eq!(state.take_bake(), None);
        let view = state.view().expect("a proof");
        assert_eq!(view.target, ProofTarget::Space(OutputSpace::AdobeRgb));
        assert!(view.gamut_warning);
    }

    #[test]
    fn an_icc_profile_shows_only_once_its_table_lands() {
        let mut state = with_icc();
        assert_eq!(state.view(), None);
        let key = state.take_bake().expect("a bake");
        assert_eq!(state.take_bake(), None, "one at a time");
        assert!(state.is_baking());
        state.finish_bake(key, Ok(table(7)));
        assert!(!state.is_baking());
        assert_eq!(state.take_bake(), None, "already baked");
        assert_eq!(state.view().map(|proof| proof.target), Some(ProofTarget::Table(table(7))));
    }

    #[test]
    fn each_intent_gets_its_own_table() {
        let mut state = with_icc();
        let key = state.take_bake().expect("a bake");
        state.finish_bake(key, Ok(table(1)));
        state.intent = ProofIntent::Perceptual;
        assert_eq!(state.view(), None);
        assert!(state.take_bake().is_some());
    }

    #[test]
    fn a_refused_profile_says_why_and_is_not_retried() {
        let mut state = with_icc();
        let key = state.take_bake().expect("a bake");
        state.finish_bake(key, Err("not a profile".to_owned()));
        assert_eq!(state.refusal(), Some("not a profile"));
        assert_eq!(state.take_bake(), None);
    }

    #[test]
    fn switching_off_leaves_nothing_to_bake() {
        let mut state = with_icc();
        state.enabled = false;
        assert_eq!(state.take_bake(), None);
    }

    #[test]
    fn profiles_stay_sorted_and_unique() {
        let mut state = ProofState::default();
        let profile = |path: &str, name: &str| IccProfile { path: path.into(), name: name.to_owned() };
        state.add_profile(profile("/b.icc", "beta"));
        state.add_profile(profile("/a.icc", "Alpha"));
        state.add_profile(profile("/b.icc", "Beta 2"));
        let names: Vec<&str> = state.profiles.iter().map(|profile| profile.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Beta 2"]);
    }
}
