//! Bounded proofs of the session's pure date and time-of-day logic
//! (#631; `crates/rutis-bridge/src/session/values.rs`). Each proof is
//! checked over every value of its inputs, not a sample.
//!
//! Run from this directory, with Kani installed (`cargo kani setup`):
//!
//!     cargo kani
//!
//! Not run in the change that added this file: `cargo-kani` is not
//! installed here and the Kani release download stalled (see `README.md`).
//!
//! guarantee: bridge/value-tags
#![cfg(kani)]

use rutis_bridge::session::{Date, TimeOfDay};

/// A date the constructor accepts keeps the fields it was given and stays
/// in the range the `date` tag can carry (years 0001–9999, months 1–12,
/// days 1–31): no normalization, no wrap.
#[kani::proof]
fn date_new_keeps_valid_fields() {
    let year: u16 = kani::any();
    let month: u8 = kani::any();
    let day: u8 = kani::any();
    if let Ok(date) = Date::new(year, month, day) {
        assert!((1..=9999).contains(&year));
        assert!((1..=12).contains(&month));
        assert!((1..=31).contains(&day));
        assert_eq!(date.year(), year);
        assert_eq!(date.month(), month);
        assert_eq!(date.day(), day);
    }
}

/// No date has month 0 or day 0, whatever the other fields are.
#[kani::proof]
fn date_new_rejects_month_and_day_zero() {
    let year: u16 = kani::any();
    let month: u8 = kani::any();
    let day: u8 = kani::any();
    if month == 0 || day == 0 {
        assert!(Date::new(year, month, day).is_err());
    }
}

/// Every day in years 0001–9999, written as the number of days from
/// 1970-01-01, reads back as the same date and the same number. The
/// bounds are `Date::from_days_since_unix_epoch`'s own (0001-01-01 is
/// −719162 before 1970-01-01; 9999-12-31 is 2932896 after it).
#[kani::proof]
fn days_from_epoch_roundtrip() {
    let days: i64 = kani::any();
    kani::assume((-719_162..=2_932_896).contains(&days));
    let date =
        Date::from_days_since_unix_epoch(days).expect("a day in 0001–9999 is a date in 0001–9999");
    assert_eq!(date.days_since_unix_epoch(), days);
}

/// A time of day the constructor accepts keeps every field: hour 0–23,
/// minute and second 0–59, nanosecond below 10^9, no leap second and no
/// rounding.
#[kani::proof]
fn time_of_day_keeps_valid_fields() {
    let hour: u8 = kani::any();
    let minute: u8 = kani::any();
    let second: u8 = kani::any();
    let nanosecond: u32 = kani::any();
    if let Ok(time) = TimeOfDay::new(hour, minute, second, nanosecond) {
        assert!(hour <= 23);
        assert!(minute <= 59);
        assert!(second <= 59);
        assert!(nanosecond < 1_000_000_000);
        assert_eq!(time.hour(), hour);
        assert_eq!(time.minute(), minute);
        assert_eq!(time.second(), second);
        assert_eq!(time.nanosecond(), nanosecond);
    }
}
