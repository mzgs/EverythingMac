use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};
use regex::{Regex, RegexBuilder};
use std::{path::Path, time::SystemTime};

#[derive(Debug)]
pub(crate) struct Query(Expr, Option<Regex>);

#[derive(Debug)]
enum Expr {
    All,
    Text(Regex, bool),
    Fuzzy(Regex, Regex),
    Extension(Regex),
    Kind(bool),
    Size(u64, u64),
    Modified(Option<SystemTime>, Option<SystemTime>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

impl Query {
    pub(crate) fn parse(
        text: &str,
        match_case: bool,
        match_path: bool,
        regex_mode: bool,
        fuzzy: bool,
    ) -> Result<Self, String> {
        if text.len() > 4096 {
            return Err("Search is too long (maximum 4096 bytes).".into());
        }
        if text.trim().is_empty() {
            return Ok(Self(Expr::All, None));
        }
        if regex_mode {
            return Ok(Self(
                Expr::Text(compile(text, match_case)?, match_path || text.contains('/')),
                None,
            ));
        }
        let mut parser = Parser {
            tokens: tokenize(text)?,
            pos: 0,
            match_case,
            match_path,
            fuzzy: fuzzy && !match_case,
        };
        let phrase = if !match_path && parser.tokens.len() > 1 {
            parser
                .tokens
                .iter()
                .map(|token| match token {
                    Token::Word(word, _) if !word.contains([':', '/', '*', '?']) => {
                        Some(word.as_str())
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .map(|words| compile(&regex::escape(&words.join(" ")), match_case))
                .transpose()?
        } else {
            None
        };
        let expr = parser.or(0)?;
        if parser.pos != parser.tokens.len() {
            return Err("Unexpected '>'. Check search grouping.".into());
        }
        Ok(Self(expr, phrase))
    }

    pub(crate) fn matches(
        &self,
        path: &Path,
        is_dir: bool,
        size: u64,
        modified: Option<SystemTime>,
    ) -> bool {
        self.0.matches(path, is_dir, size, modified)
    }

    pub(crate) fn rank(
        &self,
        path: &Path,
        is_dir: bool,
        size: u64,
        modified: Option<SystemTime>,
    ) -> u8 {
        let rank = self
            .0
            .rank(path, is_dir, size, modified)
            .filter(|rank| *rank > 0)
            .unwrap_or(5);
        self.1
            .as_ref()
            .and_then(|phrase| text_rank(phrase, path))
            .map_or(rank, |phrase_rank| rank.min(phrase_rank))
    }
}

impl Expr {
    fn matches(&self, path: &Path, is_dir: bool, size: u64, modified: Option<SystemTime>) -> bool {
        match self {
            Self::All => true,
            Self::Text(regex, full_path) => {
                let text = if *full_path {
                    path.as_os_str()
                } else {
                    path.file_name().unwrap_or(path.as_os_str())
                };
                regex.is_match(&text.to_string_lossy())
            }
            Self::Fuzzy(_, regex) => regex.is_match(
                &path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy(),
            ),
            Self::Extension(regex) => {
                !is_dir
                    && regex.is_match(
                        &path
                            .file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy(),
                    )
            }
            Self::Kind(directory) => is_dir == *directory,
            Self::Size(min, max) => !is_dir && size >= *min && size <= *max,
            Self::Modified(min, max) => modified.is_some_and(|time| {
                min.is_none_or(|min| time >= min) && max.is_none_or(|max| time < max)
            }),
            Self::And(items) => items
                .iter()
                .all(|item| item.matches(path, is_dir, size, modified)),
            Self::Or(items) => items
                .iter()
                .any(|item| item.matches(path, is_dir, size, modified)),
            Self::Not(item) => !item.matches(path, is_dir, size, modified),
        }
    }

    fn rank(
        &self,
        path: &Path,
        is_dir: bool,
        size: u64,
        modified: Option<SystemTime>,
    ) -> Option<u8> {
        match self {
            Self::Text(regex, false) | Self::Fuzzy(regex, _) => {
                if let Some(rank) = text_rank(regex, path) {
                    Some(rank)
                } else if let Self::Fuzzy(_, approximate) = self {
                    approximate
                        .is_match(
                            &path
                                .file_name()
                                .unwrap_or(path.as_os_str())
                                .to_string_lossy(),
                        )
                        .then_some(4)
                } else {
                    None
                }
            }
            // A conjunction is only as strong as its weakest filename term.
            Self::And(items) => items.iter().try_fold(0, |rank, item| {
                Some(rank.max(item.rank(path, is_dir, size, modified)?))
            }),
            Self::Or(items) => items
                .iter()
                .filter_map(|item| item.rank(path, is_dir, size, modified))
                .min_by_key(|rank| if *rank == 0 { 5 } else { *rank }),
            // Predicates, paths and exclusions filter results without changing relevance.
            _ => self.matches(path, is_dir, size, modified).then_some(0),
        }
    }
}

fn text_rank(regex: &Regex, path: &Path) -> Option<u8> {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    regex.find(&name).map(|found| {
        if found.start() == 0 {
            if found.end() == name.len() { 1 } else { 2 }
        } else {
            3
        }
    })
}

fn compile(pattern: &str, match_case: bool) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(!match_case)
        .size_limit(2 * 1024 * 1024)
        .build()
        .map_err(|error| format!("Invalid search pattern: {error}"))
}

#[derive(Debug, PartialEq)]
enum Token {
    Word(String, bool),
    Or,
    Not,
    Open,
    Close,
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(&ch) = chars.peek() {
        let token = match ch {
            ch if ch.is_whitespace() => {
                chars.next();
                continue;
            }
            '|' => Token::Or,
            '!' => Token::Not,
            '<' => Token::Open,
            '>' => Token::Close,
            _ => {
                let literal = ch == '"';
                let mut value = String::new();
                let mut quoted = false;
                while let Some(&ch) = chars.peek() {
                    if ch == '"' {
                        quoted = !quoted;
                    } else if !quoted
                        && (ch.is_whitespace()
                            || matches!(ch, '|' | '!')
                            || (matches!(ch, '<' | '>')
                                && !["size:", "modified:", "dm:"]
                                    .iter()
                                    .any(|key| value.eq_ignore_ascii_case(key))))
                    {
                        break;
                    } else {
                        value.push(ch);
                    }
                    chars.next();
                }
                if quoted {
                    return Err("Unclosed quote in search.".into());
                }
                if value.is_empty() {
                    return Err("Search terms cannot be empty.".into());
                }
                tokens.push(Token::Word(value, literal));
                continue;
            }
        };
        chars.next();
        tokens.push(token);
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    match_case: bool,
    match_path: bool,
    fuzzy: bool,
}

impl Parser {
    fn or(&mut self, depth: usize) -> Result<Expr, String> {
        let mut items = vec![self.and(depth)?];
        while self.tokens.get(self.pos) == Some(&Token::Or) {
            self.pos += 1;
            items.push(self.and(depth)?);
        }
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            Expr::Or(items)
        })
    }

    fn and(&mut self, depth: usize) -> Result<Expr, String> {
        let mut items = Vec::new();
        while self
            .tokens
            .get(self.pos)
            .is_some_and(|token| !matches!(token, Token::Or | Token::Close))
        {
            items.push(self.term(depth)?);
        }
        match items.len() {
            0 => Err("Expected a search term beside an operator or inside a group.".into()),
            1 => Ok(items.remove(0)),
            _ => Ok(Expr::And(items)),
        }
    }

    fn term(&mut self, depth: usize) -> Result<Expr, String> {
        if depth >= 32 {
            return Err("Search grouping is too deep (maximum 32 levels).".into());
        }
        let token = self
            .tokens
            .get(self.pos)
            .ok_or("Expected a term after '!'.")?;
        self.pos += 1;
        match token {
            Token::Not => Ok(Expr::Not(Box::new(self.term(depth + 1)?))),
            Token::Open => {
                let expr = self.or(depth + 1)?;
                if self.tokens.get(self.pos) != Some(&Token::Close) {
                    return Err("Unclosed '<' group in search.".into());
                }
                self.pos += 1;
                Ok(expr)
            }
            Token::Word(text, literal) => self.word(text, *literal),
            _ => Err("Expected a search term.".into()),
        }
    }

    fn word(&self, text: &str, literal: bool) -> Result<Expr, String> {
        if !literal && let Some((key, value)) = text.split_once(':') {
            match key.to_ascii_lowercase().as_str() {
                "file" | "folder" => {
                    let kind = Expr::Kind(key.eq_ignore_ascii_case("folder"));
                    return if value.is_empty() {
                        Ok(kind)
                    } else {
                        Ok(Expr::And(vec![
                            kind,
                            self.text(value, self.match_path || value.contains('/'))?,
                        ]))
                    };
                }
                "path" | "name" => {
                    if value.is_empty() {
                        return Err(format!(
                            "{key}: needs text, for example {key}:\"my files\"."
                        ));
                    }
                    return self.text(value, key.eq_ignore_ascii_case("path"));
                }
                "ext" => {
                    let extensions: Vec<_> = value
                        .split(';')
                        .map(|ext| ext.trim_start_matches('.'))
                        .collect();
                    if extensions
                        .iter()
                        .any(|ext| ext.is_empty() || ext.contains('/'))
                    {
                        return Err("ext: needs extensions, for example ext:rs;swift.".into());
                    }
                    let pattern = format!(
                        r"\.({})$",
                        extensions
                            .iter()
                            .map(|ext| regex::escape(ext))
                            .collect::<Vec<_>>()
                            .join("|")
                    );
                    return Ok(Expr::Extension(compile(&pattern, self.match_case)?));
                }
                "size" => return size_predicate(value),
                "modified" | "dm" => return modified_predicate(value, Local::now().date_naive()),
                _ if !key.is_empty() && key.chars().all(|ch| ch.is_ascii_alphabetic()) => {
                    return Err(format!(
                        "Unsupported search predicate '{key}:'. Quote the whole term to search a filename containing ':'."
                    ));
                }
                _ => {}
            }
        }
        let full_path = self.match_path || text.contains('/');
        if self.fuzzy && !literal && !full_path && !text.contains(['*', '?']) {
            let chars: Vec<_> = text.chars().collect();
            // shortcut: one typo in 4–64 character plain terms; expand only if broader matching is needed.
            if (4..=64).contains(&chars.len()) {
                let escaped: Vec<_> = chars
                    .iter()
                    .map(|ch| regex::escape(&ch.to_string()))
                    .collect();
                let mut patterns = vec![regex::escape(text)];
                for i in 0..chars.len() {
                    let before = escaped[..i].concat();
                    let after = escaped[i + 1..].concat();
                    patterns.push(format!("{before}(?s:.)?{after}"));
                    patterns.push(format!("{before}(?s:.){}", escaped[i..].concat()));
                    if i + 1 < chars.len() {
                        patterns.push(format!(
                            "{before}{}{}{}",
                            escaped[i + 1],
                            escaped[i],
                            escaped[i + 2..].concat()
                        ));
                    }
                }
                return Ok(Expr::Fuzzy(
                    compile(&regex::escape(text), self.match_case)?,
                    compile(&format!("(?:{})", patterns.join("|")), self.match_case)?,
                ));
            }
        }
        self.text(text, full_path)
    }

    fn text(&self, text: &str, full_path: bool) -> Result<Expr, String> {
        let wildcard = text.contains(['*', '?']);
        let mut pattern = String::new();
        if wildcard && !full_path {
            pattern.push('^');
        }
        for ch in text.chars() {
            match ch {
                '*' => pattern.push_str(r"[\s\S]*"),
                '?' => pattern.push_str(r"[\s\S]"),
                _ => pattern.push_str(&regex::escape(&ch.to_string())),
            }
        }
        if wildcard && !full_path {
            pattern.push('$');
        }
        Ok(Expr::Text(compile(&pattern, self.match_case)?, full_path))
    }
}

fn modified_predicate(value: &str, today: NaiveDate) -> Result<Expr, String> {
    let invalid = || {
        "Invalid modified date. Use today, yesterday, thisweek, last7days, YYYY-MM-DD, a comparison, or an inclusive date range.".to_owned()
    };
    let parse = |text: &str| {
        if text.len() != 10 {
            return Err(invalid());
        }
        NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| invalid())
    };
    let next = |day: NaiveDate| day.succ_opt().ok_or_else(invalid);
    let bounds = |start: Option<NaiveDate>, end: Option<NaiveDate>| {
        let midnight = |day: NaiveDate| -> Result<SystemTime, String> {
            Local
                .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
                .earliest()
                .map(SystemTime::from)
                .ok_or_else(invalid)
        };
        Ok(Expr::Modified(
            start.map(midnight).transpose()?,
            end.map(midnight).transpose()?,
        ))
    };
    match value.to_ascii_lowercase().as_str() {
        "today" => return bounds(Some(today), Some(next(today)?)),
        "yesterday" => return bounds(Some(today.pred_opt().ok_or_else(invalid)?), Some(today)),
        "thisweek" => {
            let monday = today
                .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
                .ok_or_else(invalid)?;
            return bounds(
                Some(monday),
                Some(monday.checked_add_days(Days::new(7)).ok_or_else(invalid)?),
            );
        }
        "last7days" => {
            return bounds(
                Some(today.checked_sub_days(Days::new(6)).ok_or_else(invalid)?),
                Some(next(today)?),
            );
        }
        _ => {}
    }
    if let Some((min, max)) = value.split_once("..") {
        let min = parse(min)?;
        let max = parse(max)?;
        if min > max {
            return Err("Modified date range minimum exceeds maximum.".into());
        }
        return bounds(Some(min), Some(next(max)?));
    }
    for operator in [">=", "<=", ">", "<", "="] {
        if let Some(value) = value.strip_prefix(operator) {
            let day = parse(value)?;
            return match operator {
                ">=" => bounds(Some(day), None),
                ">" => bounds(Some(next(day)?), None),
                "<=" => bounds(None, Some(next(day)?)),
                "<" => bounds(None, Some(day)),
                _ => bounds(Some(day), Some(next(day)?)),
            };
        }
    }
    let day = parse(value)?;
    bounds(Some(day), Some(next(day)?))
}

fn size_predicate(value: &str) -> Result<Expr, String> {
    if let Some((min, max)) = value.split_once("..") {
        let min = bytes(min)?;
        let max = bytes(max)?;
        if min > max {
            return Err("Size range minimum exceeds maximum.".into());
        }
        return Ok(Expr::Size(min, max));
    }
    for operator in [">=", "<=", ">", "<", "="] {
        if let Some(number) = value.strip_prefix(operator) {
            let size = bytes(number)?;
            return match operator {
                ">=" => Ok(Expr::Size(size, u64::MAX)),
                "<=" => Ok(Expr::Size(0, size)),
                ">" => Ok(size
                    .checked_add(1)
                    .map_or(Expr::Size(1, 0), |min| Expr::Size(min, u64::MAX))),
                "<" => Ok(size
                    .checked_sub(1)
                    .map_or(Expr::Size(1, 0), |max| Expr::Size(0, max))),
                _ => Ok(Expr::Size(size, size)),
            };
        }
    }
    let size = bytes(value)?;
    Ok(Expr::Size(size, size))
}

fn bytes(text: &str) -> Result<u64, String> {
    let invalid = || {
        "Invalid size. Use a nonnegative number with B, KB, MB, GB, or TB (1024-based).".to_owned()
    };
    let split = text
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1u64,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        "t" | "tb" | "tib" => 1024u64.pow(4),
        _ => return Err(invalid()),
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.chars().all(|ch| ch.is_ascii_digit())
        || !fraction.chars().all(|ch| ch.is_ascii_digit())
        || fraction.len() > 9
    {
        return Err(invalid());
    }
    let whole = whole
        .parse::<u64>()
        .map_err(|_| invalid())?
        .checked_mul(multiplier)
        .ok_or_else(invalid)?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<u64>()
            .map_err(|_| invalid())?
            .checked_mul(multiplier)
            .ok_or_else(invalid)?
            / 10u64.pow(fraction.len() as u32)
    };
    whole.checked_add(fraction).ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(query: &str, path: &str) -> bool {
        Query::parse(query, false, false, false, false)
            .unwrap()
            .matches(Path::new(path), false, 0, None)
    }

    #[test]
    fn boolean_precedence_quotes_and_unicode() {
        assert!(matches("apple | pear red", "/fruit/apple-green"));
        assert!(!matches("apple | pear red", "/fruit/pear-green"));
        assert!(matches("<apple | pear> !red", "/fruit/pear-green"));
        assert!(!matches("<apple | pear> !red", "/fruit/apple-red"));
        assert!(matches("\"Résumé final\" !draft", "/docs/RÉSUMÉ FINAL.pdf"));
        assert!(matches("\"a:b\"", "/docs/a:b.txt"));
        assert!(matches("!!hello", "/hello"));
    }

    #[test]
    fn scope_case_regex_and_wildcards() {
        assert!(!matches("docs", "/docs/report.txt"));
        assert!(matches("/docs/", "/docs/report.txt"));
        assert!(matches("path:docs name:report", "/docs/report.txt"));
        assert!(matches("*.rs", "/src/main.rs"));
        assert!(matches("*.rs", "/src/main\nfile.rs"));
        assert!(matches("file:/src/", "/src/main.rs"));
        assert!(!matches("main?", "/src/main.rs"));
        assert!(matches("main.??", "/src/main.rs"));
        assert!(matches("ext:rs;swift", "/src/main.SWIFT"));
        assert!(
            !Query::parse("MAIN", true, false, false, false)
                .unwrap()
                .matches(Path::new("main.rs"), false, 0, None)
        );
        assert!(
            Query::parse("src", false, true, false, false)
                .unwrap()
                .matches(Path::new("/src/main.rs"), false, 0, None)
        );
        assert!(
            Query::parse(r"^(main|lib)\.rs$", false, false, true, false)
                .unwrap()
                .matches(Path::new("/src/lib.rs"), false, 0, None)
        );
    }

    #[test]
    fn kinds_sizes_and_ranges() {
        let path = Path::new("/src/main.rs");
        for (query, directory, size, expected) in [
            ("file:", false, 0, true),
            ("file:main", false, 0, true),
            ("folder:", true, 0, true),
            ("file:", true, 0, false),
            ("ext:rs", true, 0, false),
            ("size:>10kb", false, 10240, false),
            ("size:>=10kb", false, 10240, true),
            ("size:<10kb", false, 10239, true),
            ("size:<=10kb", false, 10240, true),
            ("size:1kb..10kb", false, 1024, true),
            ("size:1kb..10kb", false, 10241, false),
            ("size:1.5kb", false, 1536, true),
            ("size:0", false, 0, true),
            ("size:<0", false, 0, false),
            ("size:>18446744073709551615", false, u64::MAX, false),
            ("size:>=0", true, 10240, false),
        ] {
            assert_eq!(
                Query::parse(query, false, false, false, false)
                    .unwrap()
                    .matches(path, directory, size, None),
                expected,
                "{query}"
            );
        }
    }

    #[test]
    fn invalid_queries_are_errors() {
        for query in [
            "a |",
            "| a",
            "a || b",
            "!",
            "<a",
            "a>",
            "<>",
            "\"a",
            "\"\"",
            "ext:",
            "name:",
            "size:wat",
            "size:2kb..1kb",
            "size:18446744073709551616",
            "date:today",
        ] {
            assert!(
                Query::parse(query, false, false, false, false).is_err(),
                "{query}"
            );
        }
        assert!(Query::parse(&"!".repeat(40), false, false, false, false).is_err());
        assert!(Query::parse(&"a".repeat(4097), false, false, false, false).is_err());
        assert!(Query::parse("[", false, false, true, false).is_err());
        assert!(matches(" ", "/anything"));
    }

    #[test]
    fn modified_dates_use_local_calendar_days_and_inclusive_ranges() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let at = |day, hour| {
            SystemTime::from(
                Local
                    .with_ymd_and_hms(2026, 10, day, hour, 0, 0)
                    .earliest()
                    .unwrap(),
            )
        };
        for (query, time, expected) in [
            ("today", at(19, 0), true),
            ("today", at(19, 23), true),
            ("today", at(20, 0), false),
            ("today", at(18, 23), false),
            ("yesterday", at(18, 12), true),
            ("yesterday", at(19, 0), false),
            ("thisweek", at(19, 0), true),
            ("thisweek", at(18, 23), false),
            ("thisweek", at(25, 23), true),
            ("thisweek", at(26, 0), false),
            ("last7days", at(13, 0), true),
            ("last7days", at(12, 23), false),
            ("2026-10-18..2026-10-19", at(19, 23), true),
            ("2026-10-18..2026-10-19", at(20, 0), false),
            (">2026-10-18", at(19, 0), true),
            (">2026-10-18", at(18, 23), false),
            (">=2026-10-19", at(19, 0), true),
            ("<2026-10-19", at(19, 0), false),
            ("<=2026-10-19", at(19, 23), true),
            ("=2026-10-19", at(19, 12), true),
        ] {
            let expr = modified_predicate(query, today).unwrap();
            assert_eq!(
                expr.matches(Path::new("/report.pdf"), false, 0, Some(time)),
                expected,
                "{query}"
            );
            assert!(!expr.matches(Path::new("/unknown"), false, 0, None));
        }
        for query in [
            "modified:>=2026-10-19 ext:pdf",
            "dm:<2026-10-20",
            "<dm:2026-10-19 | dm:2026-10-20> !draft",
        ] {
            assert!(
                Query::parse(query, false, false, false, false)
                    .unwrap()
                    .matches(Path::new("/report.pdf"), false, 0, Some(at(19, 12))),
                "{query}"
            );
        }
        for query in [
            "modified:",
            "modified:2026-02-30",
            "modified:tomorrow",
            "dm:2026-10-20..2026-10-19",
            "dm:2026-1-1",
        ] {
            assert!(
                Query::parse(query, false, false, false, false).is_err(),
                "{query}"
            );
        }
        let leap = modified_predicate("2024-02-29", today).unwrap();
        assert!(
            leap.matches(
                Path::new("/file"),
                false,
                0,
                Some(SystemTime::from(
                    Local
                        .with_ymd_and_hms(2024, 2, 29, 12, 0, 0)
                        .earliest()
                        .unwrap()
                ))
            )
        );
    }

