//! Dates said in words, resolved by code: "yesterday", "dün", "3 days ago", "geçen salı", "last
//! month", "in March", "2026-10". The model never does date arithmetic; it copies what was said and
//! this turns it into a day or a span of days against the time it was said. A date known only to the
//! month or year stays a span, never a made-up day.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

use crate::text::tokens;

/// A day, or a span of days known no closer, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct When {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl When {
    pub fn day(d: NaiveDate) -> Self {
        Self { start: d, end: d }
    }

    pub fn is_day(&self) -> bool {
        self.start == self.end
    }

    /// `2026-10-27`, `2026-10` for a whole month, `2026` for a whole year, else `start..end`.
    pub fn display(&self) -> String {
        if self.is_day() {
            return self.start.format("%Y-%m-%d").to_string();
        }
        let (s, e) = (self.start, self.end);
        if s.day() == 1 && e == month_end(s.year(), s.month()) {
            return s.format("%Y-%m").to_string();
        }
        if s.ordinal() == 1 && e.month() == 12 && e.day() == 31 && s.year() == e.year() {
            return s.year().to_string();
        }
        format!("{}..{}", s.format("%Y-%m-%d"), e.format("%Y-%m-%d"))
    }

    /// Reads what `display` writes.
    pub fn parse_display(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some((a, b)) = text.split_once("..") {
            let start = NaiveDate::parse_from_str(a, "%Y-%m-%d").ok()?;
            let end = NaiveDate::parse_from_str(b, "%Y-%m-%d").ok()?;
            return (start <= end).then_some(Self { start, end });
        }
        iso(text)
    }

    /// Whole days from this to `day`: positive when this is before it, by its end.
    pub fn days_before(&self, day: NaiveDate) -> i64 {
        (day - self.end).num_days()
    }
}

fn month_end(year: i32, month: u32) -> NaiveDate {
    let (y, m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).expect("a valid month") - Duration::days(1)
}

fn month_span(year: i32, month: u32) -> Option<When> {
    Some(When {
        start: NaiveDate::from_ymd_opt(year, month, 1)?,
        end: month_end(year, month),
    })
}

fn year_span(year: i32) -> Option<When> {
    Some(When {
        start: NaiveDate::from_ymd_opt(year, 1, 1)?,
        end: NaiveDate::from_ymd_opt(year, 12, 31)?,
    })
}

/// `2026-10-27`, `2026-10`, `2026`, `27.10.2026`.
fn iso(text: &str) -> Option<When> {
    if let Ok(d) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Some(When::day(d));
    }
    if let Ok(d) = NaiveDate::parse_from_str(text, "%d.%m.%Y") {
        return Some(When::day(d));
    }
    let parts: Vec<&str> = text.split('-').collect();
    let num = |s: &str, len: usize| {
        (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
    };
    match parts.as_slice() {
        [y, m] => month_span(num(y, 4)? as i32, num(m, 2)?),
        [y] => {
            let y = num(y, 4)? as i32;
            (1900..=2200).contains(&y).then(|| year_span(y)).flatten()
        }
        _ => None,
    }
}

/// Month names, English and Turkish, folded to ASCII.
const MONTHS: [(&str, &str, &str); 12] = [
    ("january", "jan", "ocak"),
    ("february", "feb", "subat"),
    ("march", "mar", "mart"),
    ("april", "apr", "nisan"),
    ("may", "may", "mayis"),
    ("june", "jun", "haziran"),
    ("july", "jul", "temmuz"),
    ("august", "aug", "agustos"),
    ("september", "sep", "eylul"),
    ("october", "oct", "ekim"),
    ("november", "nov", "kasim"),
    ("december", "dec", "aralik"),
];

const WEEKDAYS: [(&str, &str, Weekday); 7] = [
    ("monday", "pazartesi", Weekday::Mon),
    ("tuesday", "sali", Weekday::Tue),
    ("wednesday", "carsamba", Weekday::Wed),
    ("thursday", "persembe", Weekday::Thu),
    ("friday", "cuma", Weekday::Fri),
    ("saturday", "cumartesi", Weekday::Sat),
    ("sunday", "pazar", Weekday::Sun),
];

fn month_of(word: &str) -> Option<u32> {
    MONTHS
        .iter()
        .position(|(en, short, tr)| word == *en || word == *short || word == *tr)
        .map(|i| i as u32 + 1)
}

fn weekday_of(word: &str) -> Option<Weekday> {
    WEEKDAYS
        .iter()
        .find(|(en, tr, _)| word == *en || word == *tr)
        .map(|(_, _, d)| *d)
}

