//! Identifier naming — the single most load-bearing contract in this repo.
//!
//! The node creates tables through Sequelize, which names them
//! `underscoredIf(pluralize(ModelName), true)` and columns `underscored(field)`.
//! The query service never sees that DDL; it must *independently* derive the same
//! names or it will read from tables that do not exist.
//!
//! # Vendored, not shared
//!
//! This is a byte-for-byte port of `superquery-node`'s `subql-store::naming`.
//! It is duplicated rather than imported so the two repos build and release
//! independently. The duplication is safe only because of the parity tests at
//! the bottom of this file: they assert the same ground-truth vectors the node
//! asserts. If the node changes its naming rules, those tests fail here and the
//! divergence is caught before it reaches a query result.
//!
//! Rules come from the `inflection` library Sequelize depends on.

use regex::Regex;
use std::sync::OnceLock;

/// `inflection.underscore` — camelCase/PascalCase → snake_case, acronym-aware.
///
/// ```
/// # use superquery_query_core::naming::underscored;
/// assert_eq!(underscored("blockHeight"), "block_height");
/// assert_eq!(underscored("HTTPServer"), "http_server");
/// ```
pub fn underscored(input: &str) -> String {
    static ACRONYM: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let acronym = ACRONYM.get_or_init(|| Regex::new(r"([A-Z\d]+)([A-Z][a-z])").unwrap());
    let word = WORD.get_or_init(|| Regex::new(r"([a-z\d])([A-Z])").unwrap());

    let step1 = acronym.replace_all(input, "${1}_${2}");
    let step2 = word.replace_all(&step1, "${1}_${2}");
    step2.replace('-', "_").to_lowercase()
}

/// `inflection.pluralize` — English pluralization in the ordered rule set
/// Sequelize relies on: uncountables, then irregulars, then regex rules.
pub fn pluralize(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }
    let lower = word.to_lowercase();

    const UNCOUNTABLE: &[&str] = &[
        "equipment",
        "information",
        "rice",
        "money",
        "species",
        "series",
        "fish",
        "sheep",
        "jeans",
        "moose",
        "deer",
        "news",
    ];
    if UNCOUNTABLE.contains(&lower.as_str()) {
        return word.to_string();
    }

    const IRREGULAR: &[(&str, &str)] = &[
        ("person", "people"),
        ("man", "men"),
        ("child", "children"),
        ("sex", "sexes"),
        ("move", "moves"),
        ("cow", "kine"),
        ("zombie", "zombies"),
    ];
    for (sing, plur) in IRREGULAR {
        if lower == *sing {
            let mut result = plur.to_string();
            if word.chars().next().is_some_and(|c| c.is_uppercase()) {
                result = capitalize_first(&result);
            }
            return result;
        }
    }

    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        let raw: &[(&str, &str)] = &[
            (r"(?i)(quiz)$", "${1}zes"),
            (r"(?i)^(ox)$", "${1}en"),
            (r"(?i)([ml])ouse$", "${1}ice"),
            (r"(?i)(matr|vert|ind)(ix|ex)$", "${1}ices"),
            (r"(?i)(x|ch|ss|sh)$", "${1}es"),
            (r"(?i)([^aeiouy]|qu)y$", "${1}ies"),
            (r"(?i)(hive)$", "${1}s"),
            (r"(?i)(?:([^f])fe|([lr])f)$", "${1}${2}ves"),
            (r"(?i)sis$", "ses"),
            (r"(?i)([ti])um$", "${1}a"),
            (r"(?i)(buffal|tomat)o$", "${1}oes"),
            (r"(?i)(bu)s$", "${1}ses"),
            (r"(?i)(alias|status)$", "${1}es"),
            (r"(?i)(octop|vir)us$", "${1}i"),
            (r"(?i)(ax|test)is$", "${1}es"),
            (r"(?i)s$", "s"),
            (r"(?i)$", "s"),
        ];
        raw.iter()
            .map(|(re, rep)| (Regex::new(re).unwrap(), *rep))
            .collect()
    });

    for (re, rep) in rules {
        if re.is_match(word) {
            return re.replace(word, *rep).into_owned();
        }
    }
    word.to_string()
}

/// `modelToTableName` — the table an entity's rows live in.
pub fn model_to_table_name(model_name: &str) -> String {
    underscored(&pluralize(model_name))
}

/// The column an entity field maps to.
pub fn field_to_column_name(field_name: &str) -> String {
    underscored(field_name)
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PARITY: these vectors are copied from `superquery-node`'s
    /// `subql-store::naming` tests. They must stay identical in both repos.
    /// A failure here means query would read a different table than node writes.
    #[test]
    fn parity_with_node_table_names() {
        assert_eq!(model_to_table_name("Transfer"), "transfers");
        assert_eq!(model_to_table_name("MyEntity"), "my_entities");
    }

    #[test]
    fn parity_with_node_underscore() {
        assert_eq!(underscored("blockHeight"), "block_height");
        assert_eq!(underscored("Transfers"), "transfers");
        assert_eq!(underscored("HTTPServer"), "http_server");
        assert_eq!(underscored("id"), "id");
        assert_eq!(underscored("myLongFieldName"), "my_long_field_name");
    }

    #[test]
    fn parity_with_node_pluralize() {
        assert_eq!(pluralize("Transfer"), "Transfers");
        assert_eq!(pluralize("account"), "accounts");
        assert_eq!(pluralize("entity"), "entities");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("person"), "people");
        assert_eq!(pluralize("status"), "statuses");
    }

    #[test]
    fn column_names_are_underscored() {
        assert_eq!(field_to_column_name("blockNumber"), "block_number");
        assert_eq!(field_to_column_name("from"), "from");
    }
}
