//! The smart collection editor's working copy. A rule is edited as plain fields and only
//! becomes a catalog `Rule` when it parses, so a half-typed value never reaches the database.

use super::dates::date_span;
use unlight_catalog::{CollectionId, Comparison, Flag, Label, MatchMode, Rule, SmartRules, TextOp};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleField {
    Rating,
    Flag,
    Label,
    Edited,
    Camera,
    Lens,
    FileName,
    Keyword,
    Folder,
    CaptureDate,
    Iso,
    Aperture,
    FocalLength,
}

/// What kind of value a field takes, which decides the controls it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Number,
    Text,
    Choice(&'static [&'static str]),
    Path,
    Date,
}

impl RuleField {
    pub const ALL: [Self; 13] = [
        Self::Rating,
        Self::Flag,
        Self::Label,
        Self::Edited,
        Self::Camera,
        Self::Lens,
        Self::FileName,
        Self::Keyword,
        Self::Folder,
        Self::CaptureDate,
        Self::Iso,
        Self::Aperture,
        Self::FocalLength,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Rating => "Rating",
            Self::Flag => "Flag",
            Self::Label => "Label",
            Self::Edited => "Edited",
            Self::Camera => "Camera",
            Self::Lens => "Lens",
            Self::FileName => "File Name",
            Self::Keyword => "Keyword",
            Self::Folder => "Folder",
            Self::CaptureDate => "Capture Date",
            Self::Iso => "ISO",
            Self::Aperture => "Aperture",
            Self::FocalLength => "Focal Length",
        }
    }

    pub fn value_kind(self) -> ValueKind {
        match self {
            Self::Rating | Self::Iso | Self::Aperture | Self::FocalLength => ValueKind::Number,
            Self::Flag => ValueKind::Choice(&["Pick", "Rejected", "Unflagged"]),
            Self::Label => ValueKind::Choice(&["Red", "Yellow", "Green", "Blue", "Purple", "None"]),
            Self::Edited => ValueKind::Choice(&["Yes", "No"]),
            Self::Camera | Self::Lens | Self::FileName | Self::Keyword => ValueKind::Text,
            Self::Folder => ValueKind::Path,
            Self::CaptureDate => ValueKind::Date,
        }
    }
}

pub const COMPARISONS: [(Comparison, &str); 6] = [
    (Comparison::Equal, "is"),
    (Comparison::NotEqual, "is not"),
    (Comparison::GreaterOrEqual, "at least"),
    (Comparison::LessOrEqual, "at most"),
    (Comparison::Greater, "above"),
    (Comparison::Less, "below"),
];

