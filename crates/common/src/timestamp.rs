//! A point in time.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// An instant, serialized as RFC 3339 in UTC, e.g. `2026-11-11T10:00:00.123Z` (D73).
///
/// Precision is milliseconds, so a value survives storage as Unix milliseconds unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    pub fn now() -> Self {
        let millis = jiff::Timestamp::now().as_millisecond();
        Self(jiff::Timestamp::from_millisecond(millis).expect("current time is in range"))
    }

    pub fn from_unix_millis(millis: i64) -> Result<Self> {
        jiff::Timestamp::from_millisecond(millis)
            .map(Self)
            .map_err(|e| Error::Internal(e.to_string()))
    }

    pub fn as_unix_millis(self) -> i64 {
        self.0.as_millisecond()
    }

    pub fn as_jiff(self) -> jiff::Timestamp {
        self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_rfc3339() {
        let at = Timestamp::from_unix_millis(1_794_138_926_400).unwrap();
        assert_eq!(
            serde_json::to_string(&at).unwrap(),
            "\"2026-11-08T11:55:26.4Z\""
        );
    }

    #[test]
    fn now_survives_storage() {
        let now = Timestamp::now();
        assert_eq!(
            Timestamp::from_unix_millis(now.as_unix_millis()).unwrap(),
            now
        );
    }
}
