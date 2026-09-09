//! Cursor pagination.
//!
//! # Cursors over offsets
//!
//! `OFFSET 50000` makes Postgres walk and discard 50 000 rows, and it silently
//! skips or repeats rows whenever the underlying data shifts between pages —
//! which, against a live indexer, is constantly. A cursor instead encodes *where
//! the last page stopped* in the ordering's own terms, so the next page is an
//! index seek and stays correct as rows are appended.
//!
//! # Shape
//!
//! A cursor holds one value per [`crate::sort::Ordering`] key, in order. Because
//! every ordering ends with `id`, the tuple is unique, which is what makes the
//! keyset comparison below strict rather than ambiguous.
//!
//! It is base64 of JSON and **opaque by contract**: clients must round-trip it
//! unchanged. Encoding it is not a security boundary — decoded values are
//! validated against the ordering and bound as parameters like any other value.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

/// Default page size when the client asks for none.
pub const DEFAULT_PAGE_SIZE: u32 = 20;
/// Hard ceiling on a single page, regardless of what the client asks for.
pub const MAX_PAGE_SIZE: u32 = 100;

/// An opaque pagination cursor: the ordering-key values of one row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    /// One entry per ordering key, in the same order. `None` = SQL NULL.
    #[serde(rename = "v")]
    pub values: Vec<Option<String>>,
}

impl Cursor {
    pub fn new(values: Vec<Option<String>>) -> Self {
        Self { values }
    }

    /// Encode to the opaque string handed to clients.
    pub fn encode(&self) -> String {
        // Serialization of a Vec<Option<String>> cannot fail.
        let json = serde_json::to_vec(self).expect("cursor is always serializable");
        URL_SAFE_NO_PAD.encode(json)
    }

    /// Decode a client-supplied cursor.
    ///
    /// `expected_len` is the number of ordering keys. A cursor from a query with
    /// a *different* `orderBy` would otherwise be compared against the wrong
    /// columns and silently return nonsense, so the arity is checked here.
    pub fn decode(raw: &str, expected_len: usize) -> CoreResult<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|e| CoreError::InvalidCursor(e.to_string()))?;
        let cursor: Cursor =
            serde_json::from_slice(&bytes).map_err(|e| CoreError::InvalidCursor(e.to_string()))?;
        if cursor.values.len() != expected_len {
            return Err(CoreError::InvalidCursor(format!(
                "cursor has {} key(s) but the current ordering has {expected_len}; \
                 cursors cannot be reused across different orderBy arguments",
                cursor.values.len()
            )));
        }
        Ok(cursor)
    }
}

/// Which direction a page is being read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDirection {
    Forward,
    Backward,
}

/// Validated Relay-style pagination arguments.
#[derive(Debug, Clone)]
pub struct PageRequest {
    pub direction: PageDirection,
    /// Rows to return. Already clamped to `MAX_PAGE_SIZE`.
    pub limit: u32,
    /// Exclusive bound: the row the previous page ended at.
    pub cursor: Option<Cursor>,
}

impl PageRequest {
    /// Resolve raw GraphQL connection arguments into a request.
    ///
    /// `max_page_size` comes from server config so limits are not baked into
    /// core types (the guide's §19 note).
    pub fn resolve(
        first: Option<u32>,
        last: Option<u32>,
        after: Option<&str>,
        before: Option<&str>,
        ordering_len: usize,
        max_page_size: u32,
        default_page_size: u32,
    ) -> CoreResult<Self> {
        if first.is_some() && last.is_some() {
            return Err(CoreError::InvalidValue {
                field: "first/last".into(),
                reason: "pass either `first` or `last`, not both".into(),
            });
        }
        if after.is_some() && before.is_some() {
            return Err(CoreError::InvalidValue {
                field: "after/before".into(),
                reason: "pass either `after` or `before`, not both".into(),
            });
        }

        let backward = last.is_some() || before.is_some();
        let direction = if backward {
            PageDirection::Backward
        } else {
            PageDirection::Forward
        };

        let requested = first.or(last).unwrap_or(default_page_size);
        // Clamp rather than reject: a client asking for 10 000 gets the maximum
        // page plus `hasNextPage: true`, which is more useful than an error.
        let limit = requested.min(max_page_size);

        let raw_cursor = after.or(before);
        let cursor = raw_cursor
            .map(|c| Cursor::decode(c, ordering_len))
            .transpose()?;

        Ok(Self {
            direction,
            limit,
            cursor,
        })
    }

