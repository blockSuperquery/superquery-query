//! Filter IR — GraphQL-independent, SQL-independent.
//!
//! # Why an IR at all
//!
//! The security property this repo must guarantee is that *nothing a client
//! sends can become SQL syntax*. Field names and operators are the dangerous
//! part — values are easy, they bind as parameters.
//!
//! Splitting on the IR enforces that structurally:
//!
//! ```text
//!   client JSON ──▶ FilterExpr ──▶ parameterized SQL
//!                ▲              ▲
//!                │              └─ postgres crate: columns come from the IR,
//!                │                 values become $1, $2, … always
//!                └─ graphql crate: field names resolved against the schema IR;
//!                   an unknown name is an error, never a passthrough
//! ```
//!
//! By the time a `FilterExpr` exists, every `Field` in it was looked up in the
//! schema IR, so the column name is one we generated. A malicious field name
//! fails at construction, not at execution.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A field reference that has already been resolved against the schema IR.
///
/// The only constructor is [`Field::resolved`], which callers reach after a
/// successful schema lookup. There is deliberately no `From<String>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// GraphQL field name, for error messages.
    name: String,
    /// Postgres column name, derived by us.
    column: String,
}

impl Field {
    /// Build a reference from a schema lookup that already succeeded.
    pub fn resolved(name: impl Into<String>, column: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            column: column.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn column(&self) -> &str {
        &self.column
    }
}

/// Comparison operators. Kept as a closed enum so the SQL compiler's match is
/// exhaustive — adding an operator forces updating the compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl CmpOp {
    /// The GraphQL input field name this operator is exposed as, matching
    /// SubQuery's PostGraphile-derived naming so existing dApp queries port over.
    pub fn graphql_name(self) -> &'static str {
        match self {
            Self::Eq => "equalTo",
            Self::Ne => "notEqualTo",
            Self::Gt => "greaterThan",
            Self::Gte => "greaterThanOrEqualTo",
            Self::Lt => "lessThan",
            Self::Lte => "lessThanOrEqualTo",
        }
    }

    /// The SQL operator. Note this is a fixed `&'static str` chosen by a match on
    /// our own enum — it can never be client-supplied text.
    pub fn sql(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
        }
    }

    /// Operators available on every filterable type.
    pub const EQUALITY: [CmpOp; 2] = [CmpOp::Eq, CmpOp::Ne];
    /// Operators available only on ordered types.
    pub const ORDERING: [CmpOp; 4] = [CmpOp::Gt, CmpOp::Gte, CmpOp::Lt, CmpOp::Lte];
}

/// A filter expression tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FilterExpr {
    /// `field <op> value`
    Compare {
        field: Field,
        op: CmpOp,
        value: Value,
    },
    /// `field IN (values…)`
    In {
        field: Field,
        values: Vec<Value>,
    },
    /// `field IS NULL` / `IS NOT NULL`
    IsNull {
        field: Field,
        negated: bool,
    },
    And(Vec<FilterExpr>),
    Or(Vec<FilterExpr>),
    Not(Box<FilterExpr>),
}

impl FilterExpr {
    /// Combine into a single expression, flattening the common cases so the
    /// generated SQL does not accumulate `(((…)))` for every nesting level.
    pub fn all(mut parts: Vec<FilterExpr>) -> Option<FilterExpr> {
        match parts.len() {
            0 => None,
            1 => parts.pop(),
            _ => Some(FilterExpr::And(parts)),
        }
    }

    /// Depth of the expression tree. Used by the query-limit layer to reject
    /// pathologically nested filters before they reach the planner.
    pub fn depth(&self) -> usize {
        match self {
            Self::Compare { .. } | Self::In { .. } | Self::IsNull { .. } => 1,
            Self::Not(inner) => 1 + inner.depth(),
            Self::And(parts) | Self::Or(parts) => {
                1 + parts.iter().map(Self::depth).max().unwrap_or(0)
            }
        }
    }

    /// Every field referenced anywhere in the tree — lets the SQL layer know
    /// which columns need to be join-reachable.
    pub fn referenced_fields(&self) -> Vec<&Field> {
        let mut out = Vec::new();
        self.collect_fields(&mut out);
        out
    }

    fn collect_fields<'a>(&'a self, out: &mut Vec<&'a Field>) {
        match self {
            Self::Compare { field, .. } | Self::In { field, .. } | Self::IsNull { field, .. } => {
                out.push(field)
            }
            Self::Not(inner) => inner.collect_fields(out),
            Self::And(parts) | Self::Or(parts) => {
                for p in parts {
                    p.collect_fields(out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(name: &str, col: &str) -> Field {
        Field::resolved(name, col)
    }

    #[test]
    fn depth_counts_nesting() {
        let leaf = FilterExpr::Compare {
            field: f("id", "id"),
            op: CmpOp::Eq,
            value: json!("1"),
        };
        assert_eq!(leaf.depth(), 1);

        let nested = FilterExpr::And(vec![
            leaf.clone(),
            FilterExpr::Or(vec![leaf.clone(), FilterExpr::Not(Box::new(leaf.clone()))]),
        ]);
        // And → Or → Not → Compare
        assert_eq!(nested.depth(), 4);
    }

    #[test]
    fn all_flattens_trivial_cases() {
        assert!(FilterExpr::all(vec![]).is_none());

        let one = FilterExpr::IsNull {
            field: f("data", "data"),
            negated: false,
        };
        // A single filter should not be wrapped in a pointless AND.
        assert_eq!(FilterExpr::all(vec![one.clone()]), Some(one));
    }

    #[test]
    fn referenced_fields_walks_whole_tree() {
        let e = FilterExpr::And(vec![
            FilterExpr::Compare {
                field: f("from", "from"),
                op: CmpOp::Eq,
                value: json!("0xabc"),
            },
            FilterExpr::Or(vec![FilterExpr::In {
                field: f("blockNumber", "block_number"),
                values: vec![json!(1), json!(2)],
            }]),
        ]);
        let names: Vec<_> = e.referenced_fields().iter().map(|f| f.name()).collect();
        assert_eq!(names, ["from", "blockNumber"]);
    }

    #[test]
    fn sql_operators_are_fixed_strings() {
        // Guards the injection property: operators never come from input.
        assert_eq!(CmpOp::Eq.sql(), "=");
        assert_eq!(CmpOp::Ne.sql(), "<>");
        assert_eq!(CmpOp::Gte.sql(), ">=");
    }

    #[test]
    fn graphql_names_match_subquery() {
        assert_eq!(CmpOp::Eq.graphql_name(), "equalTo");
        assert_eq!(CmpOp::Gte.graphql_name(), "greaterThanOrEqualTo");
    }
}
