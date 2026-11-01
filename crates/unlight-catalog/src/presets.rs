use crate::{Catalog, Error, PresetId, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use unlight_recipe::Recipe;

/// Some of a recipe's settings, in the recipe's JSON shape. Applying one overwrites only the
/// settings it names. Lists such as masks are replaced whole.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartialRecipe(Map<String, Value>);

impl PartialRecipe {
    /// The settings where `edited` differs from `base`.
    pub fn between(base: &Recipe, edited: &Recipe) -> Result<Self> {
        let base = recipe_object(base)?;
        let edited = recipe_object(edited)?;
        Ok(Self(difference(&base, &edited)))
    }

    /// Wraps settings that came from elsewhere, such as a checklist of sections in the UI.
    pub fn from_settings(settings: Map<String, Value>) -> Self {
        Self(settings)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn apply_to(&self, recipe: &Recipe) -> Result<Recipe> {
        let mut merged = recipe_object(recipe)?;
        overlay(&mut merged, &self.0);
        Ok(serde_json::from_value(Value::Object(merged))?)
    }
}

fn recipe_object(recipe: &Recipe) -> Result<Map<String, Value>> {
    match serde_json::to_value(recipe)? {
        Value::Object(object) => Ok(object),
        _ => Err(Error::InvalidRecipe),
    }
}

fn difference(base: &Map<String, Value>, edited: &Map<String, Value>) -> Map<String, Value> {
    let mut changes = Map::new();
    for (key, value) in edited {
        match (base.get(key), value) {
            (Some(Value::Object(old)), Value::Object(new)) => {
                let nested = difference(old, new);
                if !nested.is_empty() {
                    changes.insert(key.clone(), Value::Object(nested));
                }
            }
            (Some(old), new) if old == new => {}
            (_, new) => {
                changes.insert(key.clone(), new.clone());
            }
        }
    }
    changes
}

fn overlay(target: &mut Map<String, Value>, changes: &Map<String, Value>) {
    for (key, value) in changes {
        match (target.get_mut(key), value) {
            (Some(Value::Object(old)), Value::Object(new)) => overlay(old, new),
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub id: PresetId,
    pub group: String,
    pub name: String,
    pub settings: PartialRecipe,
}

impl Catalog {
    /// Saves a preset, replacing any of the same name in the group.
    pub fn save_preset(
        &self,
        group: &str,
        name: &str,
        settings: &PartialRecipe,
    ) -> Result<PresetId> {
        let json = serde_json::to_string(settings)?;
        self.write(|tx| {
            let id = tx.query_row(
                "INSERT INTO presets (group_name, name, settings) VALUES (?1, ?2, ?3)
                 ON CONFLICT (group_name, name) DO UPDATE SET settings = excluded.settings
                 RETURNING id",
                params![group, name, json],
                |row| row.get(0),
            )?;
            Ok(id)
        })
    }

    /// All presets, ordered by group and then name.
    pub fn presets(&self) -> Result<Vec<Preset>> {
        let rows: Vec<(PresetId, String, String, String)> = self.read(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT id, group_name, name, settings FROM presets ORDER BY group_name, name",
            )?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        rows.into_iter()
            .map(|(id, group, name, json)| {
                Ok(Preset { id, group, name, settings: serde_json::from_str(&json)? })
            })
            .collect()
    }

    pub fn delete_preset(&self, id: PresetId) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM presets WHERE id = ?1", [id])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_recipe::WhiteBalance;

    #[test]
    fn a_difference_names_only_what_changed() {
        let mut edited = Recipe::default();
        edited.basic.exposure = 1.5;
        let partial = PartialRecipe::between(&Recipe::default(), &edited).unwrap();
        assert_eq!(serde_json::to_string(&partial).unwrap(), r#"{"basic":{"exposure":1.5}}"#);
    }

    #[test]
    fn applying_leaves_unnamed_settings_alone() {
        let mut edited = Recipe::default();
        edited.basic.exposure = 1.5;
        let partial = PartialRecipe::between(&Recipe::default(), &edited).unwrap();

        let mut target = Recipe::default();
        target.basic.contrast = 30.0;
        target.basic.exposure = -2.0;
        let applied = partial.apply_to(&target).unwrap();
        assert_eq!(applied.basic.exposure, 1.5);
        assert_eq!(applied.basic.contrast, 30.0);
    }

    #[test]
    fn white_balance_switches_mode_in_both_directions() {
        let custom = WhiteBalance::Custom { temperature: 4800.0, tint: 12.0 };
        let edited = Recipe { white_balance: custom, ..Recipe::default() };
        let to_custom = PartialRecipe::between(&Recipe::default(), &edited).unwrap();
        assert_eq!(to_custom.apply_to(&Recipe::default()).unwrap().white_balance, custom);

        let to_as_shot = PartialRecipe::between(&edited, &Recipe::default()).unwrap();
        assert_eq!(to_as_shot.apply_to(&edited).unwrap().white_balance, WhiteBalance::AsShot);
    }

    #[test]
    fn a_list_is_replaced_whole() {
        let mut edited = Recipe::default();
        edited.masks.push(Default::default());
        let partial = PartialRecipe::between(&Recipe::default(), &edited).unwrap();

        let target =
            Recipe { masks: vec![Default::default(), Default::default()], ..Recipe::default() };
        assert_eq!(partial.apply_to(&target).unwrap().masks.len(), 1);
    }

    #[test]
    fn a_setting_of_the_wrong_type_is_an_error() {
        let settings = serde_json::json!({"basic": {"exposure": "bright"}});
        let Value::Object(settings) = settings else { unreachable!() };
        assert!(PartialRecipe::from_settings(settings).apply_to(&Recipe::default()).is_err());
    }
}
