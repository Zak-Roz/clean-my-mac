//! Version-token normalization shared by `versioned-siblings` and `redundant-archives` (owned by S2).
//!
//! A *token* is a part of a file name that identifies one version of otherwise the same thing:
//! dates (`28_01_2025`, `2025-01-28`, `20250128`), timestamps (`2025-06-20T11-00-00-030Z`,
//! `2025-04-12 14.05.31`), epoch milliseconds (`1741234567890`), versions (`v1.2.3`, `0.1.1`, `v2`)
//! and copy markers (` (1)`, ` copy`, ` copy 2`, ` - Copy`). Two names are versions of each other
//! when their [`NameInfo::key`] is equal: tokens removed, lower-cased, separator runs collapsed.

use std::sync::LazyLock;

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime};
use regex::Regex;

/// What a name looks like once its version tokens are taken out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameInfo {
    /// Comparison key: tokens removed, lower-cased, runs of `-_. ()[]`/whitespace collapsed to `_`.
    pub key: String,
    /// The original name with every token replaced by `*` (human-readable group pattern).
    pub pattern: String,
    /// The first date/timestamp token, parsed (day-first for `dd_mm_yyyy`).
    pub date: Option<NaiveDateTime>,
    /// The first version token, as numeric components (`v1.2.3` -> `[1, 2, 3]`).
    pub version: Option<Vec<u64>>,
    /// At least one token was found.
    pub has_token: bool,
    /// The part that is neither a token nor the extension contains a letter. Names like
    /// `2025-04-12 14.05.31.jpg` have none and are never grouped (every screenshot would match).
    pub has_stem_letters: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    DateTime,
    EpochMillis,
    Ymd,
    Dmy,
    Version,
    Copy,
}

struct Token {
    start: usize,
    end: usize,
    date: Option<NaiveDateTime>,
    version: Option<Vec<u64>>,
}

static DATETIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d{4})[-_.]?(\d{2})[-_.]?(\d{2})[T _-](\d{2})[-_.:]?(\d{2})[-_.:]?(\d{2})(?:[-_.:,]\d{1,9})?Z?")
        .unwrap()
});
static EPOCH_MILLIS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{13}").unwrap());
static YMD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{4})([-_.]?)(\d{2})([-_.]?)(\d{2})").unwrap());
static DMY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{1,2})([-_.])(\d{1,2})([-_.])(\d{4})").unwrap());
static VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[vV]\d+(?:\.\d+){0,3}|\d+(?:\.\d+){1,3}").unwrap());
static COPY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i) ?\(\d{1,3}\)| - copy(?: \(\d{1,3}\))?| copy(?: \d{1,3})?").unwrap());

const KINDS: [TokenKind; 6] =
    [TokenKind::DateTime, TokenKind::EpochMillis, TokenKind::Ymd, TokenKind::Dmy, TokenKind::Version, TokenKind::Copy];

fn regex_for(kind: TokenKind) -> &'static Regex {
    match kind {
        TokenKind::DateTime => &DATETIME,
        TokenKind::EpochMillis => &EPOCH_MILLIS,
        TokenKind::Ymd => &YMD,
        TokenKind::Dmy => &DMY,
        TokenKind::Version => &VERSION,
        TokenKind::Copy => &COPY,
    }
}

fn plausible_year(y: i32) -> bool {
    (1990..=2100).contains(&y)
}

fn num<T: std::str::FromStr>(caps: &regex::Captures, i: usize) -> Option<T> {
    caps.get(i)?.as_str().parse().ok()
}

fn date(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
    if plausible_year(y) { NaiveDate::from_ymd_opt(y, m, d) } else { None }
}

