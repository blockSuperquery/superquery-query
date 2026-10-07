//! Query protection limits.
//!
//! A generated GraphQL API over a relational database is an excellent denial-of-
//! service target: a schema with relations lets a short query describe an
//! enormous amount of work. Upstream SubQuery handles this with PostGraphile
//! plugins (`QueryDepthLimitPlugin`, `QueryAliasLimitPlugin`,
//! `QueryComplexityPlugin`); the Rust equivalents are configured here and
//! applied by `async-graphql`'s own validation phase, which runs *before*
//! execution — so a rejected query costs no database work at all.
//!
//! Values live in server config, not as constants baked into core types, so
//! operators can tune them per deployment.
//!
//! Aliases are the one limit `async-graphql` has no knob for, so they are
//! counted here over the parsed document — also before execution.

use async_graphql::parser::types::{ExecutableDocument, Selection, SelectionSet};

/// Default alias ceiling. Generous for hand-written queries, which rarely use
/// more than a handful; low enough that one request cannot fan a single field
/// out into hundreds of independent collection reads.
pub const DEFAULT_MAX_ALIASES: usize = 50;

/// Limits applied to every incoming query.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Maximum nesting depth. Relations make depth the main amplification lever:
    /// `account { transfers { account { transfers { … } } } }`.
    pub max_depth: Option<usize>,
    /// Maximum complexity score, roughly "number of fields that could resolve".
    pub max_complexity: Option<usize>,
    /// Maximum aliased fields per document. Aliases are how one request asks
    /// for the same expensive field many times over:
    /// `a: transfers(first: 100) { … } b: transfers(first: 100) { … } …`.
    pub max_aliases: Option<usize>,
    /// Rows a single connection may return.
    pub max_page_size: u32,
    /// Page size when the client does not ask for one.
    pub default_page_size: u32,
    /// Wall-clock budget for one request.
    pub timeout: std::time::Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // Depth 10 comfortably fits legitimate nested reads while cutting off
            // the exponential cases.
            max_depth: Some(10),
            max_complexity: Some(1000),
            max_aliases: Some(DEFAULT_MAX_ALIASES),
            max_page_size: superquery_query_core::MAX_PAGE_SIZE,
            default_page_size: superquery_query_core::DEFAULT_PAGE_SIZE,
            // Matches upstream SubQuery's `--query-timeout` default.
            timeout: std::time::Duration::from_millis(10_000),
        }
    }
}

impl Limits {
    /// Remove every limit. Upstream's `--unsafe`.
    ///
    /// Appropriate only for a private deployment behind a trusted gateway; the
    /// server logs a warning at startup when this is on.
    pub fn unsafe_unlimited() -> Self {
        Self {
            max_depth: None,
            max_complexity: None,
            max_aliases: None,
            max_page_size: u32::MAX,
            default_page_size: superquery_query_core::DEFAULT_PAGE_SIZE,
            timeout: std::time::Duration::from_secs(300),
        }
    }

    /// Whether any protection is disabled — drives the startup warning.
    pub fn is_unrestricted(&self) -> bool {
        self.max_depth.is_none() && self.max_complexity.is_none()
    }

    /// Checks that run on the raw document, before `async-graphql` validates
    /// or executes it.
    ///
    /// A document that does not parse passes here on purpose: the executor
    /// reports syntax errors with positions, which is more useful than
    /// anything this layer could say.
    pub fn check_document(&self, query: &str) -> Result<(), String> {
        let Some(max) = self.max_aliases else {
            return Ok(());
        };
        let Ok(doc) = async_graphql::parser::parse_query(query) else {
            return Ok(());
        };
        let aliases = count_aliases(&doc);
        if aliases > max {
            return Err(format!("query uses {aliases} aliases; the limit is {max}"));
        }
        Ok(())
    }
}

/// Every aliased field in a document, across all operations and fragments.
///
/// A fragment is counted once per definition, not once per spread. Repeated
/// spreads are an amplification the complexity limit already sees, since it
/// scores the document after fragments are expanded.
pub fn count_aliases(doc: &ExecutableDocument) -> usize {
    let in_operations: usize = doc
        .operations
        .iter()
        .map(|(_, op)| count_in(&op.node.selection_set.node))
        .sum();
    let in_fragments: usize = doc
        .fragments
        .values()
        .map(|f| count_in(&f.node.selection_set.node))
        .sum();
    in_operations + in_fragments
}

fn count_in(set: &SelectionSet) -> usize {
    set.items
        .iter()
        .map(|item| match &item.node {
            Selection::Field(field) => {
                usize::from(field.node.alias.is_some()) + count_in(&field.node.selection_set.node)
            }
            Selection::InlineFragment(frag) => count_in(&frag.node.selection_set.node),
            Selection::FragmentSpread(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_restrictive() {
        let l = Limits::default();
        assert!(!l.is_unrestricted());
        assert_eq!(l.max_page_size, 100);
        assert_eq!(l.default_page_size, 20);
    }

    #[test]
    fn unsafe_mode_is_detectable() {
        assert!(Limits::unsafe_unlimited().is_unrestricted());
    }

    fn aliases(query: &str) -> usize {
        count_aliases(&async_graphql::parser::parse_query(query).unwrap())
    }

    #[test]
    fn counts_aliases_at_every_level() {
        assert_eq!(aliases("{ transfers { nodes { id } } }"), 0);
        assert_eq!(
            aliases("{ a: transfers { nodes { x: id y: id } } b: transfers { nodes { id } } }"),
            4
        );
    }

    #[test]
    fn counts_aliases_inside_fragments() {
        let q = r#"
            query { ...F  ... on Query { c: _meta { chain } } }
            fragment F on Query { a: transfers { nodes { id } } b: transfers { nodes { id } } }
        "#;
        assert_eq!(aliases(q), 3);
    }

    #[test]
    fn alias_flood_is_rejected_before_execution() {
        let fields: String = (0..60)
            .map(|i| format!("a{i}: transfers(first: 100) {{ nodes {{ id }} }} "))
            .collect();
        let err = Limits::default()
            .check_document(&format!("{{ {fields} }}"))
            .unwrap_err();
        assert!(err.contains("60 aliases"), "{err}");
    }

    #[test]
    fn alias_limit_is_off_in_unsafe_mode() {
        let fields: String = (0..60)
            .map(|i| format!("a{i}: _meta {{ chain }} "))
            .collect();
        assert!(Limits::unsafe_unlimited()
            .check_document(&format!("{{ {fields} }}"))
            .is_ok());
    }

    #[test]
    fn unparseable_documents_are_left_to_the_executor() {
        assert!(Limits::default().check_document("{ not valid").is_ok());
    }
}