    #[test]
    fn best_match_ranks_filename_terms_without_ranking_filters() {
        for (text, name, case, expected) in [
            ("annual", "annual", false, 1),
            ("annual", "ANNUAL", false, 1),
            ("annual", "annual-report.pdf", false, 2),
            ("annual", "old-annual.pdf", false, 3),
            ("annual", "anual.pdf", false, 4),
            ("annual", "Annual", true, 5),
            ("résumé", "RÉSUMÉ", false, 1),
            ("résumé", "réumé.pdf", false, 4),
            ("annual report", "annual report", false, 1),
            ("annual report", "annual report.pdf", false, 2),
            ("annual report", "old-annual-report.pdf", false, 3),
            ("annual report", "annual_reprot.pdf", false, 4),
            ("\"annual report\" ext:pdf", "annual report.pdf", false, 2),
            ("name:annual ext:pdf size:>1", "annual.pdf", false, 2),
            ("annual !backup", "annual.pdf", false, 2),
            ("<annual | report> ext:pdf", "report.pdf", false, 2),
            ("<annual ext:txt> | report", "annual.pdf", false, 5),
            ("annual | file:", "annual", false, 1),
            ("annual | file:", "other.pdf", false, 5),
            ("ext:pdf", "annual.pdf", false, 5),
            ("path:docs annual", "/docs/annual.pdf", false, 2),
            ("path:annual", "/annual/report.pdf", false, 5),
            ("annual*", "annual-report.pdf", false, 1),
            ("", "annual", false, 5),
        ] {
            let query = Query::parse(text, case, false, false, true).unwrap();
            assert_eq!(
                query.rank(Path::new(name), false, 20, None),
                expected,
                "{text}: {name}"
            );
            if expected < 5 {
                assert!(query.matches(Path::new(name), false, 20, None));
            }
        }
        let query = Query::parse("annual", false, false, false, false).unwrap();
        assert_eq!(query.rank(Path::new("anual.pdf"), false, 0, None), 5);
        assert!(
            !Query::parse("ext:pdf", false, false, false, false)
                .unwrap()
                .matches(Path::new("folder.pdf"), true, 0, None)
        );
    }

