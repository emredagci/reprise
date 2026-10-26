//! Allowed values of record fields (docs/vault-format.md). Frontmatter parsing is in `record`; writing
//! records is in `store`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = UnknownValue;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|v| v.as_str() == s)
                    .ok_or_else(|| UnknownValue { field: stringify!($name), value: s.to_owned() })
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: invalid value {value:?}")]
pub struct UnknownValue {
    pub field: &'static str,
    pub value: String,
}

string_enum!(ItemType {
    Bug => "bug",
    Feature => "feature",
    Idea => "idea",
    Debt => "debt",
});

string_enum!(ItemStatus {
    Inbox => "inbox",
    Planned => "planned",
    Doing => "doing",
    DoneUnverified => "done-unverified",
    Verified => "verified",
    Dropped => "dropped",
});

string_enum!(DecisionStatus {
    Accepted => "accepted",
    Superseded => "superseded",
});

impl ItemStatus {
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Verified | Self::Dropped)
    }

    /// `verified` is only set by `item verify`, never by hand.
    pub fn settable_by_hand(self) -> bool {
        self != Self::Verified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_the_vault_format() {
        let texts = |all: &[ItemStatus]| all.iter().map(|s| s.as_str()).collect::<Vec<_>>();
        assert_eq!(
            texts(ItemStatus::ALL),
            [
                "inbox",
                "planned",
                "doing",
                "done-unverified",
                "verified",
                "dropped"
            ]
        );
        assert_eq!(
            ItemType::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
            ["bug", "feature", "idea", "debt"]
        );
        assert_eq!(
            DecisionStatus::ALL
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>(),
            ["accepted", "superseded"]
        );
    }

    #[test]
    fn parse_round_trip_and_reject() {
        for s in ItemStatus::ALL {
            assert_eq!(s.as_str().parse::<ItemStatus>(), Ok(*s));
        }
        let err = "done".parse::<ItemStatus>().unwrap_err();
        assert_eq!(err.field, "ItemStatus");
        assert_eq!(
            serde_json::to_string(&ItemStatus::DoneUnverified).unwrap(),
            "\"done-unverified\""
        );
    }

    #[test]
    fn closed_and_hand_settable() {
        let closed: Vec<_> = ItemStatus::ALL.iter().filter(|s| s.is_closed()).collect();
        assert_eq!(closed, [&ItemStatus::Verified, &ItemStatus::Dropped]);
        assert!(!ItemStatus::Verified.settable_by_hand());
        assert!(ItemStatus::DoneUnverified.settable_by_hand());
    }
}
