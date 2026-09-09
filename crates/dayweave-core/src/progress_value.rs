//! Exact scalar values for independent, user-recorded item progress.
//!
//! These values neither complete an item nor grant execution credit. Canonical
//! decimal strings keep native clients and services independent of floating-point
//! rounding, locale and scientific-notation serialization.

use thiserror::Error;

pub const MAX_PROGRESS_COMPONENTS: usize = 16;
pub const MAX_PROGRESS_NAME_SCALARS: usize = 80;
pub const MAX_PROGRESS_UNIT_SCALARS: usize = 32;
pub const MAX_PROGRESS_BASIS_POINTS: u16 = 10_000;
/// One hundred Julian years. Elapsed and remaining are bounded independently.
pub const MAX_PROGRESS_SECONDS: u64 = 3_155_760_000;
/// Maximum magnitude, in millionths: 999999999999.999999.
pub const MAX_PROGRESS_SCALED_QUANTITY: i64 = 999_999_999_999_999_999;
const QUANTITY_SCALE: i64 = 1_000_000;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ProgressDecimalError {
    #[error("quantity must be a plain decimal string")]
    InvalidSyntax,
    #[error("quantity must use its canonical decimal spelling")]
    NonCanonical,
    #[error("quantity supports at most six decimal places")]
    TooPrecise,
    #[error("quantity exceeds the supported magnitude")]
    OutOfRange,
}

/// Parses a canonical plain decimal into exact signed millionths.
///
/// # Errors
///
/// Rejects non-ASCII digits, exponents, whitespace, plus signs, leading integer
/// zeros, negative zero, trailing fractional zeros, excessive precision and
/// out-of-range magnitudes. The error never includes user content.
pub fn parse_progress_decimal(value: &str) -> Result<i64, ProgressDecimalError> {
    let (negative, magnitude) = value
        .strip_prefix('-')
        .map_or((false, value), |rest| (true, rest));
    let (integer, fraction) = match magnitude.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (magnitude, None),
    };
    if integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.is_some_and(|digits| {
            digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(ProgressDecimalError::InvalidSyntax);
    }
    if integer.len() > 1 && integer.starts_with('0') {
        return Err(ProgressDecimalError::NonCanonical);
    }
    if fraction.is_some_and(|digits| digits.len() > 6) {
        return Err(ProgressDecimalError::TooPrecise);
    }
    if fraction.is_some_and(|digits| digits.ends_with('0')) {
        return Err(ProgressDecimalError::NonCanonical);
    }
    if integer.len() > 12 {
        return Err(ProgressDecimalError::OutOfRange);
    }
    let whole = integer
        .parse::<i64>()
        .map_err(|_| ProgressDecimalError::OutOfRange)?;
    let fractional = match fraction {
        Some(digits) => {
            let amount = digits
                .parse::<i64>()
                .map_err(|_| ProgressDecimalError::InvalidSyntax)?;
            let decimal_places =
                u32::try_from(digits.len()).map_err(|_| ProgressDecimalError::TooPrecise)?;
            amount * 10_i64.pow(6 - decimal_places)
        }
        None => 0,
    };
    let scaled = whole
        .checked_mul(QUANTITY_SCALE)
        .and_then(|scaled| scaled.checked_add(fractional))
        .filter(|scaled| *scaled <= MAX_PROGRESS_SCALED_QUANTITY)
        .ok_or(ProgressDecimalError::OutOfRange)?;
    if negative && scaled == 0 {
        return Err(ProgressDecimalError::NonCanonical);
    }
    Ok(if negative { -scaled } else { scaled })
}

/// Returns the unique wire spelling of a supported exact value.
#[must_use]
pub fn format_progress_decimal(scaled_millionths: i64) -> Option<String> {
    let magnitude = scaled_millionths.checked_abs()?;
    if magnitude > MAX_PROGRESS_SCALED_QUANTITY {
        return None;
    }
    let integer = magnitude / QUANTITY_SCALE;
    let fraction = magnitude % QUANTITY_SCALE;
    let mut value = if fraction == 0 {
        integer.to_string()
    } else {
        let fraction = format!("{fraction:06}");
        format!("{integer}.{}", fraction.trim_end_matches('0'))
    };
    if scaled_millionths < 0 {
        value.insert(0, '-');
    }
    Some(value)
}

/// Validates a name/unit using Unicode scalar count, not UTF-8 bytes, UTF-16
/// code units or grapheme clusters. Whitespace uses Unicode `White_Space`.
#[must_use]
pub fn is_valid_progress_label(value: &str, max_scalars: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.chars().count() <= max_scalars
        && !value
            .chars()
            .any(|character| matches!(u32::from(character), 0..=31 | 127..=159))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_values_round_trip_without_floating_point() {
        for (wire, scaled) in [
            ("0", 0),
            ("1", 1_000_000),
            ("-1", -1_000_000),
            ("0.000001", 1),
            ("-0.000001", -1),
            ("3.5", 3_500_000),
            ("999999999999.999999", MAX_PROGRESS_SCALED_QUANTITY),
            ("-999999999999.999999", -MAX_PROGRESS_SCALED_QUANTITY),
        ] {
            assert_eq!(parse_progress_decimal(wire), Ok(scaled));
            assert_eq!(format_progress_decimal(scaled).as_deref(), Some(wire));
        }
        for scaled in -100_000..=100_000 {
            let wire = format_progress_decimal(scaled).expect("supported value");
            assert_eq!(parse_progress_decimal(&wire), Ok(scaled));
        }
    }

    #[test]
    fn invalid_values_have_content_free_precise_errors() {
        for value in [
            "", "-", "+1", "1e2", "1E2", " 1", "1 ", "1.2.3", ".1", "1.", "１２", "NaN",
        ] {
            assert_eq!(
                parse_progress_decimal(value),
                Err(ProgressDecimalError::InvalidSyntax)
            );
        }
        for value in ["01", "-01", "-0", "0.0", "1.20", "-0.000000"] {
            assert_eq!(
                parse_progress_decimal(value),
                Err(ProgressDecimalError::NonCanonical)
            );
        }
        assert_eq!(
            parse_progress_decimal("0.0000001"),
            Err(ProgressDecimalError::TooPrecise)
        );
        assert_eq!(
            parse_progress_decimal("1000000000000"),
            Err(ProgressDecimalError::OutOfRange)
        );
        assert_eq!(format_progress_decimal(i64::MIN), None);
        assert_eq!(format_progress_decimal(i64::MAX), None);
        assert_eq!(
            format_progress_decimal(MAX_PROGRESS_SCALED_QUANTITY + 1),
            None
        );
    }

    #[test]
    fn labels_have_scalar_limits_and_explicit_control_rules() {
        assert!(is_valid_progress_label("Read chapters", 80));
        assert!(is_valid_progress_label("📖".repeat(32).as_str(), 32));
        assert!(!is_valid_progress_label("📖".repeat(33).as_str(), 32));
        assert!(is_valid_progress_label("e\u{301}", 2));
        assert!(!is_valid_progress_label("e\u{301}", 1));
        for value in [
            "",
            " ",
            " name",
            "name ",
            "\u{a0}name",
            "name\u{3000}",
            "a\nb",
            "a\u{85}b",
            "a\u{7f}b",
        ] {
            assert!(!is_valid_progress_label(value, 80));
        }
    }
}
