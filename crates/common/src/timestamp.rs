//! A point in time.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
///
/// A plain integer until something needs calendar formatting (O39).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    pub fn now() -> Self {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(elapsed.as_millis() as i64)
    }

    pub fn from_unix_millis(millis: i64) -> Self {
        Self(millis)
    }

    pub fn as_unix_millis(self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_a_plain_number() {
        let at = Timestamp::from_unix_millis(1_794_138_926_400);
        assert_eq!(serde_json::to_string(&at).unwrap(), "1794138926400");
    }
}