pub const TEXT_OPS: [(TextOp, &str); 6] = [
    (TextOp::Contains, "contains"),
    (TextOp::Excludes, "doesn't contain"),
    (TextOp::Is, "is"),
    (TextOp::IsNot, "is not"),
    (TextOp::StartsWith, "starts with"),
    (TextOp::EndsWith, "ends with"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct RuleDraft {
    pub field: RuleField,
    pub comparison: Comparison,
    pub text_op: TextOp,
    pub value: String,
}

impl Default for RuleDraft {
    fn default() -> Self {
        Self {
            field: RuleField::Rating,
            comparison: Comparison::GreaterOrEqual,
            text_op: TextOp::Contains,
            value: "3".into(),
        }
    }
}

fn flag_named(name: &str) -> Option<Flag> {
    match name {
        "Pick" => Some(Flag::Pick),
        "Rejected" => Some(Flag::Reject),
        "Unflagged" => Some(Flag::Unflagged),
        _ => None,
    }
}

fn label_named(name: &str) -> Option<Label> {
    match name {
        "Red" => Some(Label::Red),
        "Yellow" => Some(Label::Yellow),
        "Green" => Some(Label::Green),
        "Blue" => Some(Label::Blue),
        "Purple" => Some(Label::Purple),
        "None" => Some(Label::None),
        _ => None,
    }
}

impl RuleDraft {
    fn number(&self) -> Option<f64> {
        self.value
            .trim()
            .parse()
            .ok()
            .filter(|value: &f64| value.is_finite())
    }

    fn text(&self) -> Option<String> {
        let text = self.value.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// The catalog rule, or `None` while the value doesn't parse.
    pub fn to_rule(&self) -> Option<Rule> {
        let (op, text_op) = (self.comparison, self.text_op);
        Some(match self.field {
            RuleField::Rating => Rule::Rating {
                op,
                value: self.number().filter(|value| (0.0..=5.0).contains(value))? as u8,
            },
            RuleField::Flag => Rule::Flag {
                value: flag_named(&self.value)?,
            },
            RuleField::Label => Rule::Label {
                value: label_named(&self.value)?,
            },
            RuleField::Edited => Rule::Edited {
                value: self.value == "Yes",
            },
            RuleField::Camera => Rule::Camera {
                op: text_op,
                value: self.text()?,
            },
            RuleField::Lens => Rule::Lens {
                op: text_op,
                value: self.text()?,
            },
            RuleField::FileName => Rule::FileName {
                op: text_op,
                value: self.text()?,
            },
            RuleField::Keyword => Rule::Keyword {
                op: text_op,
                value: self.text()?,
            },
            RuleField::Folder => Rule::Folder { path: self.text()? },
            RuleField::CaptureDate => Rule::CaptureTime {
                op,
                value: date_span(&self.value)?.0,
            },
            RuleField::Iso => Rule::Iso {
                op,
                value: self.number()?,
            },
            RuleField::Aperture => Rule::Aperture {
                op,
                value: self.number()?,
            },
            RuleField::FocalLength => Rule::FocalLength {
                op,
                value: self.number()?,
            },
        })
    }

    /// Rules this editor has no row for, such as nested groups, stay as they are.
    pub fn from_rule(rule: &Rule) -> Option<Self> {
        let base = Self::default();
        let compared = |field, comparison, value: String| Self {
            field,
            comparison,
            value,
            ..Self::default()
        };
        let texted = |field, text_op, value: &str| Self {
            field,
            text_op,
            value: value.to_owned(),
            ..Self::default()
        };
        Some(match rule {
            Rule::Rating { op, value } => compared(RuleField::Rating, *op, value.to_string()),
            Rule::Flag { value } => Self {
                field: RuleField::Flag,
                value: match value {
                    Flag::Pick => "Pick",
                    Flag::Reject => "Rejected",
                    Flag::Unflagged => "Unflagged",
                }
                .into(),
                ..base
            },
            Rule::Label { value } => Self {
                field: RuleField::Label,
                value: super::library::label_name(*value).into(),
                ..base
            },
            Rule::Edited { value } => Self {
                field: RuleField::Edited,
                value: if *value { "Yes" } else { "No" }.into(),
                ..base
            },
            Rule::Camera { op, value } => texted(RuleField::Camera, *op, value),
            Rule::Lens { op, value } => texted(RuleField::Lens, *op, value),
            Rule::FileName { op, value } => texted(RuleField::FileName, *op, value),
            Rule::Keyword { op, value } => texted(RuleField::Keyword, *op, value),
            Rule::Folder { path } => Self {
                field: RuleField::Folder,
                value: path.clone(),
                ..base
            },
            Rule::Iso { op, value } => compared(RuleField::Iso, *op, value.to_string()),
            Rule::Aperture { op, value } => compared(RuleField::Aperture, *op, value.to_string()),
            Rule::FocalLength { op, value } => compared(RuleField::FocalLength, *op, value.to_string()),
            _ => return None,
        })
    }

    /// Picks a new field, with a value that suits it.
    pub fn set_field(&mut self, field: RuleField) {
        self.field = field;
        self.value = match field.value_kind() {
            ValueKind::Choice(options) => options[0].to_owned(),
            ValueKind::Number => "0".to_owned(),
            ValueKind::Text | ValueKind::Path | ValueKind::Date => String::new(),
        };
    }
}

/// The whole dialog's state.
#[derive(Clone, Debug, PartialEq)]
pub struct SmartEditor {
    pub existing: Option<CollectionId>,
    pub name: String,
    pub mode: MatchMode,
    pub drafts: Vec<RuleDraft>,
    /// Rules with no row in the editor, kept when saving.
    kept: Vec<Rule>,
}

impl Default for SmartEditor {
    fn default() -> Self {
        Self {
            existing: None,
            name: String::new(),
            mode: MatchMode::All,
            drafts: vec![RuleDraft::default()],
            kept: Vec::new(),
        }
    }
}

impl SmartEditor {
    pub fn editing(id: CollectionId, name: &str, rules: &SmartRules) -> Self {
        let (drafts, kept): (Vec<_>, Vec<_>) = rules
            .rules
            .iter()
            .map(|rule| RuleDraft::from_rule(rule).ok_or_else(|| rule.clone()))
            .partition(Result::is_ok);
        Self {
            existing: Some(id),
            name: name.to_owned(),
            mode: rules.mode,
            drafts: drafts.into_iter().flatten().collect(),
            kept: kept.into_iter().filter_map(Result::err).collect(),
        }
    }

    /// The rules to save, or `None` while something is unfinished.
    pub fn rules(&self) -> Option<SmartRules> {
        if self.name.trim().is_empty() {
            return None;
        }
        let mut rules: Vec<Rule> = self
            .drafts
            .iter()
            .map(RuleDraft::to_rule)
            .collect::<Option<_>>()?;
        rules.extend(self.kept.iter().cloned());
        Some(SmartRules {
            mode: self.mode,
            rules,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(field: RuleField, value: &str) -> RuleDraft {
        let mut draft = RuleDraft::default();
        draft.set_field(field);
        draft.value = value.to_owned();
        draft
    }

    #[test]
    fn every_field_turns_into_a_rule_and_back() {
        let cases = [
            (RuleField::Rating, "4"),
            (RuleField::Flag, "Pick"),
            (RuleField::Label, "Green"),
            (RuleField::Edited, "Yes"),
            (RuleField::Camera, "Sony"),
            (RuleField::Lens, "FE"),
            (RuleField::FileName, "DSC"),
            (RuleField::Keyword, "cats"),
            (RuleField::Folder, "/photos"),
            (RuleField::Iso, "800"),
            (RuleField::Aperture, "2.8"),
            (RuleField::FocalLength, "35"),
        ];
        for (field, value) in cases {
            let rule = draft(field, value).to_rule().unwrap();
            assert_eq!(
                RuleDraft::from_rule(&rule).unwrap().to_rule().unwrap(),
                rule,
                "{field:?}"
            );
        }
    }

    #[test]
    fn unfinished_values_make_no_rule() {
        assert_eq!(draft(RuleField::Rating, "seven").to_rule(), None);
        assert_eq!(draft(RuleField::Rating, "9").to_rule(), None);
        assert_eq!(draft(RuleField::Camera, "  ").to_rule(), None);
        assert_eq!(draft(RuleField::CaptureDate, "soon").to_rule(), None);
    }

    #[test]
    fn a_new_field_gets_a_sensible_value() {
        let mut rule = RuleDraft::default();
        rule.set_field(RuleField::Flag);
        assert_eq!(rule.value, "Pick");
        assert!(rule.to_rule().is_some());
    }

    #[test]
    fn saving_needs_a_name_and_finished_rules() {
        let mut editor = SmartEditor::default();
        assert_eq!(editor.rules(), None);
        editor.name = "Best".into();
        assert_eq!(editor.rules().map(|rules| rules.rules.len()), Some(1));
        editor.drafts.push(draft(RuleField::Camera, ""));
        assert_eq!(editor.rules(), None);
    }

    #[test]
    fn rules_the_editor_cant_show_survive_an_edit() {
        let nested = Rule::Group(SmartRules::default());
        let rules = SmartRules {
            mode: MatchMode::Any,
            rules: vec![Rule::Edited { value: true }, nested.clone()],
        };
        let editor = SmartEditor::editing(CollectionId(1), "Mixed", &rules);
        assert_eq!(editor.drafts.len(), 1);
        assert_eq!(editor.rules().unwrap().rules[1], nested);
    }
}
