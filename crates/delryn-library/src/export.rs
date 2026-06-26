//! Export the (filtered) book list to CSV, JSON, or Markdown. Pure string
//! formatting — the caller writes the result to disk.

use delryn_store::BookRow;

/// One export column: its header and a value extractor.
type Column = (&'static str, fn(&BookRow) -> String);

/// Columns exported, in order.
fn columns() -> [Column; 8] {
    [
        ("title", |b| b.title.clone()),
        ("author", |b| b.author.clone()),
        ("year", |b| {
            b.year.map(|y| y.to_string()).unwrap_or_default()
        }),
        ("series", |b| b.series.clone()),
        ("publisher", |b| b.publisher.clone()),
        ("rating", |b| {
            if b.rating > 0 {
                b.rating.to_string()
            } else {
                String::new()
            }
        }),
        ("progress", |b| format!("{}", b.pct)),
        ("path", |b| b.path.clone()),
    ]
}

/// RFC-4180 CSV: quote fields containing `,`, `"`, or a newline; double inner quotes.
pub fn to_csv(books: &[BookRow]) -> String {
    let cols = columns();
    let mut out = String::new();
    out.push_str(&cols.iter().map(|(h, _)| *h).collect::<Vec<_>>().join(","));
    out.push('\n');
    for b in books {
        let row: Vec<String> = cols.iter().map(|(_, get)| csv_field(&get(b))).collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// A JSON array of objects (one per book). Hand-rolled — no serde dependency.
pub fn to_json(books: &[BookRow]) -> String {
    let cols = columns();
    let mut out = String::from("[\n");
    for (i, b) in books.iter().enumerate() {
        out.push_str("  {");
        let fields: Vec<String> = cols
            .iter()
            .map(|(h, get)| format!("\"{}\": \"{}\"", h, json_escape(&get(b))))
            .collect();
        out.push_str(&fields.join(", "));
        out.push('}');
        if i + 1 < books.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push(']');
    out
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A Markdown table.
pub fn to_markdown(books: &[BookRow]) -> String {
    let cols = columns();
    let mut out = String::new();
    let header: Vec<&str> = cols.iter().map(|(h, _)| *h).collect();
    out.push_str(&format!("| {} |\n", header.join(" | ")));
    out.push_str(&format!(
        "|{}|\n",
        cols.iter().map(|_| " --- ").collect::<Vec<_>>().join("|")
    ));
    for b in books {
        let row: Vec<String> = cols.iter().map(|(_, get)| md_cell(&get(b))).collect();
        out.push_str(&format!("| {} |\n", row.join(" | ")));
    }
    out
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(title: &str, author: &str, rating: u8) -> BookRow {
        BookRow {
            path: "/p.epub".into(),
            title: title.into(),
            author: author.into(),
            year: Some(2020),
            size: 0,
            favorite: false,
            pct: 50,
            series: String::new(),
            series_index: None,
            publisher: "Pub".into(),
            subtitle: String::new(),
            isbn: String::new(),
            language: String::new(),
            converted: false,
            rating,
            status: String::new(),
            tags: String::new(),
        }
    }

    #[test]
    fn csv_has_header_and_quotes_commas() {
        let csv = to_csv(&[book("A, B", "Knuth", 4)]);
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "title,author,year,series,publisher,rating,progress,path"
        );
        let row = lines.next().unwrap();
        assert!(row.starts_with("\"A, B\",Knuth,2020"), "row: {row}");
        assert!(row.contains(",4,50,"), "rating+progress: {row}");
    }

    #[test]
    fn csv_escapes_quotes() {
        let csv = to_csv(&[book("say \"hi\"", "X", 0)]);
        assert!(csv.contains("\"say \"\"hi\"\"\""), "{csv}");
    }

    #[test]
    fn json_is_array_of_objects_escaped() {
        let json = to_json(&[book("a\"b", "X", 5)]);
        assert!(json.starts_with('[') && json.trim_end().ends_with(']'));
        assert!(json.contains(r#""title": "a\"b""#), "{json}");
        assert!(json.contains(r#""rating": "5""#));
    }

    #[test]
    fn markdown_table_escapes_pipes() {
        let md = to_markdown(&[book("a|b", "X", 0)]);
        assert!(md.contains("| title | author |"));
        assert!(md.contains("| a\\|b | X |"), "{md}");
    }

    #[test]
    fn empty_export_is_just_header() {
        assert_eq!(
            to_csv(&[]).trim(),
            "title,author,year,series,publisher,rating,progress,path"
        );
        assert_eq!(to_json(&[]), "[\n]");
    }
}