/// Units of "N <unit> ago", in days for days and weeks; months and years are calendar steps.
enum Unit {
    Day,
    Week,
    Month,
    Year,
}

fn unit_of(word: &str) -> Option<Unit> {
    Some(match word {
        "day" | "days" | "gun" => Unit::Day,
        "week" | "weeks" | "hafta" => Unit::Week,
        "month" | "months" | "ay" => Unit::Month,
        "year" | "years" | "yil" | "sene" => Unit::Year,
        _ => return None,
    })
}

fn number_of(word: &str) -> Option<i64> {
    if let Ok(n) = word.parse::<i64>() {
        return (0..=1000).contains(&n).then_some(n);
    }
    Some(match word {
        "a" | "an" | "one" | "bir" => 1,
        "two" | "iki" => 2,
        "three" | "uc" => 3,
        "four" | "dort" => 4,
        "five" | "bes" => 5,
        "six" | "alti" => 6,
        "seven" | "yedi" => 7,
        "eight" | "sekiz" => 8,
        "nine" | "dokuz" => 9,
        "ten" | "on" => 10,
        _ => return None,
    })
}

fn months_back(anchor: NaiveDate, n: i64) -> Option<(i32, u32)> {
    let total = i64::from(anchor.year()) * 12 + i64::from(anchor.month0()) - n;
    Some((
        i32::try_from(total.div_euclid(12)).ok()?,
        u32::try_from(total.rem_euclid(12)).ok()? + 1,
    ))
}

fn week_of(day: NaiveDate) -> When {
    let start = day - Duration::days(i64::from(day.weekday().num_days_from_monday()));
    When {
        start,
        end: start + Duration::days(6),
    }
}

