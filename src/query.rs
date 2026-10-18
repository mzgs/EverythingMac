use regex::{Regex, RegexBuilder};
use std::path::Path;

#[derive(Debug)]
pub(crate) struct Query(Expr);

#[derive(Debug)]
enum Expr {
    All,
    Text(Regex, bool),
    Kind(bool),
    Size(u64, u64),
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
    ) -> Result<Self, String> {
        if text.len() > 4096 {
            return Err("Search is too long (maximum 4096 bytes).".into());
        }
        if text.trim().is_empty() {
            return Ok(Self(Expr::All));
        }
        if regex_mode {
            return Ok(Self(Expr::Text(
                compile(text, match_case)?,
                match_path || text.contains('/'),
            )));
        }
        let mut parser = Parser {
            tokens: tokenize(text)?,
            pos: 0,
            match_case,
            match_path,
        };
        let expr = parser.or(0)?;
        if parser.pos != parser.tokens.len() {
            return Err("Unexpected '>'. Check search grouping.".into());
        }
        Ok(Self(expr))
    }

    pub(crate) fn matches(&self, path: &Path, is_dir: bool, size: u64) -> bool {
        self.0.matches(path, is_dir, size)
    }
}

impl Expr {
    fn matches(&self, path: &Path, is_dir: bool, size: u64) -> bool {
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
            Self::Kind(directory) => is_dir == *directory,
            Self::Size(min, max) => !is_dir && size >= *min && size <= *max,
            Self::And(items) => items.iter().all(|item| item.matches(path, is_dir, size)),
            Self::Or(items) => items.iter().any(|item| item.matches(path, is_dir, size)),
            Self::Not(item) => !item.matches(path, is_dir, size),
        }
    }
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
                            || (matches!(ch, '<' | '>') && !value.eq_ignore_ascii_case("size:")))
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
                    return Ok(Expr::And(vec![
                        Expr::Kind(false),
                        Expr::Text(compile(&pattern, self.match_case)?, false),
                    ]));
                }
                "size" => return size_predicate(value),
                _ if !key.is_empty() && key.chars().all(|ch| ch.is_ascii_alphabetic()) => {
                    return Err(format!(
                        "Unsupported search predicate '{key}:'. Quote the whole term to search a filename containing ':'."
                    ));
                }
                _ => {}
            }
        }
        self.text(text, self.match_path || text.contains('/'))
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
        Query::parse(query, false, false, false)
            .unwrap()
            .matches(Path::new(path), false, 0)
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
        assert!(!Query::parse("MAIN", true, false, false).unwrap().matches(
            Path::new("main.rs"),
            false,
            0
        ));
        assert!(Query::parse("src", false, true, false).unwrap().matches(
            Path::new("/src/main.rs"),
            false,
            0
        ));
        assert!(
            Query::parse(r"^(main|lib)\.rs$", false, false, true)
                .unwrap()
                .matches(Path::new("/src/lib.rs"), false, 0)
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
                Query::parse(query, false, false, false)
                    .unwrap()
                    .matches(path, directory, size),
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
            assert!(Query::parse(query, false, false, false).is_err(), "{query}");
        }
        assert!(Query::parse(&"!".repeat(40), false, false, false).is_err());
        assert!(Query::parse(&"a".repeat(4097), false, false, false).is_err());
        assert!(Query::parse("[", false, false, true).is_err());
        assert!(matches(" ", "/anything"));
    }
}
