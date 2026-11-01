//! Smart collection rules. They are stored as JSON and compiled to parameterised SQL over the
//! same `v`, `p` query the library filter uses.

use crate::{
    Comparison, Flag, Label, TextOp,
    clause::{Clause, subtree_range},
};
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

const KEYWORD_SUBQUERY: &str = "SELECT vk.variant_id FROM variant_keywords vk
    JOIN keywords k ON k.id = vk.keyword_id";
const FOLDER_SUBQUERY: &str = "SELECT id FROM folders";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    #[default]
    All,
    Any,
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SmartRules {
    pub mode: MatchMode,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "field", rename_all = "snake_case")]
pub enum Rule {
    Rating {
        op: Comparison,
        value: u8,
    },
    Flag {
        value: Flag,
    },
    Label {
        value: Label,
    },
    Edited {
        value: bool,
    },
    Camera {
        op: TextOp,
        value: String,
    },
    Lens {
        op: TextOp,
        value: String,
    },
    FileName {
        op: TextOp,
        value: String,
    },
    /// Matches keyword names, at any level of the hierarchy.
    Keyword {
        op: TextOp,
        value: String,
    },
    /// Matches a photo in this folder or anywhere below it.
    Folder {
        path: String,
    },
    /// Seconds since the Unix epoch.
    CaptureTime {
        op: Comparison,
        value: i64,
    },
    Iso {
        op: Comparison,
        value: f64,
    },
    Aperture {
        op: Comparison,
        value: f64,
    },
    FocalLength {
        op: Comparison,
        value: f64,
    },
    Shutter {
        op: Comparison,
        value: f64,
    },
    Group(SmartRules),
}

impl SmartRules {
    pub(crate) fn clause(&self) -> Clause {
        let parts: Vec<Clause> = self.rules.iter().map(Rule::clause).collect();
        if parts.is_empty() {
            let matches_everything = self.mode != MatchMode::Any;
            return Clause::new(if matches_everything { "1" } else { "0" }, []);
        }
        match self.mode {
            MatchMode::All => Clause::join(parts, " AND "),
            MatchMode::Any => Clause::join(parts, " OR "),
            MatchMode::None => Clause::join(parts, " OR ").negate(),
        }
    }
}

impl Rule {
    fn clause(&self) -> Clause {
        match self {
            Self::Rating { op, value } => Clause::compare("v.rating", *op, *value as i64),
            Self::Flag { value } => Clause::compare("v.flag", Comparison::Equal, value.code()),
            Self::Label { value } => Clause::compare("v.label", Comparison::Equal, value.code()),
            Self::Edited { value } => edited_clause(*value),
            Self::Camera { op, value } => Clause::text("p.camera", *op, value),
            Self::Lens { op, value } => Clause::text("p.lens", *op, value),
            Self::FileName { op, value } => Clause::text("p.file_name", *op, value),
            Self::Keyword { op, value } => {
                Clause::related_text("v.id", KEYWORD_SUBQUERY, "k.name", *op, value)
            }
            Self::Folder { path } => folder_clause(path),
            Self::CaptureTime { op, value } => Clause::compare("p.capture_time", *op, *value),
            Self::Iso { op, value } => Clause::compare("p.iso", *op, *value),
            Self::Aperture { op, value } => Clause::compare("p.aperture", *op, *value),
            Self::FocalLength { op, value } => Clause::compare("p.focal_length", *op, *value),
            Self::Shutter { op, value } => Clause::compare("p.shutter", *op, *value),
            Self::Group(group) => group.clause(),
        }
    }
}

pub(crate) fn edited_clause(edited: bool) -> Clause {
    let test = if edited { "IS NOT NULL" } else { "IS NULL" };
    Clause::new(format!("v.edited_at {test}"), [])
}

fn folder_clause(path: &str) -> Clause {
    let (low, high) = subtree_range(path);
    Clause::new(
        format!("p.folder_id IN ({FOLDER_SUBQUERY} WHERE path = ? OR (path >= ? AND path < ?))"),
        [Value::from(path.to_owned()), low.into(), high.into()],
    )
}

pub(crate) fn text_search_clause(term: &str) -> Clause {
    let columns = ["p.file_name", "p.camera", "p.lens"];
    let mut parts: Vec<_> =
        columns.iter().map(|column| Clause::text(column, TextOp::Contains, term)).collect();
    parts.push(Clause::related_text("v.id", KEYWORD_SUBQUERY, "k.name", TextOp::Contains, term));
    Clause::join(parts, " OR ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_round_trip_through_json() {
        let rules = SmartRules {
            mode: MatchMode::All,
            rules: vec![
                Rule::Rating { op: Comparison::GreaterOrEqual, value: 3 },
                Rule::Group(SmartRules {
                    mode: MatchMode::Any,
                    rules: vec![
                        Rule::Camera { op: TextOp::Contains, value: "Sony".into() },
                        Rule::Keyword { op: TextOp::Is, value: "wedding".into() },
                    ],
                }),
            ],
        };
        let json = serde_json::to_string(&rules).unwrap();
        assert!(json.contains(r#""field":"group""#));
        assert_eq!(serde_json::from_str::<SmartRules>(&json).unwrap(), rules);
    }

    #[test]
    fn values_only_reach_sql_as_parameters() {
        let hostile = "x'; DROP TABLE photos; --";
        let clause = Rule::Camera { op: TextOp::Is, value: hostile.into() }.clause();
        assert!(!clause.sql.contains("DROP"));
        assert_eq!(clause.params, [Value::from(hostile.to_owned())]);
    }

    #[test]
    fn an_empty_group_follows_its_mode() {
        let group = |mode| SmartRules { mode, rules: Vec::new() }.clause().sql;
        assert_eq!(group(MatchMode::All), "1");
        assert_eq!(group(MatchMode::None), "1");
        assert_eq!(group(MatchMode::Any), "0");
    }
}
