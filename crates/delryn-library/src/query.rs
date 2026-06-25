//! A small filter-query language over the library, e.g.
//! `author:knuth year>=1990 (series:art OR favorite) -converted`.
//!
//! It is **lenient**: any input parses. Bare words match title/author/series/
//! publisher (so the plain `/` search still works); fields, comparisons, flags,
//! `AND`/`OR`/`NOT` (or a `-` prefix), and parentheses add structure. The app
//! keeps using full-text body search for plain queries and switches to this
//! field evaluator once a query [`is_structured`](Query::is_structured).

use delryn_store::BookRow;

/// A parsed filter query (a boolean tree over [`Term`]s).
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    /// Matches everything (empty input).
    All,
    Term(Term),
    Not(Box<Query>),
    And(Vec<Query>),
    Or(Vec<Query>),
}

/// A single matchable condition.
#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    /// A bare word: case-insensitive substring over title/author/series/publisher.
    Text(String),
    /// A boolean flag (`favorite`, `converted`, reading status).
    Flag(Flag),
    /// A field comparison (`author:knuth`, `year>=1990`, `progress<50`).
    Field { key: String, op: Op, value: String },
}

/// Boolean book predicates usable as bare keywords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Favorite,
    Converted,
    /// No reading progress yet.
    Unread,
    /// Started but not finished.
    Reading,
    /// Read to (near) the end.
    Finished,
    /// Manual override: paused.
    Paused,
    /// Manual override: dropped.
    Dropped,
    /// Manual override: kept as a reference.
    Reference,
}

impl Flag {
    /// The reading status this flag denotes, if it is a status flag (else `None`
    /// for `Favorite` / `Converted`).
    fn status(self) -> Option<delryn_model::ReadingStatus> {
        use delryn_model::ReadingStatus as RS;
        Some(match self {
            Flag::Unread => RS::Unread,
            Flag::Reading => RS::Reading,
            Flag::Finished => RS::Finished,
            Flag::Paused => RS::Paused,
            Flag::Dropped => RS::Dropped,
            Flag::Reference => RS::Reference,
            _ => return None,
        })
    }
}

/// Comparison operator. `Colon` is the field-default (substring for text,
/// equality for numbers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Colon,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

// --- Parsing ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Atom(String),
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            tokens.push(Token::LParen);
            i += 1;
        } else if c == ')' {
            tokens.push(Token::RParen);
            i += 1;
        } else {
            // A word runs until whitespace or a paren; quotes protect spaces.
            let mut word = String::new();
            let mut in_quote = false;
            while i < chars.len() {
                let c = chars[i];
                if c == '"' {
                    in_quote = !in_quote;
                    i += 1;
                } else if !in_quote && (c.is_whitespace() || c == '(' || c == ')') {
                    break;
                } else {
                    word.push(c);
                    i += 1;
                }
            }
            tokens.push(match word.to_ascii_lowercase().as_str() {
                "and" => Token::And,
                "or" => Token::Or,
                "not" => Token::Not,
                _ => Token::Atom(word),
            });
        }
    }
    tokens
}

/// Parse a filter query. Never fails — malformed input degrades to text terms.
pub fn parse(input: &str) -> Query {
    let tokens = tokenize(input);
    let mut p = Parser { tokens, pos: 0 };
    let q = p.parse_or();
    q.unwrap_or(Query::All)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn parse_or(&mut self) -> Option<Query> {
        let mut parts = vec![self.parse_and()?];
        while matches!(self.peek(), Some(Token::Or)) {
            self.pos += 1;
            if let Some(q) = self.parse_and() {
                parts.push(q);
            }
        }
        Some(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            Query::Or(parts)
        })
    }

    fn parse_and(&mut self) -> Option<Query> {
        let mut parts = vec![self.parse_unary()?];
        loop {
            match self.peek() {
                // Explicit AND, or juxtaposition (another primary follows).
                Some(Token::And) => {
                    self.pos += 1;
                }
                Some(Token::Not | Token::LParen | Token::Atom(_)) => {}
                _ => break,
            }
            match self.parse_unary() {
                Some(q) => parts.push(q),
                None => break,
            }
        }
        Some(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            Query::And(parts)
        })
    }

    fn parse_unary(&mut self) -> Option<Query> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.pos += 1;
            return Some(Query::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Option<Query> {
        match self.peek()? {
            Token::LParen => {
                self.pos += 1;
                let inner = self.parse_or();
                if matches!(self.peek(), Some(Token::RParen)) {
                    self.pos += 1;
                }
                inner
            }
            Token::Atom(_) => {
                let Some(Token::Atom(s)) = self.tokens.get(self.pos).cloned() else {
                    return None;
                };
                self.pos += 1;
                Some(term_from_atom(&s))
            }
            // A stray closing paren / operator: skip it and move on.
            _ => {
                self.pos += 1;
                self.parse_primary()
            }
        }
    }
}

