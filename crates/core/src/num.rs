//! Number helpers with JavaScript-compatible text conversion, so the output matches the TS
//! implementation (`String(n)`, `toFixed`, `parseFloat`, `Number(s)`).

/// `String(x)` for a JS number.
pub fn js_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x == 0.0 {
        return "0".into();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    // Rust's `{:e}` prints the shortest round-trip digits: `1.2345e6`, `5e-7`.
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let e: i32 = exp.parse().unwrap_or(0);
    let k = digits.len() as i32;
    let n = e + 1;
    let body = if k <= n && n <= 21 {
        format!("{}{}", digits, "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        let es = if n > 0 { format!("+{}", n - 1) } else { format!("{}", n - 1) };
        if k == 1 {
            format!("{digits}e{es}")
        } else {
            format!("{}.{}e{}", &digits[..1], &digits[1..], es)
        }
    };
    format!("{sign}{body}")
}

/// Rounds a non-negative plain decimal string (`123`, `0.5`, `12.3456`) to `decimals` places,
/// half up. Always returns exactly `decimals` fraction digits.
pub fn round_decimal_str(s: &str, decimals: usize) -> String {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let int = if int.is_empty() { "0" } else { int };
    let mut digits: Vec<u8> = int.bytes().collect();
    let fb = frac.as_bytes();
    for i in 0..decimals {
        digits.push(*fb.get(i).unwrap_or(&b'0'));
    }
    let round_up = fb.get(decimals).map(|d| *d >= b'5').unwrap_or(false);
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let (i, f) = digits.split_at(digits.len() - decimals);
    let mut i = String::from_utf8_lossy(i).trim_start_matches('0').to_string();
    if i.is_empty() {
        i.push('0');
    }
    if decimals == 0 {
        i
    } else {
        format!("{}.{}", i, String::from_utf8_lossy(f))
    }
}

/// `x.toFixed(decimals)`.
pub fn to_fixed(x: f64, decimals: usize) -> String {
    let decimals = decimals.min(100);
    if x.is_nan() {
        return "NaN".into();
    }
    if x.abs() >= 1e21 || x.is_infinite() {
        return js_to_string(x);
    }
    let neg = x < 0.0;
    // 25 extra digits of the exact binary value are enough to round half-up like JS does.
    let exact = format!("{:.*}", decimals + 25, x.abs());
    let r = round_decimal_str(&exact, decimals);
    if neg {
        format!("-{r}")
    } else {
        r
    }
}

/// Rounds an exact decimal string (`-1234.505`) half away from zero, like `toFixed` would do on
/// an exact value.
pub fn decimal_to_fixed(s: &str, decimals: usize) -> Option<String> {
    let plain = decimal_plain(s)?;
    let (neg, abs) = match plain.strip_prefix('-') {
        Some(a) => (true, a.to_string()),
        None => (false, plain),
    };
    let r = round_decimal_str(&abs, decimals.min(100));
    let zero = r.bytes().all(|b| b == b'0' || b == b'.');
    Some(if neg && !zero { format!("-{r}") } else { r })
}

/// Normalizes a decimal literal (`+1.50`, `1e3`, `-0.25E-2`) into plain `[-]int[.frac]` form.
pub fn decimal_plain(s: &str) -> Option<String> {
    let s = s.trim();
    let (neg, rest) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let (mant, exp) = match rest.find(['e', 'E']) {
        Some(i) => (&rest[..i], rest[i + 1..].parse::<i64>().ok()?),
        None => (rest, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if exp.abs() > 1000 {
        return None;
    }
    let digits = format!("{int}{frac}");
    let point = int.len() as i64 + exp;
    let (i, f) = if point <= 0 {
        (String::from("0"), format!("{}{}", "0".repeat((-point) as usize), digits))
    } else if point as usize >= digits.len() {
        (format!("{}{}", digits, "0".repeat(point as usize - digits.len())), String::new())
    } else {
        (digits[..point as usize].to_string(), digits[point as usize..].to_string())
    };
    let i = i.trim_start_matches('0');
    let i = if i.is_empty() { "0" } else { i };
    let f = f.trim_end_matches('0');
    let body = if f.is_empty() { i.to_string() } else { format!("{i}.{f}") };
    let zero = body == "0";
    Some(if neg && !zero { format!("-{body}") } else { body })
}

fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

/// `parseFloat(s)`: longest numeric prefix after leading whitespace.
pub fn js_parse_float(s: &str) -> f64 {
    let t = s.trim_start_matches(is_js_space);
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if t[i..].starts_with("Infinity") {
        return if b.first() == Some(&b'-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let start_digits = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut had_digits = i > start_digits;
    if i < b.len() && b[i] == b'.' {
        let j = i + 1;
        let mut k = j;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > j || had_digits {
            had_digits = had_digits || k > j;
            i = k;
        }
    }
    if !had_digits {
        return f64::NAN;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut k = i + 1;
        if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
            k += 1;
        }
        let ds = k;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > ds {
            i = k;
        }
    }
    t[..i].parse::<f64>().unwrap_or(f64::NAN)
}

/// `Number(s)` for a string.
pub fn js_string_to_number(s: &str) -> f64 {
    let t = s.trim_matches(is_js_space);
    if t.is_empty() {
        return 0.0;
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return u64::from_str_radix(rest, radix).map(|v| v as f64).unwrap_or(f64::NAN);
        }
    }
    let ok = t.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'));
    if !ok {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// Integer conversion used for numeric filter arguments (`ToIntegerOrInfinity`, clamped).
pub fn to_int(x: f64) -> i64 {
    if x.is_nan() {
        0
    } else if x >= i64::MAX as f64 {
        i64::MAX
    } else if x <= i64::MIN as f64 {
        i64::MIN
    } else {
        x.trunc() as i64
    }
}

/// `Math.round`.
pub fn js_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let r = (x + 0.5).floor();
    if r == 0.0 && x < 0.0 {
        -0.0
    } else {
        r
    }
}

/// Serializes an `f64` as a JSON number the way `JSON.stringify` would (`2` instead of `2.0`).
pub fn json_number(x: f64) -> serde_json::Value {
    if !x.is_finite() {
        return serde_json::Value::Null;
    }
    if x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 {
        return serde_json::Value::from(x as i64);
    }
    serde_json::Number::from_f64(x).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null)
}

/// serde helper: `#[serde(serialize_with = "crate::num::ser_f64")]`.
pub fn ser_f64<S: serde::Serializer>(x: &f64, s: S) -> Result<S::Ok, S::Error> {
    use serde::Serialize;
    json_number(*x).serialize(s)
}

pub fn ser_opt_f64<S: serde::Serializer>(x: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    use serde::Serialize;
    match x {
        Some(v) => json_number(*v).serialize(s),
        None => s.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_strings() {
        assert_eq!(js_to_string(1.5), "1.5");
        assert_eq!(js_to_string(2.0), "2");
        assert_eq!(js_to_string(-0.0), "0");
        assert_eq!(js_to_string(1e21), "1e+21");
        assert_eq!(js_to_string(1e20), "100000000000000000000");
        assert_eq!(js_to_string(1e-7), "1e-7");
        assert_eq!(js_to_string(0.000001), "0.000001");
        assert_eq!(js_to_string(123.456), "123.456");
        assert_eq!(js_to_string(1.2345e-10), "1.2345e-10");
    }

    #[test]
    fn fixed() {
        assert_eq!(to_fixed(1234567.891, 2), "1234567.89");
        assert_eq!(to_fixed(0.125, 2), "0.13");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(-1.5, 0), "-2");
        assert_eq!(to_fixed(-0.0001, 2), "-0.00");
        assert_eq!(to_fixed(9.995, 2), "9.99");
        assert_eq!(to_fixed(99.5, 0), "100");
        assert_eq!(to_fixed(10.5, 2), "10.50");
    }

    #[test]
    fn decimals() {
        assert_eq!(decimal_to_fixed("1234.505", 2).unwrap(), "1234.51");
        assert_eq!(decimal_to_fixed("-0.001", 2).unwrap(), "0.00");
        assert_eq!(decimal_to_fixed("1e3", 1).unwrap(), "1000.0");
        assert_eq!(decimal_plain("0012.3400").unwrap(), "12.34");
        assert_eq!(decimal_plain("-2.5E-3").unwrap(), "-0.0025");
        assert!(decimal_plain("abc").is_none());
    }

    #[test]
    fn parsing() {
        assert_eq!(js_parse_float("  12.5abc"), 12.5);
        assert_eq!(js_parse_float(".5"), 0.5);
        assert!(js_parse_float("abc").is_nan());
        assert_eq!(js_parse_float("1e3x"), 1000.0);
        assert_eq!(js_string_to_number(" 12 "), 12.0);
        assert_eq!(js_string_to_number(""), 0.0);
        assert!(js_string_to_number("12a").is_nan());
        assert!(js_string_to_number("inf").is_nan());
        assert_eq!(js_string_to_number("0x1F"), 31.0);
    }
}
