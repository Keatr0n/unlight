//! What the Profile dropdown lists. A recipe naming no profile gets the photo's automatic one,
//! so choosing that one stores nothing, and the recipe stays as if it had never been touched.

use unlight_engine::{BUILT_IN_PROFILE, ProfileChoices};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileEntry {
    pub label: String,
    /// What the recipe stores for this entry.
    pub stored: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileMenu {
    pub current: String,
    pub entries: Vec<ProfileEntry>,
    /// The recipe names a profile this machine doesn't have.
    pub missing: bool,
}

fn stored_for(name: &str, automatic: Option<&str>) -> Option<String> {
    let is_automatic = automatic.map_or(name == BUILT_IN_PROFILE, |automatic| automatic == name);
    (!is_automatic).then(|| name.to_owned())
}

pub fn profile_menu(requested: Option<&str>, choices: &ProfileChoices) -> ProfileMenu {
    let automatic = choices.automatic.as_deref();
    let names = std::iter::once(BUILT_IN_PROFILE).chain(choices.available.iter().map(String::as_str));
    let entries: Vec<ProfileEntry> = names
        .map(|name| ProfileEntry { label: name.to_owned(), stored: stored_for(name, automatic) })
        .collect();
    let chosen = requested.or(automatic).unwrap_or(BUILT_IN_PROFILE);
    let missing = !entries.iter().any(|entry| entry.label == chosen);
    let current = if missing { format!("{chosen} (missing)") } else { chosen.to_owned() };
    ProfileMenu { current, entries, missing }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices(automatic: Option<&str>, available: &[&str]) -> ProfileChoices {
        ProfileChoices {
            automatic: automatic.map(str::to_owned),
            available: available.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn labels(menu: &ProfileMenu) -> Vec<&str> {
        menu.entries.iter().map(|entry| entry.label.as_str()).collect()
    }

    fn stored(menu: &ProfileMenu, label: &str) -> Option<String> {
        menu.entries.iter().find(|entry| entry.label == label).expect("an entry").stored.clone()
    }

    #[test]
    fn with_nothing_installed_the_default_is_the_only_entry() {
        let menu = profile_menu(None, &choices(None, &[]));
        assert_eq!(labels(&menu), ["Unlight Default"]);
        assert_eq!(menu.current, "Unlight Default");
        assert_eq!(stored(&menu, "Unlight Default"), None, "the recipe stays untouched");
    }

    #[test]
    fn the_default_can_be_chosen_over_a_profile_that_applies_by_itself() {
        let menu = profile_menu(None, &choices(Some("Adobe Standard"), &["Adobe Standard", "Vivid"]));
        assert_eq!(menu.current, "Adobe Standard");
        assert_eq!(stored(&menu, "Unlight Default"), Some(BUILT_IN_PROFILE.to_owned()));
        assert_eq!(stored(&menu, "Adobe Standard"), None);
        assert_eq!(stored(&menu, "Vivid"), Some("Vivid".to_owned()));
    }

    #[test]
    fn a_choice_shows_as_current() {
        let menu = profile_menu(Some("Vivid"), &choices(None, &["Vivid"]));
        assert_eq!(menu.current, "Vivid");
        assert!(!menu.missing);
    }

    #[test]
    fn a_profile_from_another_machine_says_so() {
        let menu = profile_menu(Some("Gone"), &choices(None, &["Vivid"]));
        assert_eq!(menu.current, "Gone (missing)");
        assert!(menu.missing);
        assert_eq!(labels(&menu), ["Unlight Default", "Vivid"]);
    }
}