fn term_from_atom(s: &str) -> Query {
    if let Some(rest) = s.strip_prefix('-')
        && !rest.is_empty()
    {
        return Query::Not(Box::new(term_from_atom(rest)));
    }
    // Field/comparison: split on the first operator (longest first).
    for (sym, op) in [
        (">=", Op::Ge),
        ("<=", Op::Le),
        ("!=", Op::Ne),
        (":", Op::Colon),
        ("=", Op::Eq),
        (">", Op::Gt),
        ("<", Op::Lt),
    ] {
        if let Some(i) = s.find(sym)
            && i > 0
        {
            let key = s[..i].to_ascii_lowercase();
            let value = unquote(&s[i + sym.len()..]);
            return Query::Term(Term::Field { key, op, value });
        }
    }
    let lower = s.to_ascii_lowercase();
    match flag_from_word(&lower) {
        Some(flag) => Query::Term(Term::Flag(flag)),
        None => Query::Term(Term::Text(unquote(s))),
    }
}

fn unquote(s: &str) -> String {
    s.replace('"', "")
}

fn flag_from_word(w: &str) -> Option<Flag> {
    Some(match w {
        "favorite" | "favourite" | "fav" | "starred" => Flag::Favorite,
        "converted" => Flag::Converted,
        "unread" | "new" => Flag::Unread,
        "reading" | "started" | "inprogress" => Flag::Reading,
        "finished" | "done" | "complete" => Flag::Finished,
        "paused" => Flag::Paused,
        "dropped" | "abandoned" => Flag::Dropped,
        "reference" | "ref" => Flag::Reference,
        _ => return None,
    })
}

// --- Evaluation ------------------------------------------------------------

impl Query {
    /// Whether the query uses anything beyond plain text terms — fields, flags,
    /// negation, or `OR`. Plain-text queries can keep using full-text search.
    pub fn is_structured(&self) -> bool {
        match self {
            Query::All => false,
            Query::Term(Term::Text(_)) => false,
            Query::Term(_) => true,
            Query::Not(_) | Query::Or(_) => true,
            Query::And(parts) => parts.iter().any(Query::is_structured),
        }
    }

    /// Evaluate the query against a book row.
    pub fn matches(&self, b: &BookRow) -> bool {
        match self {
            Query::All => true,
            Query::Term(t) => t.matches(b),
            Query::Not(q) => !q.matches(b),
            Query::And(parts) => parts.iter().all(|q| q.matches(b)),
            Query::Or(parts) => parts.iter().any(|q| q.matches(b)),
        }
    }
}

impl Term {
    fn matches(&self, b: &BookRow) -> bool {
        match self {
            Term::Text(s) => {
                let n = s.to_lowercase();
                [&b.title, &b.author, &b.series, &b.publisher]
                    .iter()
                    .any(|f| f.to_lowercase().contains(&n))
            }
            Term::Flag(flag) => match flag {
                Flag::Favorite => b.favorite,
                Flag::Converted => b.converted,
                // Status flags compare against the single *effective* status
                // (a manual override wins), so they stay mutually exclusive.
                _ => flag.status().is_some_and(|want| {
                    delryn_model::ReadingStatus::effective(b.pct, &b.status) == want
                }),
            },
            Term::Field { key, op, value } => match_field(key, *op, value, b),
        }
    }
}

/// Which book attribute a field key names.
enum Resolved<'a> {
    Text(&'a str),
    Num(Option<f64>),
    Unknown,
}

fn resolve<'a>(key: &str, b: &'a BookRow) -> Resolved<'a> {
    match key {
        "title" | "t" | "name" => Resolved::Text(&b.title),
        "author" | "a" | "by" => Resolved::Text(&b.author),
        "series" => Resolved::Text(&b.series),
        "publisher" | "pub" => Resolved::Text(&b.publisher),
        "language" | "lang" => Resolved::Text(&b.language),
        "isbn" => Resolved::Text(&b.isbn),
        "subtitle" => Resolved::Text(&b.subtitle),
        "path" | "file" => Resolved::Text(&b.path),
        "year" | "yr" | "y" => Resolved::Num(b.year.map(f64::from)),
        "progress" | "pct" | "percent" => Resolved::Num(Some(f64::from(b.pct))),
        "rating" | "stars" => Resolved::Num(Some(f64::from(b.rating))),
        _ => Resolved::Unknown,
    }
}

fn match_field(key: &str, op: Op, value: &str, b: &BookRow) -> bool {
    // `status:reading` / `is:favorite` compare the requested flag to the book's.
    if matches!(key, "status" | "is") {
        return match flag_from_word(&value.to_lowercase()) {
            Some(want) => Term::Flag(want).matches(b),
            None => false,
        };
    }
    match resolve(key, b) {
        Resolved::Text(hay) => {
            let hay = hay.to_lowercase();
            let needle = value.to_lowercase();
            match op {
                Op::Colon | Op::Eq => hay.contains(&needle),
                Op::Ne => !hay.contains(&needle),
                _ => false,
            }
        }
        Resolved::Num(actual) => cmp_num(actual, op, value),
        Resolved::Unknown => false,
    }
}

