//! The address of an agent, operator or adapter.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

const MAX_LEN: usize = 64;

/// A validated alias, stored without the leading `@` and displayed with it.
///
/// Agents, operators and adapters share one namespace (D3).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Alias(String);

#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "invalid alias {0:?}: expected 1-{MAX_LEN} characters of a-z, 0-9 and '-', starting with a letter or digit"
)]
pub struct InvalidAlias(String);

impl Alias {
    /// The alias without the leading `@`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Alias {
    type Err = InvalidAlias;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let name = s.strip_prefix('@').unwrap_or(s);
        let first_ok = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let rest_ok = name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');

        if first_ok && rest_ok && name.len() <= MAX_LEN {
            Ok(Self(name.to_owned()))
        } else {
            Err(InvalidAlias(s.to_owned()))
        }
    }
}

impl TryFrom<String> for Alias {
    type Error = InvalidAlias;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Alias> for String {
    fn from(alias: Alias) -> Self {
        alias.0
    }
}

impl fmt::Display for Alias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_with_and_without_at() {
        let a: Alias = "@coder".parse().unwrap();
        let b: Alias = "coder".parse().unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "coder");
        assert_eq!(a.to_string(), "@coder");
    }

    #[test]
    fn rejects_invalid() {
        let too_long = "a".repeat(MAX_LEN + 1);
        for bad in [
            "",
            "@",
            "-coder",
            "Coder",
            "co der",
            "@@coder",
            "кодер",
            &too_long,
        ] {
            assert!(bad.parse::<Alias>().is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn deserialization_validates() {
        assert!(serde_json::from_str::<Alias>("\"Bad Alias\"").is_err());
        let alias: Alias = serde_json::from_str("\"coder\"").unwrap();
        assert_eq!(serde_json::to_string(&alias).unwrap(), "\"coder\"");
    }
}
