//! Sorting — closed enum of orderings generated from the schema.
//!
//! Clients never send column names. They send enum values like `BLOCK_NUMBER_DESC`
//! that we generated from the schema IR, and which we map back to a column. An
//! unrecognised value is rejected by GraphQL enum validation before it reaches us.

use serde::{Deserialize, Serialize};

use crate::filter::Field;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }

    /// The direction that walks the opposite way — needed for backward
    /// pagination, which runs the query reversed and then un-reverses the page.
    pub fn flipped(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// One component of an ORDER BY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    pub field: Field,
    pub direction: SortDirection,
}

impl SortKey {
    pub fn new(field: Field, direction: SortDirection) -> Self {
        Self { field, direction }
    }

    /// The GraphQL enum value for this ordering, e.g. `BLOCK_NUMBER_DESC`.
    ///
    /// Built from the *column* name so it matches SubQuery's convention
    /// (PostGraphile derives enum values from column names, not field names).
    pub fn enum_value(&self) -> String {
        format!(
            "{}_{}",
            self.field.column().to_uppercase(),
            self.direction.suffix()
        )
    }
}

/// A complete, always-deterministic ordering.
///
/// # The tiebreaker is not optional
///
/// `ORDER BY block_number DESC` over rows sharing a block number has no defined
/// row order, so two pages of a paginated read can overlap or skip rows. Every
/// `Ordering` therefore ends with `id`, appended automatically by [`Ordering::new`].
/// This is also what makes cursors work: the cursor encodes exactly these keys,
/// and a non-unique ordering would make it ambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ordering {
    keys: Vec<SortKey>,
}

impl Ordering {
    /// Build an ordering, appending the `id` tiebreaker if not already present.
    ///
    /// `id_field` is the entity's primary key as resolved from the schema IR.
    pub fn new(mut keys: Vec<SortKey>, id_field: Field) -> Self {
        let has_id = keys.iter().any(|k| k.field.column() == id_field.column());
        if !has_id {
            // Direction of the tiebreaker follows the last user key so that
            // reversing the whole ordering stays consistent.
            let dir = keys.last().map_or(SortDirection::Asc, |k| k.direction);
            keys.push(SortKey::new(id_field, dir));
        }
        Self { keys }
    }

    /// Default ordering when the client asks for none: by id ascending.
    pub fn default_for(id_field: Field) -> Self {
        Self::new(vec![], id_field)
    }

    pub fn keys(&self) -> &[SortKey] {
        &self.keys
    }

    /// Reverse every key. Used for backward (`last`/`before`) pagination.
    pub fn reversed(&self) -> Self {
        Self {
            keys: self
                .keys
                .iter()
                .map(|k| SortKey::new(k.field.clone(), k.direction.flipped()))
                .collect(),
        }
    }

    /// The `ORDER BY` clause body, unqualified.
    ///
    /// Prefer [`Ordering::to_sql_with_alias`] when the SELECT list aliases
    /// columns — see the hazard documented there.
    pub fn to_sql(&self) -> String {
        self.to_sql_with_alias(None)
    }

    /// The `ORDER BY` clause body, optionally qualified with a table alias.
    ///
    /// # Why the alias matters
    ///
    /// SQL resolves `ORDER BY <name>` against **output column names first**, and
    /// only then against input columns. Our SELECT list projects
    /// `"value"::text AS "value"`, so an unqualified `ORDER BY "value"` binds to
    /// the *text* output column and sorts `9000` above `115792…` —
    /// lexicographically, not numerically.
    ///
    /// Qualifying as `e."value"` forces resolution to the input column, where
    /// the type is still `numeric`. This also keeps ordering consistent with the
    /// keyset predicate, which lives in `WHERE` and therefore never sees output
    /// aliases at all — without the alias the two silently disagree, which is
    /// the worst version of this bug.
    ///
    /// Column names come from the IR; directions are fixed strings — no client
    /// text reaches this output.
    pub fn to_sql_with_alias(&self, alias: Option<&str>) -> String {
        let prefix = alias.map(|a| format!("{a}.")).unwrap_or_default();
        self.keys
            .iter()
            .map(|k| format!("{prefix}\"{}\" {}", k.field.column(), k.direction.sql()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, col: &str) -> Field {
        Field::resolved(name, col)
    }

    fn id() -> Field {
        f("id", "id")
    }

    #[test]
    fn tiebreaker_is_appended_automatically() {
        let o = Ordering::new(
            vec![SortKey::new(
                f("blockNumber", "block_number"),
                SortDirection::Desc,
            )],
            id(),
        );
        assert_eq!(o.to_sql(), r#""block_number" DESC, "id" DESC"#);
    }

    #[test]
    fn tiebreaker_not_duplicated_when_already_sorted_by_id() {
        let o = Ordering::new(vec![SortKey::new(id(), SortDirection::Asc)], id());
        assert_eq!(o.keys().len(), 1);
        assert_eq!(o.to_sql(), r#""id" ASC"#);
    }

    #[test]
    fn default_ordering_is_id_asc() {
        assert_eq!(Ordering::default_for(id()).to_sql(), r#""id" ASC"#);
    }

    #[test]
    fn reversing_flips_every_key() {
        let o = Ordering::new(
            vec![SortKey::new(
                f("blockNumber", "block_number"),
                SortDirection::Desc,
            )],
            id(),
        );
        assert_eq!(o.reversed().to_sql(), r#""block_number" ASC, "id" ASC"#);
    }

    #[test]
    fn qualified_ordering_avoids_output_alias_shadowing() {
        // Regression: an unqualified ORDER BY binds to the `col::text` output
        // alias and sorts numerics as strings.
        let o = Ordering::new(
            vec![SortKey::new(f("value", "value"), SortDirection::Desc)],
            id(),
        );
        assert_eq!(
            o.to_sql_with_alias(Some("e")),
            r#"e."value" DESC, e."id" DESC"#
        );
    }

    #[test]
    fn enum_values_match_subquery_convention() {
        let k = SortKey::new(f("blockNumber", "block_number"), SortDirection::Desc);
        assert_eq!(k.enum_value(), "BLOCK_NUMBER_DESC");
    }
}