    #[test]
    fn case_sensitive_search_does_not_treat_case_differences_as_typos() {
        for (text, name, expected) in [
            ("Annual", "annual.pdf", false),
            ("annual", "Annual.pdf", false),
            ("annual", "annUal.pdf", false),
            ("annual", "annuaL.pdf", false),
            ("Résumé", "résumé.pdf", false),
            ("annual", "annual.pdf", true),
            ("Annual", "Annual.pdf", true),
            ("annual", "anual.pdf", false),
            ("!Annual", "annual.pdf", true),
        ] {
            let query = Query::parse(text, true, false, false, true).unwrap();
            assert_eq!(
                query.matches(Path::new(name), false, 0, None),
                expected,
                "{text}: {name}"
            );
        }
    }

    #[test]
    fn fuzzy_matches_one_unicode_typo_but_preserves_exact_syntax() {
        for (query, name, expected) in [
            ("anual report", "annual_report.pdf", true),
            ("annual", "anual.pdf", true),
            ("annual", "annxual.pdf", true),
            ("annual", "annxal.pdf", true),
            ("annual", "annau l.pdf", false),
            ("annual", "annula.pdf", true),
            ("annual", "axyual.pdf", false),
            ("résumé", "réumé.pdf", true),
            ("RÉSUMÉ", "résumé.pdf", true),
            ("cat", "cut.txt", false),
            ("\"annual\"", "anual.pdf", false),
            ("annual*", "anual.pdf", false),
            ("ext:pdf", "file.pxf", false),
            ("name:annual", "anual.pdf", false),
            ("!annual", "anual.pdf", false),
        ] {
            assert_eq!(
                Query::parse(query, false, false, false, true)
                    .unwrap()
                    .matches(Path::new(name), false, 0, None),
                expected,
                "{query}: {name}"
            );
        }
        assert!(
            !Query::parse("ANNUAL", true, false, false, true)
                .unwrap()
                .matches(Path::new("annual.pdf"), false, 0, None)
        );
        assert!(
            !Query::parse("anual", false, true, false, true)
                .unwrap()
                .matches(Path::new("/annual/report.pdf"), false, 0, None)
        );
        let longest = "a".repeat(64);
        assert!(
            Query::parse(&longest, false, false, false, true)
                .unwrap()
                .matches(Path::new(&"a".repeat(63)), false, 0, None)
        );
        assert!(
            !Query::parse(&"a".repeat(65), false, false, false, true)
                .unwrap()
                .matches(Path::new(&longest), false, 0, None)
        );
    }
}
