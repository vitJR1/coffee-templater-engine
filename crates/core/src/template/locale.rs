//! A tiny locale table for the `currency` filter (open question §15.3).
//!
//! The TS version uses `Intl.NumberFormat`; the core has no ICU, so it ships patterns for common
//! locales that reproduce `Intl` output for them. Unknown locales fall back to `en-US`. Hosts that
//! need full ICU can declare their own `currency` host filter, which overrides this one.

use crate::num::{decimal_to_fixed, to_fixed};
use crate::value::Value;

const NBSP: &str = "\u{a0}";
const NNBSP: &str = "\u{202f}";

#[derive(Clone, Copy)]
enum Placement {
    /// `$1,234.50`
    Before,
    /// `€ 1.234,50`
    BeforeSpace,
    /// `1.234,50 €`
    AfterSpace,
}

#[derive(Clone, Copy)]
struct Locale {
    lang: &'static str,
    group: &'static str,
    decimal: &'static str,
    placement: Placement,
    /// Minimum integer digits before grouping kicks in (`es`/`pl` don't group 4-digit numbers).
    min_grouping: usize,
    /// Sign goes after the symbol (`€ -1,00`).
    sign_after_symbol: bool,
}

fn locale(tag: Option<&str>) -> Locale {
    let tag = tag.unwrap_or("en-US").replace('_', "-");
    let lower = tag.to_lowercase();
    let lang = lower.split('-').next().unwrap_or("en");
    let l =
        |lang, group, decimal, placement, min_grouping| Locale { lang, group, decimal, placement, min_grouping, sign_after_symbol: false };
    match lang {
        "de" if lower == "de-ch" => l("de", "’", ".", Placement::BeforeSpace, 1),
        "de" => l("de", ".", ",", Placement::AfterSpace, 1),
        "ru" => l("ru", NBSP, ",", Placement::AfterSpace, 1),
        "uk" => l("uk", NBSP, ",", Placement::AfterSpace, 1),
        "be" | "kk" => l("ru", NBSP, ",", Placement::AfterSpace, 1),
        "fr" => l("fr", NNBSP, ",", Placement::AfterSpace, 1),
        "es" => l("es", ".", ",", Placement::AfterSpace, 2),
        "it" => l("it", ".", ",", Placement::AfterSpace, 1),
        "pl" => l("pl", NBSP, ",", Placement::AfterSpace, 2),
        "cs" | "sk" => l("cs", NBSP, ",", Placement::AfterSpace, 1),
        "fi" | "sv" | "nb" | "no" => l("sv", NBSP, ",", Placement::AfterSpace, 1),
        "pt" if lower == "pt-br" || lower == "pt" => l("pt", ".", ",", Placement::BeforeSpace, 1),
        "pt" => l("pt", NBSP, ",", Placement::AfterSpace, 1),
        "nl" => {
            Locale { lang: "nl", group: ".", decimal: ",", placement: Placement::BeforeSpace, min_grouping: 1, sign_after_symbol: true }
        }
        "tr" => l("tr", ".", ",", Placement::Before, 1),
        "ja" => l("ja", ",", ".", Placement::Before, 1),
        "zh" => l("zh", ",", ".", Placement::Before, 1),
        "ko" => l("ko", ",", ".", Placement::Before, 1),
        _ => l("en", ",", ".", Placement::Before, 1),
    }
}

fn symbol(code: &str, lang: &str) -> String {
    let s = match (code, lang) {
        ("RUB", "ru") => "₽",
        ("UAH", "uk") | ("UAH", "ru") => "₴",
        ("PLN", "pl") => "zł",
        ("CZK", "cs") => "Kč",
        ("BRL", _) => "R$",
        ("USD", "pt") => "US$",
        ("JPY", "ja") => "￥",
        ("CNY", "zh") => "¥",
        ("CNY", _) => "CN¥",
        ("KRW", _) => "₩",
        ("TRY", "tr") => "₺",
        ("SEK", "sv") | ("NOK", "sv") => "kr",
        ("CAD", "en") => "CA$",
        ("AUD", "en") => "A$",
        ("USD", _) => "$",
        ("EUR", _) => "€",
        ("GBP", _) => "£",
        ("JPY", _) => "¥",
        ("INR", _) => "₹",
        ("ILS", _) => "₪",
        ("VND", _) => "₫",
        ("NGN", _) => "₦",
        _ => return code.to_string(),
    };
    s.to_string()
}

fn currency_digits(code: &str) -> usize {
    match code {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "PYG" | "XAF" | "XOF" => 0,
        "KWD" | "BHD" | "OMR" | "JOD" | "TND" | "LYD" | "IQD" => 3,
        _ => 2,
    }
}

fn group(int: &str, sep: &str, min_grouping: usize) -> String {
    let len = int.len();
    if len < 4 + (min_grouping - 1) {
        return int.to_string();
    }
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push_str(sep);
        }
        out.push(c);
    }
    out
}

pub fn format_currency(v: &Value, code: &str, locale_tag: Option<&str>, decimals: Option<usize>) -> String {
    let code = code.to_uppercase();
    let loc = locale(locale_tag);
    let digits = decimals.unwrap_or_else(|| currency_digits(&code));
    let (fixed, negative) = match v {
        Value::Decimal(d) => match decimal_to_fixed(d, digits) {
            Some(f) => match f.strip_prefix('-') {
                Some(a) => (a.to_string(), true),
                None => (f, false),
            },
            None => return "NaN".into(),
        },
        _ => {
            let n = v.to_number();
            if n.is_nan() {
                return "NaN".into();
            }
            (to_fixed(n.abs(), digits), n < 0.0)
        }
    };
    let (int, frac) = fixed.split_once('.').unwrap_or((&fixed, ""));
    let mut number = group(int, loc.group, loc.min_grouping);
    if !frac.is_empty() {
        number.push_str(loc.decimal);
        number.push_str(frac);
    }
    let sym = symbol(&code, loc.lang);
    let sign = if negative { "-" } else { "" };
    let code_like = sym.chars().all(|c| c.is_ascii_alphabetic());
    match loc.placement {
        Placement::Before if code_like => format!("{sign}{sym}{NBSP}{number}"),
        Placement::Before => format!("{sign}{sym}{number}"),
        Placement::BeforeSpace if loc.sign_after_symbol => format!("{sym}{NBSP}{sign}{number}"),
        Placement::BeforeSpace => format!("{sign}{sym}{NBSP}{number}"),
        Placement::AfterSpace => format!("{sign}{number}{NBSP}{sym}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(v: f64, code: &str, loc: Option<&str>) -> String {
        format_currency(&Value::num(v), code, loc, None)
    }

    #[test]
    fn intl_like() {
        assert_eq!(c(1234.5, "USD", None), "$1,234.50");
        assert_eq!(c(-1234.5, "USD", Some("en-US")), "-$1,234.50");
        assert_eq!(c(25.0, "EUR", Some("de-DE")), "25,00\u{a0}€");
        assert_eq!(c(1234.5, "RUB", Some("ru-RU")), "1\u{a0}234,50\u{a0}₽");
        assert_eq!(c(1234.0, "EUR", Some("es-ES")), "1234,00\u{a0}€");
        assert_eq!(c(12345.0, "EUR", Some("es-ES")), "12.345,00\u{a0}€");
        assert_eq!(c(1234.0, "JPY", Some("ja-JP")), "￥1,234");
        assert_eq!(c(5.0, "CHF", None), "CHF\u{a0}5.00");
        assert_eq!(c(-1.0, "EUR", Some("nl-NL")), "€\u{a0}-1,00");
    }
}
