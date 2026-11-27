use chrono::{TimeDelta, Utc};
use proptest::prelude::*;
use tayassu_core::{
    AccountId, CooldownEntry, MAX_ACCOUNT_ID_LEN, MAX_REMAINING_PERCENT, ModelBucket,
    RemainingPercent,
};

/// Range of reset offsets exercised around the current instant.
const MAX_RESET_OFFSET_SECONDS: i64 = 86_400;

proptest! {
    #[test]
    fn accepted_account_ids_are_normalized_safe_and_round_trip(raw in ".*") {
        if let Ok(id) = raw.parse::<AccountId>() {
            let normalized = id.as_str();
            prop_assert!(!normalized.is_empty());
            prop_assert!(normalized.len() <= MAX_ACCOUNT_ID_LEN);
            prop_assert!(normalized.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'));
            prop_assert_eq!(normalized.parse::<AccountId>().expect("normalized id"), id);
        }
    }

    #[test]
    fn valid_account_ids_ignore_case_and_surrounding_whitespace(raw in "[A-Za-z0-9_-]{1,64}") {
        let padded = format!("  {raw}  ");
        let id = padded.parse::<AccountId>().expect("valid id");
        prop_assert_eq!(id.as_str(), raw.to_ascii_lowercase());
    }

    #[test]
    fn remaining_percent_accepts_only_the_documented_range(value in any::<u8>()) {
        match RemainingPercent::try_from(value) {
            Ok(percent) => {
                prop_assert!(value <= MAX_REMAINING_PERCENT);
                prop_assert_eq!(percent.value(), value);
                prop_assert_eq!(percent.is_exhausted(), value == 0);
            }
            Err(_) => prop_assert!(value > MAX_REMAINING_PERCENT),
        }
    }

    #[test]
    fn cooldown_is_active_exactly_until_its_reset(offset in -MAX_RESET_OFFSET_SECONDS..=MAX_RESET_OFFSET_SECONDS) {
        let now = Utc::now();
        let cooldown = CooldownEntry {
            bucket: ModelBucket::default(),
            resets_at: now + TimeDelta::seconds(offset),
            reason: "test".to_owned(),
        };
        prop_assert_eq!(cooldown.is_active(now), offset > 0);
        prop_assert_eq!(cooldown.remaining(now), (offset > 0).then(|| TimeDelta::seconds(offset)));
    }
}

#[test]
fn identifier_length_boundaries_are_enforced() {
    assert!("".parse::<AccountId>().is_err());
    assert!(
        "a".repeat(MAX_ACCOUNT_ID_LEN - 1)
            .parse::<AccountId>()
            .is_ok()
    );
    assert!("a".repeat(MAX_ACCOUNT_ID_LEN).parse::<AccountId>().is_ok());
    assert!(
        "a".repeat(MAX_ACCOUNT_ID_LEN + 1)
            .parse::<AccountId>()
            .is_err()
    );
    assert!("../profile".parse::<AccountId>().is_err());
}

#[test]
fn cooldown_expires_at_the_reset_instant() {
    let now = Utc::now();
    let cooldown = CooldownEntry {
        bucket: ModelBucket::default(),
        resets_at: now,
        reason: "test".to_owned(),
    };
    assert!(!cooldown.is_active(now));
    assert_eq!(cooldown.remaining(now), None);
    assert!(cooldown.is_active(now - TimeDelta::seconds(1)));
}
