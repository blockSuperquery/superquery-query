//! Custom scalars exposed in the generated schema.
//!
//! `async-graphql`'s dynamic schema needs each non-builtin scalar registered by
//! name with a description; serialization happens in the resolvers, which hand
//! back already-shaped JSON (see `superquery-postgres::row`).
//!
//! The descriptions matter more than usual here — they are what tells a dApp
//! developer, in GraphiQL, that `BigInt` arrives as a string.

use async_graphql::dynamic::Scalar;

/// Names of the scalars this service defines beyond the GraphQL builtins.
pub const CUSTOM_SCALARS: &[(&str, &str)] = &[
    (
        "BigInt",
        "An arbitrary-precision integer, serialized as a **string**. \
         Blockchain values routinely exceed 2^53, which is the largest integer a \
         JSON number can represent exactly, so they are never sent as numbers.",
    ),
    (
        "BigDecimal",
        "An arbitrary-precision decimal, serialized as a **string** for the same \
         reason as `BigInt`.",
    ),
    (
        "Bytes",
        "A byte array as a `0x`-prefixed, lower-case hex string.",
    ),
    ("Date", "A timestamp in RFC 3339 / ISO 8601 form."),
    (
        "JSON",
        "An arbitrary JSON value, stored in a `jsonb` column.",
    ),
    (
        "Cursor",
        "An opaque pagination cursor. Pass it back unchanged as `after`/`before`; \
         its contents are an implementation detail and may change between releases.",
    ),
];

/// Build the scalar type definitions for registration.
pub fn scalar_types() -> Vec<Scalar> {
    CUSTOM_SCALARS
        .iter()
        .map(|(name, description)| Scalar::new(*name).description(*description))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_custom_scalar_is_registered() {
        // The scalar map in query-core names these; if one is added there
        // without a definition here, schema construction fails at startup.
        let names: Vec<_> = CUSTOM_SCALARS.iter().map(|(n, _)| *n).collect();
        for expected in ["BigInt", "BigDecimal", "Bytes", "Date", "JSON", "Cursor"] {
            assert!(names.contains(&expected), "missing scalar {expected}");
        }
    }

    #[test]
    fn builds_without_panicking() {
        assert_eq!(scalar_types().len(), CUSTOM_SCALARS.len());
    }
}