/// The date `text` names, said on `anchor`; `None` when it names none this understands.
pub fn resolve(text: &str, anchor: NaiveDate) -> Option<When> {
    let trimmed = text.trim();
    if let Some(w) = When::parse_display(trimmed) {
        return Some(w);
    }
    let w = tokens(trimmed);
    let words: Vec<&str> = w.iter().map(String::as_str).collect();
    let has = |x: &str| words.contains(&x);
    let pair = |a: &str, b: &str| words.windows(2).any(|p| p[0] == a && p[1] == b);
    let last = has("last") || has("gecen") || has("previous") || has("onceki");
    let this = has("this") || has("bu");
    let next = has("next") || has("gelecek") || has("onumuzdeki");

    // Single words and fixed phrases.
    if pair("day", "before") || pair("evvelsi", "gun") || pair("evvelki", "gun") || has("evvelsi") {
        return Some(When::day(anchor - Duration::days(2)));
    }
    if pair("day", "after") || has("obur") || has("ertesi") {
        return Some(When::day(anchor + Duration::days(2)));
    }
    if has("today")
        || has("bugun")
        || pair("this", "morning")
        || pair("bu", "sabah")
        || has("tonight")
        || pair("bu", "aksam")
    {
        return Some(When::day(anchor));
    }
    if has("yesterday") || has("dun") {
        return Some(When::day(anchor - Duration::days(1)));
    }
    if has("tomorrow") || has("yarin") {
        return Some(When::day(anchor + Duration::days(1)));
    }

    // "3 days ago", "iki hafta önce", "a month ago".
    if has("ago") || has("once") {
        for k in 0..words.len().saturating_sub(1) {
            let (Some(n), Some(unit)) = (number_of(words[k]), unit_of(words[k + 1])) else {
                continue;
            };
            return match unit {
                Unit::Day => Some(When::day(anchor - Duration::days(n))),
                Unit::Week => Some(week_of(anchor - Duration::days(7 * n))),
                Unit::Month => months_back(anchor, n).and_then(|(y, m)| month_span(y, m)),
                Unit::Year => year_span(anchor.year() - i32::try_from(n).ok()?),
            };
        }
    }

    // "last week", "geçen ay", "this year", "next month".
    for unit in words.iter().filter_map(|w| unit_of(w)) {
        let step: i64 = if last {
            -1
        } else if next {
            1
        } else if this {
            0
        } else {
            continue;
        };
        return match unit {
            Unit::Day => Some(When::day(anchor + Duration::days(step))),
            Unit::Week => Some(week_of(anchor + Duration::days(7 * step))),
            Unit::Month => months_back(anchor, -step).and_then(|(y, m)| month_span(y, m)),
            Unit::Year => year_span(anchor.year() + i32::try_from(step).ok()?),
        };
    }

    // A weekday: the latest one before the day it was said (the next one with "next").
    if let Some(day) = words.iter().find_map(|w| weekday_of(w)) {
        let mut d = anchor;
        loop {
            d += Duration::days(if next { 1 } else { -1 });
            if d.weekday() == day {
                return Some(When::day(d));
            }
        }
    }

    // A month, with a day and a year when they are given: "3 mart", "March 3, 2026", "in March".
    if let Some(k) = words.iter().position(|w| month_of(w).is_some()) {
        let month = month_of(words[k]).expect("found");
        let year = words
            .iter()
            .filter_map(|w| w.parse::<i32>().ok())
            .find(|y| (1900..=2200).contains(y));
        let day = [k.checked_sub(1), Some(k + 1)]
            .into_iter()
            .flatten()
            .filter_map(|i| words.get(i))
            .filter_map(|w| w.parse::<u32>().ok())
            .find(|d| (1..=31).contains(d));
        // Without a year, the latest such month up to the day it was said.
        let year = year.unwrap_or(if month > anchor.month() {
            anchor.year() - 1
        } else {
            anchor.year()
        });
        return match day {
            Some(d) => NaiveDate::from_ymd_opt(year, month, d).map(When::day),
            None => month_span(year, month),
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// A Friday.
    fn anchor() -> NaiveDate {
        d("2026-10-30")
    }

    fn r(text: &str) -> String {
        resolve(text, anchor()).map_or_else(|| "-".into(), |w| w.display())
    }

    #[test]
    fn fixed_words() {
        assert_eq!(r("today"), "2026-10-30");
        assert_eq!(r("bugün"), "2026-10-30");
        assert_eq!(r("yesterday"), "2026-10-29");
        assert_eq!(r("dün akşam"), "2026-10-29");
        assert_eq!(r("evvelsi gün"), "2026-10-28");
        assert_eq!(r("the day before yesterday"), "2026-10-28");
        assert_eq!(r("yarın"), "2026-10-31");
    }

    #[test]
    fn counted_back() {
        assert_eq!(r("3 days ago"), "2026-10-27");
        assert_eq!(r("üç gün önce"), "2026-10-27");
        assert_eq!(r("a month ago"), "2026-09");
        assert_eq!(r("2 ay önce"), "2026-08");
        assert_eq!(r("bir yıl önce"), "2025");
        assert_eq!(r("2 weeks ago"), "2026-10-12..2026-10-18");
        assert_eq!(r("11 months ago"), "2025-11");
    }

    #[test]
    fn named_periods() {
        assert_eq!(r("last week"), "2026-10-19..2026-10-25");
        assert_eq!(r("geçen hafta"), "2026-10-19..2026-10-25");
        assert_eq!(r("bu hafta"), "2026-10-26..2026-11-01");
        assert_eq!(r("geçen ay"), "2026-09");
        assert_eq!(r("last year"), "2025");
        assert_eq!(r("bu yıl"), "2026");
        assert_eq!(r("next month"), "2026-11");
    }

    #[test]
    fn weekdays_and_months() {
        assert_eq!(r("geçen salı"), "2026-10-27");
        assert_eq!(r("on Friday"), "2026-10-23", "not the day it was said");
        assert_eq!(r("next monday"), "2026-11-02");
        assert_eq!(r("mart'ta"), "2026-03");
        assert_eq!(r("in December"), "2025-12", "a later month is last year's");
        assert_eq!(r("3 mart"), "2026-03-03");
        assert_eq!(r("March 3, 2024"), "2024-03-03");
        assert_eq!(r("ekim 2025"), "2025-10");
    }

    #[test]
    fn written_dates_and_nothing() {
        assert_eq!(r("2026-10-01"), "2026-10-01");
        assert_eq!(r("2026-10"), "2026-10");
        assert_eq!(r("2025"), "2025");
        assert_eq!(r("01.10.2026"), "2026-10-01");
        assert_eq!(r("2026-10-01..2026-10-05"), "2026-10-01..2026-10-05");
        assert_eq!(r("sometime"), "-");
        assert_eq!(r("on"), "-");
        assert_eq!(r(""), "-");
    }

    #[test]
    fn display_round_trips() {
        for text in ["2026-10-30", "2026-02", "2024", "2026-10-19..2026-10-25"] {
            assert_eq!(When::parse_display(text).unwrap().display(), text);
        }
        assert_eq!(When::day(d("2026-10-20")).days_before(anchor()), 10);
    }
}