    /// Rows to actually fetch: one extra, to learn whether another page exists
    /// without a second round trip or a `COUNT(*)`.
    pub fn fetch_limit(&self) -> i64 {
        i64::from(self.limit) + 1
    }
}

/// Relay `PageInfo`.
#[derive(Debug, Clone, PartialEq)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
    pub end_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips() {
        let c = Cursor::new(vec![Some("21012345".into()), Some("0xabc".into())]);
        let decoded = Cursor::decode(&c.encode(), 2).unwrap();
        assert_eq!(c, decoded);
    }

    #[test]
    fn cursor_preserves_nulls() {
        let c = Cursor::new(vec![None, Some("x".into())]);
        assert_eq!(Cursor::decode(&c.encode(), 2).unwrap(), c);
    }

    #[test]
    fn cursor_is_url_safe() {
        // Must survive being pasted into a query string unescaped.
        let c = Cursor::new(vec![Some("a/b+c=d".repeat(8))]);
        let enc = c.encode();
        assert!(!enc.contains('/') && !enc.contains('+') && !enc.contains('='));
    }

    #[test]
    fn garbage_cursor_is_a_user_error() {
        assert!(matches!(
            Cursor::decode("!!!not base64!!!", 1),
            Err(CoreError::InvalidCursor(_))
        ));
        assert!(matches!(
            Cursor::decode(&URL_SAFE_NO_PAD.encode("not json"), 1),
            Err(CoreError::InvalidCursor(_))
        ));
    }

    #[test]
    fn cursor_from_a_different_ordering_is_rejected() {
        // Reusing a 2-key cursor under a 1-key ordering would compare the wrong
        // columns; catch it instead of returning wrong rows.
        let c = Cursor::new(vec![Some("1".into()), Some("2".into())]);
        let err = Cursor::decode(&c.encode(), 1).unwrap_err();
        assert!(err.to_string().contains("cannot be reused"));
    }

    #[test]
    fn page_size_is_clamped_not_rejected() {
        let p = PageRequest::resolve(
            Some(10_000),
            None,
            None,
            None,
            1,
            MAX_PAGE_SIZE,
            DEFAULT_PAGE_SIZE,
        )
        .unwrap();
        assert_eq!(p.limit, MAX_PAGE_SIZE);
    }

    #[test]
    fn defaults_apply_when_unspecified() {
        let p = PageRequest::resolve(None, None, None, None, 1, MAX_PAGE_SIZE, DEFAULT_PAGE_SIZE)
            .unwrap();
        assert_eq!(p.limit, DEFAULT_PAGE_SIZE);
        assert_eq!(p.direction, PageDirection::Forward);
        assert!(p.cursor.is_none());
    }

    #[test]
    fn last_or_before_means_backward() {
        let p = PageRequest::resolve(
            None,
            Some(5),
            None,
            None,
            1,
            MAX_PAGE_SIZE,
            DEFAULT_PAGE_SIZE,
        )
        .unwrap();
        assert_eq!(p.direction, PageDirection::Backward);
        assert_eq!(p.limit, 5);
    }

    #[test]
    fn contradictory_arguments_are_rejected() {
        assert!(PageRequest::resolve(Some(1), Some(1), None, None, 1, 100, 20).is_err());
        let c = Cursor::new(vec![Some("1".into())]).encode();
        assert!(PageRequest::resolve(None, None, Some(&c), Some(&c), 1, 100, 20).is_err());
    }

    #[test]
    fn fetch_limit_asks_for_one_extra_row() {
        let p = PageRequest::resolve(Some(10), None, None, None, 1, 100, 20).unwrap();
        assert_eq!(p.fetch_limit(), 11);
    }
}
