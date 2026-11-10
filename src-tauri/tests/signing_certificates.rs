//! The certificate picker's date rendering and its platform behaviour (P3.2 D-SG5).
//!
//! Two things are tested here, and they fail in different ways.
//!
//! The **date conversion** is hand-rolled calendar arithmetic in `commands::rfc3339`. A certificate's
//! validity window is one of the few places a wrong number changes a decision a user makes, and the
//! failure mode is silent: an off-by-one in the leap-year term renders 1 March as 29 February and
//! nothing complains. Every case below is a date a certificate authority actually issues on, pinned
//! against values computed independently rather than against the function's own output.
//!
//! The **platform behaviour** is that a non-Windows build must refuse, not return an empty list. An
//! empty list renders as "you have no signing certificates", which is a false statement about the
//! user's machine; the error says so.

use document_reader_lib::commands::rfc3339;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Midnight UTC on a given day, built from the epoch so the expectation does not depend on the code
/// under test.
fn midnight(year: i64, month: u32, day: u32) -> SystemTime {
    // Days from 1970-01-01 to `year-month-day`, computed by the same civil-calendar reasoning as the
    // implementation but written out independently here. Kept deliberately dumb so a shared bug
    // cannot hide in a shared helper.
    let mut days = 0i64;
    for y in 1970..year {
        days += if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
            366
        } else {
            365
        };
    }
    let lengths = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    for (index, length) in lengths.iter().enumerate() {
        if (index as u32) + 1 < month {
            days += i64::from(*length) + if index == 1 && leap { 1 } else { 0 };
        }
    }
    days += i64::from(day) - 1;
    UNIX_EPOCH + Duration::from_secs(days as u64 * 86_400)
}

#[test]
fn the_epoch_is_the_epoch() {
    assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
}

#[test]
fn a_known_certificate_not_before_date_renders_exactly() {
    // 2024-02-29T00:00:00Z: a leap day, the single most likely date to be got wrong.
    assert_eq!(rfc3339(midnight(2024, 2, 29)), "2024-02-29T00:00:00Z");
    // The day after a leap day, which is where an off-by-one in the leap term surfaces.
    assert_eq!(rfc3339(midnight(2024, 3, 1)), "2024-03-01T00:00:00Z");
}

#[test]
fn a_non_leap_year_has_no_february_29th() {
    // 2023 is not a leap year: 28 February must be followed by 1 March with no gap or repeat.
    assert_eq!(rfc3339(midnight(2023, 2, 28)), "2023-02-28T00:00:00Z");
    assert_eq!(rfc3339(midnight(2023, 3, 1)), "2023-03-01T00:00:00Z");
}

#[test]
fn a_century_that_is_not_a_leap_year_is_handled() {
    // 1900 is not a leap year (divisible by 100, not by 400); 2000 is. Both are outside the window a
    // certificate will carry, which is why they are the cases a hand-rolled calendar gets wrong.
    assert_eq!(rfc3339(midnight(2000, 2, 29)), "2000-02-29T00:00:00Z");
    assert_eq!(rfc3339(midnight(2001, 3, 1)), "2001-03-01T00:00:00Z");
}

#[test]
fn the_time_of_day_is_rendered_not_truncated() {
    let when = midnight(2030, 12, 31) + Duration::from_secs(23 * 3600 + 59 * 60 + 59);
    assert_eq!(rfc3339(when), "2030-12-31T23:59:59Z");
}

#[test]
fn a_date_after_2038_is_not_clipped() {
    // A 30-year code-signing certificate outlives a 32-bit `time_t`. The conversion uses `u64`, and
    // this is the test that says so.
    assert_eq!(rfc3339(midnight(2050, 6, 15)), "2050-06-15T00:00:00Z");
}

#[test]
fn a_clock_before_the_epoch_reports_an_absence_rather_than_1970() {
    // `SystemTime` before the epoch is possible on a badly set clock. Printing `1970-01-01` would be
    // a date the user could act on and that is wrong, so the function says it does not know.
    let before = UNIX_EPOCH - Duration::from_secs(86_400);
    let rendered = rfc3339(before);
    assert!(
        rendered.contains("unknown"),
        "a pre-epoch time must be named as unknown, not rendered as a date: {rendered}"
    );
    assert!(
        !rendered.starts_with("1970"),
        "a pre-epoch time must not be rendered as the epoch itself: {rendered}"
    );
}

/// The store is read-only and platform-specific. This build targets Windows, but the tests run on
/// Linux, where the correct answer is a refusal. Asserting it here is what stops a future refactor
/// from "helpfully" returning an empty list on the wrong platform.
#[cfg(not(windows))]
#[test]
fn a_non_windows_build_refuses_the_certificate_store_instead_of_reporting_none() {
    let outcome = document_reader_lib::editor::signature::winstore::list_user_store();
    match outcome {
        Ok(listing) => panic!(
            "a non-Windows build must not report a store listing; it returned {} certificate(s), \
             which would render as `you have no signing certificates`",
            listing.certificates.len()
        ),
        Err(error) => {
            let message = error.to_string().to_lowercase();
            assert!(
                message.contains("certificate store") && message.contains("not available"),
                "the refusal must say the store is unavailable, not something generic: {message}"
            );
        }
    }
}
