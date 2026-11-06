//! Built-in filters, ported from the TS implementation. A filter receives the piped value and the
//! filter arguments: `{{ value | name: arg1, arg2 }}`.

use crate::date::DateTime;
use crate::num::{decimal_to_fixed, js_round, to_fixed, to_int};
use crate::value::Value;
use std::cmp::Ordering;
use std::rc::Rc;

pub const BUILTIN: &[&str] = &[
    "upper",
    "lower",
    "capitalize",
    "trim",
    "replace",
    "truncate",
    "prepend",
    "append",
    "default",
    "number",
    "fixed",
    "round",
    "currency",
    "percent",
    "date",
    "join",
    "length",
    "first",
    "last",
    "reverse",
    "sort",
    "where",
    "slice",
    "json",
    "eq",
    "ne",
    "gt",
    "gte",
    "lt",
    "lte",
    "not",
    "and",
    "or",
    "contains",
    "empty",
    "yesno",
];

pub fn is_builtin(name: &str) -> bool {
    BUILTIN.contains(&name)
}

/// Argument `i`, or `None` when missing or `undefined` (JS default parameters).
fn arg(args: &[Value], i: usize) -> Option<&Value> {
    args.get(i).filter(|v| !matches!(v, Value::Undefined))
}

fn arg_str(args: &[Value], i: usize, default: &str) -> String {
    arg(args, i).map(Value::js_string).unwrap_or_else(|| default.to_string())
}

fn arg_int(args: &[Value], i: usize, default: i64) -> i64 {
    arg(args, i).map(|v| to_int(v.js_number())).unwrap_or(default)
}

fn digits(args: &[Value], i: usize, default: i64) -> usize {
    arg_int(args, i, default).clamp(0, 100) as usize
}

fn s(v: String) -> Value {
    Value::string(v)
}

fn chars_slice(text: &str, start: i64, end: Option<i64>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let (a, b) = slice_bounds(chars.len(), start, end);
    chars[a..b].iter().collect()
}

/// `Array.prototype.slice` bounds.
fn slice_bounds(len: usize, start: i64, end: Option<i64>) -> (usize, usize) {
    let len_i = len as i64;
    let norm = |x: i64| if x < 0 { (len_i + x).max(0) } else { x.min(len_i) };
    let a = norm(start);
    let b = norm(end.unwrap_or(len_i));
    (a as usize, (b.max(a)) as usize)
}

/// Stable merge sort that tolerates inconsistent comparators (unlike `slice::sort_by`).
fn merge_sort(v: &mut Vec<Value>, cmp: &dyn Fn(&Value, &Value) -> Ordering) {
    if v.len() <= 1 {
        return;
    }
    let mut right = v.split_off(v.len() / 2);
    merge_sort(v, cmp);
    merge_sort(&mut right, cmp);
    let left = std::mem::take(v);
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
    loop {
        match (l.peek(), r.peek()) {
            (Some(a), Some(b)) => {
                if cmp(b, a) == Ordering::Less {
                    out.push(r.next().unwrap_or_default());
                } else {
                    out.push(l.next().unwrap_or_default());
                }
            }
            (Some(_), None) => out.push(l.next().unwrap_or_default()),
            (None, Some(_)) => out.push(r.next().unwrap_or_default()),
            (None, None) => break,
        }
    }
    *v = out;
}

/// Formats a number: fixed decimals, thousands separator and decimal separator.
/// `{{ 1234567.891 | number: 2, " ", "," }}` → `1 234 567,89`
pub fn format_number(value: f64, decimals: usize, thousands: &str, decimal: &str) -> String {
    if !value.is_finite() {
        return String::new();
    }
    let fixed = to_fixed(value.abs(), decimals);
    let negative = value < 0.0 && fixed.bytes().any(|b| b.is_ascii_digit() && b != b'0');
    group_fixed(&fixed, negative, thousands, decimal, 1)
}

/// `fixed` is a plain non-negative decimal string. Groups the integer part by 3.
fn group_fixed(fixed: &str, negative: bool, thousands: &str, decimal: &str, min_grouping: usize) -> String {
    let (int, frac) = fixed.split_once('.').unwrap_or((fixed, ""));
    let mut grouped = String::new();
    let len = int.len();
    let group = len > 3 + (min_grouping - 1);
    for (i, c) in int.chars().enumerate() {
        if group && i > 0 && (len - i).is_multiple_of(3) {
            grouped.push_str(thousands);
        }
        grouped.push(c);
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&grouped);
    if !frac.is_empty() {
        out.push_str(decimal);
        out.push_str(frac);
    }
    out
}