/// Validates and parses one regex match. `None` rejects the match.
fn parse_token(kind: TokenKind, name: &str, caps: &regex::Captures) -> Option<Token> {
    let m = caps.get(0)?;
    let (start, end) = (m.start(), m.end());
    let before = name[..start].chars().next_back();
    let after = name[end..].chars().next();
    let digit_free = !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit());
    let alnum_free =
        !before.is_some_and(|c| c.is_ascii_alphanumeric()) && !after.is_some_and(|c| c.is_ascii_alphanumeric());
    let token = |date, version| Some(Token { start, end, date, version });
    match kind {
        TokenKind::DateTime => {
            if !digit_free {
                return None;
            }
            let day = date(num(caps, 1)?, num(caps, 2)?, num(caps, 3)?)?;
            let time = NaiveTime::from_hms_opt(num(caps, 4)?, num(caps, 5)?, num(caps, 6)?)?;
            token(Some(day.and_time(time)), None)
        }
        TokenKind::EpochMillis => {
            let ms: i64 = m.as_str().parse().ok()?;
            let at = DateTime::from_timestamp_millis(ms)?.naive_utc();
            if !digit_free || !(2015..=2040).contains(&at.year()) {
                return None;
            }
            token(Some(at), None)
        }
        TokenKind::Ymd => {
            if !digit_free || caps.get(2)?.as_str() != caps.get(4)?.as_str() {
                return None;
            }
            let day = date(num(caps, 1)?, num(caps, 3)?, num(caps, 5)?)?;
            token(Some(day.and_time(NaiveTime::MIN)), None)
        }
        TokenKind::Dmy => {
            if !digit_free || caps.get(2)?.as_str() != caps.get(4)?.as_str() {
                return None;
            }
            let (a, b, y): (u32, u32, i32) = (num(caps, 1)?, num(caps, 3)?, num(caps, 5)?);
            // Day-first (`28_01_2025`); month-first only when the second field cannot be a month.
            let day = if b > 12 { date(y, a, b)? } else { date(y, b, a)? };
            token(Some(day.and_time(NaiveTime::MIN)), None)
        }
        TokenKind::Version => {
            if !alnum_free {
                return None;
            }
            let parts: Option<Vec<u64>> =
                m.as_str().trim_start_matches(['v', 'V']).split('.').map(|p| p.parse().ok()).collect();
            token(None, Some(parts?))
        }
        TokenKind::Copy => {
            // Must end the stem: followed by nothing, an extension dot or whitespace.
            if after.is_some_and(|c| c != '.' && !c.is_whitespace()) {
                return None;
            }
            token(None, None)
        }
    }
}

fn find_tokens(name: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    for kind in KINDS {
        for caps in regex_for(kind).captures_iter(name) {
            let Some(m) = caps.get(0) else { continue };
            if tokens.iter().any(|t| m.start() < t.end && t.start < m.end()) {
                continue;
            }
            if let Some(token) = parse_token(kind, name, &caps) {
                tokens.push(token);
            }
        }
    }
    tokens.sort_by_key(|t| t.start);
    tokens
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '_' | '.' | '(' | ')' | '[' | ']' | ',')
}

/// Splits a trailing `.ext` (1–6 alphanumerics) off `name`, if there is a non-empty stem.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty() && (1..=6).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            (stem, ext)
        }
        _ => (name, ""),
    }
}

/// Normalizes a file or directory name (see the module docs).
pub fn name_info(name: &str) -> NameInfo {
    let tokens = find_tokens(name);
    let mut stripped = String::with_capacity(name.len());
    let mut pattern = String::with_capacity(name.len());
    let mut last = 0;
    for t in &tokens {
        stripped.push_str(&name[last..t.start]);
        stripped.push('_');
        pattern.push_str(&name[last..t.start]);
        pattern.push('*');
        last = t.end;
    }
    stripped.push_str(&name[last..]);
    pattern.push_str(&name[last..]);

    let key = stripped.to_lowercase().split(is_separator).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("_");
    let has_stem_letters = split_extension(&stripped).0.chars().any(char::is_alphabetic);
    NameInfo {
        key,
        pattern,
        date: tokens.iter().find_map(|t| t.date),
        version: tokens.iter().find_map(|t| t.version.clone()),
        has_token: !tokens.is_empty(),
        has_stem_letters,
    }
}

