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

/// Limits applied to every incoming query.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Maximum nesting depth. Relations make depth the main amplification lever:
    /// `account { transfers { account { transfers { … } } } }`.
    pub max_depth: Option<usize>,
    /// Maximum complexity score, roughly "number of fields that could resolve".
    pub max_complexity: Option<usize>,
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
            max_page_size: u32::MAX,
            default_page_size: superquery_query_core::DEFAULT_PAGE_SIZE,
            timeout: std::time::Duration::from_secs(300),
        }
    }

    /// Whether any protection is disabled — drives the startup warning.
    pub fn is_unrestricted(&self) -> bool {
        self.max_depth.is_none() && self.max_complexity.is_none()
    }
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
}