/// Fixed-decimals text of a numeric value; `$decimal` values are rounded exactly.
fn fixed_text(v: &Value, decimals: usize) -> (String, bool) {
    if let Value::Decimal(d) = v {
        if let Some(f) = decimal_to_fixed(d, decimals) {
            return match f.strip_prefix('-') {
                Some(abs) => (abs.to_string(), true),
                None => (f, false),
            };
        }
    }
    let n = v.to_number();
    let f = to_fixed(n.abs(), decimals);
    let neg = n < 0.0 && f.bytes().any(|b| b.is_ascii_digit() && b != b'0');
    (f, neg)
}

fn to_date(v: &Value) -> Option<DateTime> {
    match v {
        Value::Date(d) => Some(*d),
        Value::Number(n) if n.value.is_finite() => Some(DateTime::from_ms(n.value as i64, false)),
        Value::String(s) => DateTime::parse(s),
        _ => None,
    }
}

/// `v === other || (v != null && other != null && String(v) === String(other))`
fn loose_eq(v: &Value, other: &Value) -> bool {
    v.strict_eq(other) || (!v.is_nullish() && !other.is_nullish() && v.js_string() == other.js_string())
}

fn cmp_num(v: &Value, other: Option<&Value>, f: fn(f64, f64) -> bool) -> Value {
    let o = other.map(Value::to_number).unwrap_or(f64::NAN);
    Value::Bool(f(v.to_number(), o))
}

