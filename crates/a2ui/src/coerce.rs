use serde_json::Value;

/// The largest integer below which every integer has an exact double:
/// 2^53 - 1, the range RFC 8259 names as portable.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// A computed number as a JSON value.
///
/// An integral number within ±(2^53 - 1) becomes an integer, so `1e5` and
/// `100000` are the same value and `-0` is `0`. Others stay doubles, and a
/// non-finite number, which JSON cannot hold, is null.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "the cast only runs on integral values within ±2^53, which i64 holds exactly"
)]
pub(crate) fn number_value(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER {
        return Value::from(number as i64);
    }
    serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
}