/// Comparison key for a name, ignoring version tokens (`Archive_0.1.1_mac` -> `archive_mac`).
pub fn comparison_key(name: &str) -> String {
    name_info(name).key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, day).unwrap().and_time(NaiveTime::MIN)
    }

    #[test]
    fn mongo_and_mongo_eu_are_different_keys() {
        let a = name_info("mongo_28_01_2025");
        let b = name_info("mongo_11_08_2025");
        let eu = name_info("mongo_eu_11_08_2025");
        assert_eq!(a.key, "mongo");
        assert_eq!(a.key, b.key);
        assert_eq!(eu.key, "mongo_eu");
        assert_eq!(a.pattern, "mongo_*");
        assert_eq!(eu.pattern, "mongo_eu_*");
        assert!(a.has_stem_letters && a.has_token);
        // dd_mm_yyyy is day-first.
        assert_eq!(a.date, Some(d(2025, 1, 28)));
        assert_eq!(b.date, Some(d(2025, 8, 11)));
        assert_eq!(name_info("mongo_05_03_2025").date, Some(d(2025, 3, 5)));
    }

    #[test]
    fn iso_timestamps() {
        let a = name_info("db-backup-dev-2025-06-20T11-00-00-030Z.dump");
        let b = name_info("db-backup-dev-2025-02-03T15-00-00-008Z.dump");
        let prod = name_info("db-backup-prod-2025-02-12T07-00-06-303Z.dump");
        assert_eq!(a.key, "db_backup_dev_dump");
        assert_eq!(a.key, b.key);
        assert_ne!(a.key, prod.key);
        assert_eq!(a.pattern, "db-backup-dev-*.dump");
        assert_eq!(a.date, Some(d(2025, 6, 20).date().and_hms_opt(11, 0, 0).unwrap()));
        assert!(a.date > b.date);
    }

    #[test]
    fn ymd_compact_and_dotted_dates() {
        assert_eq!(name_info("report-2025-01-28.pdf").date, Some(d(2025, 1, 28)));
        assert_eq!(name_info("report_20250128.pdf").date, Some(d(2025, 1, 28)));
        assert_eq!(name_info("report-2025-01-28.pdf").key, name_info("report_20250128.pdf").key);
        assert_eq!(name_info("photo-export-FINAL-2025-04-03.zip").key, "photo_export_final_zip");
        // Mixed separators are not a date.
        assert!(name_info("x-2025_01-28").date.is_none());
        // Implausible years (birth dates, ids) are not tokens.
        assert!(!name_info("DOE_J_A_01_01_1970.rar").has_token);
    }

    #[test]
    fn screenshots_have_no_stem_letters() {
        let a = name_info("2025-04-12 14.05.31.jpg");
        let b = name_info("2025-04-12 14.11.09.jpg");
        assert_eq!(a.key, b.key);
        assert!(!a.has_stem_letters);
        assert!(!name_info("2025_03_22_093114.JPG").has_stem_letters);
    }

    #[test]
    fn versions_and_copies() {
        assert_eq!(comparison_key("Archive_0.1.1_mac"), "archive_mac");
        assert_eq!(comparison_key("Archive_0.1.1_mac"), comparison_key("Archive_mac"));
        assert_eq!(name_info("Transfer-1.17.0.dmg").version, Some(vec![1, 17, 0]));
        assert_eq!(name_info("tool-v2.zip").version, Some(vec![2]));
        assert_eq!(comparison_key("Signed Forms (1).zip"), comparison_key("Signed Forms.zip"));
        assert_eq!(comparison_key("Notes copy.txt"), comparison_key("Notes.txt"));
        assert_eq!(comparison_key("Notes copy 2.txt"), comparison_key("Notes.txt"));
        assert_eq!(comparison_key("Notes - Copy.txt"), comparison_key("Notes.txt"));
        // Not tokens: glued to letters, plain numbers, words containing "copy".
        assert!(!name_info("Toolbox7.50.1.zip").has_token);
        assert!(!name_info("Logo 2.zip").has_token);
        assert!(!name_info("copyright.txt").has_token);
        assert!(!name_info("2.5mm-screws.pdf").has_token);
    }

    #[test]
    fn epoch_millis() {
        let a = name_info("session-export-1741234567890.zip");
        let b = name_info("session-export-1744567890123.zip");
        assert_eq!(a.key, b.key);
        assert!(a.date < b.date);
        assert!(!name_info("id-9999999999999.txt").has_token, "year > 2040");
    }

    #[test]
    fn plain_names_are_their_own_key() {
        let n = name_info("Figma.dmg");
        assert_eq!(n.key, "figma_dmg");
        assert!(!n.has_token);
        assert_eq!(n.pattern, "Figma.dmg");
    }
}