/// Applies a built-in filter. Returns `None` when `name` is not a built-in.
pub fn apply(name: &str, v: Value, args: &[Value]) -> Option<Value> {
    let nullish = v.is_nullish();
    Some(match name {
        // strings
        "upper" => {
            if nullish {
                v
            } else {
                s(v.js_string().to_uppercase())
            }
        }
        "lower" => {
            if nullish {
                v
            } else {
                s(v.js_string().to_lowercase())
            }
        }
        "capitalize" => {
            if nullish {
                v
            } else {
                let t = v.js_string();
                let mut c = t.chars();
                match c.next() {
                    Some(f) => s(f.to_uppercase().chain(c).collect()),
                    None => s(t),
                }
            }
        }
        "trim" => {
            if nullish {
                v
            } else {
                s(v.js_string().trim_matches(super::expression::is_js_space).to_string())
            }
        }
        "replace" => {
            if nullish {
                v
            } else {
                let text = v.js_string();
                let search = arg(args, 0).map(Value::js_string).unwrap_or_else(|| "undefined".into());
                let repl = arg_str(args, 1, "");
                if search.is_empty() {
                    s(text.chars().map(String::from).collect::<Vec<_>>().join(&repl))
                } else {
                    s(text.replace(&search, &repl))
                }
            }
        }
        "truncate" => {
            if nullish {
                v
            } else {
                let text = v.js_string();
                let length = arg_int(args, 0, 50);
                let suffix = arg_str(args, 1, "…");
                let n = text.chars().count() as i64;
                if n > length {
                    let keep = (length - suffix.chars().count() as i64).max(0);
                    s(format!("{}{}", chars_slice(&text, 0, Some(keep)), suffix))
                } else {
                    s(text)
                }
            }
        }
        "prepend" => {
            if v.is_empty() {
                v
            } else {
                s(format!("{}{}", arg_str(args, 0, "undefined"), v.js_string()))
            }
        }
        "append" => {
            if v.is_empty() {
                v
            } else {
                s(format!("{}{}", v.js_string(), arg_str(args, 0, "undefined")))
            }
        }

        // fallbacks
        "default" => {
            if v.is_empty() {
                arg(args, 0).cloned().unwrap_or_else(|| Value::str(""))
            } else {
                v
            }
        }

        // numbers / dates
        "number" => {
            if v.is_empty() {
                v
            } else {
                let decimals = digits(args, 0, 0);
                let thousands = arg_str(args, 1, " ");
                let decimal = arg_str(args, 2, ",");
                if matches!(v, Value::Decimal(_)) {
                    let (f, neg) = fixed_text(&v, decimals);
                    s(group_fixed(&f, neg, &thousands, &decimal, 1))
                } else {
                    s(format_number(v.to_number(), decimals, &thousands, &decimal))
                }
            }
        }
        "fixed" => {
            if v.is_empty() {
                v
            } else {
                let d = digits(args, 0, 2);
                match &v {
                    Value::Decimal(dec) => s(decimal_to_fixed(dec, d).unwrap_or_else(|| "NaN".into())),
                    _ => s(to_fixed(v.to_number(), d)),
                }
            }
        }
        "round" => {
            if v.is_empty() {
                v
            } else {
                let d = arg_int(args, 0, 0).clamp(-300, 300) as i32;
                if let Value::Decimal(dec) = &v {
                    if d >= 0 {
                        if let Some(f) = decimal_to_fixed(dec, d as usize) {
                            return Some(Value::Decimal(Rc::from(f)));
                        }
                    }
                }
                let p = 10f64.powi(d);
                Value::num(js_round(v.to_number() * p) / p)
            }
        }
        "currency" => {
            if v.is_empty() {
                v
            } else {
                let code = arg_str(args, 0, "USD");
                let locale = arg(args, 1).map(Value::js_string);
                let decimals = arg(args, 2).map(|d| to_int(d.js_number()).clamp(0, 20) as usize);
                s(super::locale::format_currency(&v, &code, locale.as_deref(), decimals))
            }
        }
        "percent" => {
            if v.is_empty() {
                v
            } else {
                s(format!("{}%", to_fixed(v.to_number() * 100.0, digits(args, 0, 0))))
            }
        }
        "date" => match to_date(&v) {
            Some(d) => s(d.format(&arg_str(args, 0, "DD.MM.YYYY"))),
            None => v,
        },

        // collections
        "join" => match &v {
            Value::Array(a) => {
                let sep = arg_str(args, 0, ", ");
                s(a.iter().map(|x| if x.is_nullish() { String::new() } else { x.js_string() }).collect::<Vec<_>>().join(&sep))
            }
            _ => v,
        },
        "length" => Value::num(match &v {
            Value::Array(a) => a.len() as f64,
            Value::String(t) => t.chars().count() as f64,
            Value::Object(o) => o.len() as f64,
            _ => 0.0,
        }),
        "first" => match &v {
            Value::Array(a) => a.first().cloned().unwrap_or_default(),
            _ => v,
        },
        "last" => match &v {
            Value::Array(a) => a.last().cloned().unwrap_or_default(),
            _ => v,
        },
        "reverse" => match &v {
            Value::Array(a) => Value::array(a.iter().rev().cloned().collect()),
            Value::String(t) => s(t.chars().rev().collect()),
            _ => v,
        },
        "sort" => match &v {
            Value::Array(a) => {
                let key = arg(args, 0).map(Value::js_string).filter(|k| !k.is_empty());
                let desc = arg_str(args, 1, "asc").to_lowercase() == "desc";
                let pick = |x: &Value| match &key {
                    Some(k) => super::expression::get(x, &super::expression::Segment::Key(k.clone())),
                    None => x.clone(),
                };
                let mut list: Vec<Value> = a.to_vec();
                let cmp = |a: &Value, b: &Value| {
                    let (pa, pb) = (pick(a), pick(b));
                    let o = match pa.js_compare(&pb) {
                        Some(Ordering::Greater) => Ordering::Greater,
                        Some(Ordering::Less) => Ordering::Less,
                        _ => Ordering::Equal,
                    };
                    if desc {
                        o.reverse()
                    } else {
                        o
                    }
                };
                merge_sort(&mut list, &cmp);
                Value::array(list)
            }
            _ => v,
        },
        "where" => match &v {
            Value::Array(a) => {
                let key = super::expression::Segment::Key(arg_str(args, 0, "undefined"));
                let wanted = args.get(1);
                Value::array(
                    a.iter()
                        .filter(|x| {
                            let field = super::expression::get(x, &key);
                            match wanted {
                                Some(w) => field.strict_eq(w),
                                None => field.truthy(),
                            }
                        })
                        .cloned()
                        .collect(),
                )
            }
            _ => v,
        },
        "slice" => {
            let start = arg_int(args, 0, 0);
            let end = arg(args, 1).map(|e| to_int(e.js_number()));
            match &v {
                Value::Array(a) => {
                    let (x, y) = slice_bounds(a.len(), start, end);
                    Value::array(a[x..y].to_vec())
                }
                Value::String(t) => s(chars_slice(t, start, end)),
                _ => v,
            }
        }
        "json" => {
            let indent = match arg(args, 0) {
                Some(Value::String(t)) => t.chars().take(10).collect(),
                Some(n) => " ".repeat(to_int(n.js_number()).clamp(0, 10) as usize),
                None => String::new(),
            };
            match v.to_json_string(&indent) {
                Some(t) => s(t),
                None => Value::Undefined,
            }
        }

        // comparison / logic
        "eq" => Value::Bool(loose_eq(&v, args.first().unwrap_or(&Value::Undefined))),
        "ne" => Value::Bool(!loose_eq(&v, args.first().unwrap_or(&Value::Undefined))),
        "gt" => cmp_num(&v, args.first(), |a, b| a > b),
        "gte" => cmp_num(&v, args.first(), |a, b| a >= b),
        "lt" => cmp_num(&v, args.first(), |a, b| a < b),
        "lte" => cmp_num(&v, args.first(), |a, b| a <= b),
        "not" => Value::Bool(v.is_empty() || !v.truthy()),
        "and" => Value::Bool(std::iter::once(&v).chain(args.iter()).all(|x| !x.is_empty() && x.truthy())),
        "or" => std::iter::once(&v).chain(args.iter()).find(|x| !x.is_empty() && x.truthy()).cloned().unwrap_or(Value::Bool(false)),
        "contains" => {
            let item = args.first().cloned().unwrap_or_default();
            Value::Bool(match &v {
                Value::Array(a) => a.iter().any(|x| x.same_value_zero(&item)),
                Value::String(t) => t.contains(&item.js_string()),
                _ => false,
            })
        }
        "empty" => Value::Bool(v.is_empty()),
        "yesno" => {
            if v.is_empty() || !v.truthy() {
                arg(args, 1).cloned().unwrap_or_else(|| Value::str("no"))
            } else {
                arg(args, 0).cloned().unwrap_or_else(|| Value::str("yes"))
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::parse_data;

    fn f(name: &str, v: &str, args: &str) -> String {
        let v = parse_data(v, &[]).unwrap();
        let Value::Array(a) = parse_data(args, &[]).unwrap() else { panic!() };
        apply(name, v, &a).unwrap().js_string()
    }

    #[test]
    fn strings() {
        assert_eq!(f("upper", "\"ann\"", "[]"), "ANN");
        assert_eq!(f("capitalize", "\"élan\"", "[]"), "Élan");
        assert_eq!(f("truncate", "\"abcdefgh\"", "[5]"), "abcd…");
        assert_eq!(f("replace", "\"a-b-c\"", "[\"-\", \"+\"]"), "a+b+c");
        assert_eq!(f("default", "null", "[\"—\"]"), "—");
        assert_eq!(f("prepend", "\"\"", "[\"x\"]"), "");
        assert_eq!(f("append", "\"a\"", "[\"x\"]"), "ax");
    }

    #[test]
    fn numbers() {
        assert_eq!(f("number", "1234567.891", "[2]"), "1 234 567,89");
        assert_eq!(f("number", "-0.001", "[2]"), "0,00");
        assert_eq!(f("number", "{\"$decimal\":\"12345678901234567.125\"}", "[2, \",\", \".\"]"), "12,345,678,901,234,567.13");
        assert_eq!(f("fixed", "\"1,5\"", "[]"), "1.50");
        assert_eq!(f("round", "2.345", "[2]"), "2.35");
        assert_eq!(f("percent", "0.256", "[1]"), "25.6%");
    }

    #[test]
    fn collections() {
        assert_eq!(f("join", "[\"a\",\"b\"]", "[\" / \"]"), "a / b");
        assert_eq!(f("sort", "[{\"n\":1},{\"n\":3},{\"n\":2}]", "[\"n\", \"desc\"]"), "[object Object],[object Object],[object Object]");
        let sorted = apply("sort", parse_data("[3,1,2,10]", &[]).unwrap(), &[]).unwrap();
        assert_eq!(sorted.js_string(), "1,2,3,10");
        assert_eq!(f("where", "[{\"s\":\"paid\",\"n\":1},{\"s\":\"due\",\"n\":2}]", "[\"s\",\"paid\"]").len(), "[object Object]".len());
        assert_eq!(f("slice", "[1,2,3,4]", "[1,-1]"), "2,3");
        assert_eq!(f("json", "{\"a\":[1,2]}", "[]"), "{\"a\":[1,2]}");
        assert_eq!(f("length", "\"héllo\"", "[]"), "5");
        assert_eq!(f("reverse", "\"abc\"", "[]"), "cba");
    }

    #[test]
    fn logic() {
        assert_eq!(f("gt", "150", "[100]"), "true");
        assert_eq!(f("eq", "1", "[\"1\"]"), "true");
        assert_eq!(f("ne", "1", "[2]"), "true");
        assert_eq!(f("or", "\"\"", "[0, \"x\"]"), "x");
        assert_eq!(f("and", "1", "[[]]"), "false");
        assert_eq!(f("contains", "[1,2]", "[2]"), "true");
        assert_eq!(f("yesno", "true", "[\"Оплачено\", \"Не оплачено\"]"), "Оплачено");
        assert_eq!(f("not", "[]", "[]"), "true");
    }

    #[test]
    fn dates() {
        assert_eq!(f("date", "{\"$date\":\"2024-01-31\"}", "[]"), "31.01.2024");
        assert_eq!(f("date", "\"2024-01-31T10:05:00Z\"", "[\"HH:mm\"]"), "10:05");
        assert_eq!(f("date", "\"nope\"", "[]"), "nope");
    }
}
