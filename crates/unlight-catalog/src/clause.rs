//! Parameterised SQL fragments shared by library filters and smart collection rules.

use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl Comparison {
    fn sql(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::NotEqual => "<>",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Greater => ">",
            Self::GreaterOrEqual => ">=",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOp {
    Is,
    IsNot,
    Contains,
    Excludes,
    StartsWith,
    EndsWith,
}

/// A boolean SQL expression. Its `?` placeholders line up with `params` in order, so clauses
/// can be joined freely.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Clause {
    pub sql: String,
    pub params: Vec<Value>,
}

impl Clause {
    pub fn new(sql: impl Into<String>, params: impl IntoIterator<Item = Value>) -> Self {
        Self { sql: sql.into(), params: params.into_iter().collect() }
    }

    pub fn join(clauses: Vec<Clause>, separator: &str) -> Self {
        let sql: Vec<_> = clauses.iter().map(|clause| clause.sql.as_str()).collect();
        let sql = format!("({})", sql.join(separator));
        Self::new(sql, clauses.into_iter().flat_map(|clause| clause.params))
    }

    pub fn negate(self) -> Self {
        Self { sql: format!("NOT {}", self.sql), params: self.params }
    }

    pub fn compare(column: &str, comparison: Comparison, value: impl Into<Value>) -> Self {
        Self::new(format!("{column} {} ?", comparison.sql()), [value.into()])
    }

    /// Case-insensitive text match on a column of the main query.
    pub fn text(column: &str, op: TextOp, value: &str) -> Self {
        let like = |pattern: String, keyword: &str| {
            Self::new(format!(r"{column} {keyword} ? ESCAPE '\'"), [Value::Text(pattern)])
        };
        let escaped = escape_like(value);
        match op {
            TextOp::Is => {
                Self::new(format!("{column} = ? COLLATE NOCASE"), [Value::Text(value.to_owned())])
            }
            TextOp::IsNot => {
                Self::new(format!("{column} <> ? COLLATE NOCASE"), [Value::Text(value.to_owned())])
            }
            TextOp::Contains => like(format!("%{escaped}%"), "LIKE"),
            TextOp::Excludes => like(format!("%{escaped}%"), "NOT LIKE"),
            TextOp::StartsWith => like(format!("{escaped}%"), "LIKE"),
            TextOp::EndsWith => like(format!("%{escaped}"), "LIKE"),
        }
    }

    /// Tests `column` against the ids a text match selects from a related table. Negative
    /// operators exclude the matches, so a row with no related rows at all still passes.
    pub fn related_text(
        column: &str,
        subquery: &str,
        text_column: &str,
        op: TextOp,
        value: &str,
    ) -> Self {
        let (positive, negated) = match op {
            TextOp::IsNot => (TextOp::Is, true),
            TextOp::Excludes => (TextOp::Contains, true),
            other => (other, false),
        };
        let inner = Self::text(text_column, positive, value);
        let sql = format!("{column} IN ({subquery} WHERE {})", inner.sql);
        let clause = Self::new(sql, inner.params);
        if negated { clause.negate() } else { clause }
    }
}

fn escape_like(text: &str) -> String {
    text.replace('\\', r"\\").replace('%', r"\%").replace('_', r"\_")
}

/// The half-open range of paths strictly below `path`, which uses the path index where a
/// `LIKE` could not. `'0'` is the character after `'/'`.
pub(crate) fn subtree_range(path: &str) -> (String, String) {
    let base = path.trim_end_matches('/');
    (format!("{base}/"), format!("{base}0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_wildcards_in_user_text_are_literal() {
        assert_eq!(escape_like(r"50%_off\now"), r"50\%\_off\\now");
    }

    #[test]
    fn joined_clauses_keep_their_parameters_in_order() {
        let joined = Clause::join(
            vec![
                Clause::compare("a", Comparison::Greater, 1i64),
                Clause::compare("b", Comparison::Equal, 2i64),
            ],
            " OR ",
        );
        assert_eq!(joined.sql, "(a > ? OR b = ?)");
        assert_eq!(joined.params, [Value::Integer(1), Value::Integer(2)]);
    }

    #[test]
    fn subtree_range_excludes_sibling_folders_sharing_a_prefix() {
        let (low, high) = subtree_range("/photos/2024");
        assert!(low.as_str() <= "/photos/2024/a" && "/photos/2024/a" < high.as_str());
        assert!("/photos/2024-backup" < low.as_str());
        assert!("/photos/2024" < low.as_str());
    }
}