fn cmp_num(actual: Option<f64>, op: Op, value: &str) -> bool {
    let (Some(a), Ok(v)) = (actual, value.trim().parse::<f64>()) else {
        return false;
    };
    match op {
        Op::Colon | Op::Eq => (a - v).abs() < f64::EPSILON,
        Op::Ne => (a - v).abs() >= f64::EPSILON,
        Op::Lt => a < v,
        Op::Le => a <= v,
        Op::Gt => a > v,
        Op::Ge => a >= v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> BookRow {
        BookRow {
            path: "/books/taocp.epub".into(),
            title: "The Art of Computer Programming".into(),
            author: "Donald Knuth".into(),
            year: Some(1997),
            size: 1000,
            favorite: true,
            pct: 42,
            series: "TAOCP".into(),
            series_index: Some(1.0),
            publisher: "Addison-Wesley".into(),
            subtitle: String::new(),
            isbn: String::new(),
            language: "en".into(),
            converted: false,
            rating: 4,
            status: String::new(),
        }
    }

    #[test]
    fn plain_text_is_not_structured_and_matches_metadata() {
        let q = parse("knuth");
        assert!(!q.is_structured());
        assert!(q.matches(&book()));
        assert!(!parse("tolkien").matches(&book()));
    }

    #[test]
    fn field_substring_and_equality() {
        assert!(parse("author:knuth").matches(&book()));
        assert!(parse("title:art").matches(&book()));
        assert!(parse("publisher:wesley").matches(&book()));
        assert!(!parse("author:tolkien").matches(&book()));
        assert!(parse("author:knuth").is_structured());
    }

    #[test]
    fn numeric_comparisons_on_year_and_progress() {
        assert!(parse("year>=1990").matches(&book()));
        assert!(parse("year<2000").matches(&book()));
        assert!(!parse("year>2000").matches(&book()));
        assert!(parse("year:1997").matches(&book()));
        assert!(parse("progress>10").matches(&book()));
        assert!(parse("progress<50").matches(&book()));
    }

    #[test]
    fn flags_and_reading_status() {
        assert!(parse("favorite").matches(&book()));
        assert!(parse("reading").matches(&book())); // pct 42 → reading
        assert!(!parse("finished").matches(&book()));
        assert!(!parse("unread").matches(&book()));
        assert!(!parse("converted").matches(&book()));
        assert!(parse("status:reading").matches(&book()));
        assert!(!parse("status:finished").matches(&book()));
    }

    #[test]
    fn manual_status_overrides_progress() {
        let mut b = book(); // pct 42 → reading by progress
        b.status = "paused".into();
        // The manual override wins: paused, not reading.
        assert!(parse("paused").matches(&b));
        assert!(parse("status:paused").matches(&b));
        assert!(!parse("reading").matches(&b));
        // A dropped/reference book likewise isn't "reading".
        b.status = "dropped".into();
        assert!(parse("dropped").matches(&b));
        assert!(!parse("paused").matches(&b));
    }

    #[test]
    fn boolean_combinators_and_precedence() {
        // implicit AND
        assert!(parse("author:knuth year>=1990").matches(&book()));
        assert!(!parse("author:knuth year>2000").matches(&book()));
        // OR
        assert!(parse("author:tolkien OR author:knuth").matches(&book()));
        // NOT (word and `-` prefix)
        assert!(parse("not author:tolkien").matches(&book()));
        assert!(parse("-converted").matches(&book()));
        assert!(!parse("-favorite").matches(&book()));
        // grouping: (A OR B) AND C
        assert!(parse("(author:tolkien OR favorite) year<2000").matches(&book()));
        assert!(!parse("(author:tolkien OR converted) year<2000").matches(&book()));
    }

    #[test]
    fn quoted_values_keep_spaces() {
        assert!(parse(r#"author:"donald knuth""#).matches(&book()));
        assert!(parse(r#"title:"art of computer""#).matches(&book()));
    }

    #[test]
    fn rating_field_compares() {
        assert!(parse("rating>=4").matches(&book()));
        assert!(parse("rating:4").matches(&book()));
        assert!(!parse("rating>4").matches(&book()));
        assert!(parse("stars<5").matches(&book()));
    }

    #[test]
    fn unknown_field_matches_nothing() {
        assert!(!parse("tag:rust").matches(&book()));
        assert!(parse("tag:rust").is_structured());
    }

    #[test]
    fn empty_query_matches_all() {
        let q = parse("   ");
        assert_eq!(q, Query::All);
        assert!(q.matches(&book()));
        assert!(!q.is_structured());
    }
}
