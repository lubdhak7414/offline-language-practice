//! RFC 4180 CSV/TSV formatting and parsing, pure and dependency-free.
//!
//! `portable.rs` is the only consumer (card export/import), so this module
//! knows nothing about cards, decks, or tags — just rows of strings. Kept
//! separate because the quoting/escaping rules are fiddly enough to deserve
//! their own focused tests, independent of the import/export semantics that
//! sit on top.
//!
//! Quoting rule (RFC 4180 s.2.6/2.7): a field is quoted when it contains the
//! delimiter, a `"`, a CR, or a LF; a literal `"` inside a quoted field is
//! escaped by doubling it (`""`). The parser accepts both CRLF and bare LF
//! row terminators, strips a leading UTF-8 BOM (Excel likes to add one), and
//! understands a quoted field that itself contains embedded newlines.

/// Serialize `rows` to CSV/TSV text using `delimiter` (`,` or `\t`).
///
/// Always emits CRLF row terminators (the RFC 4180 wire format), including
/// after the final row — that trailing terminator makes the output
/// self-consistent with what [`parse_csv`] expects to split on and matches
/// what spreadsheet tools emit.
pub fn write_csv(rows: &[Vec<String>], delimiter: char) -> String {
    let mut out = String::new();
    for row in rows {
        for (i, field) in row.iter().enumerate() {
            if i > 0 {
                out.push(delimiter);
            }
            out.push_str(&write_field(field, delimiter));
        }
        out.push_str("\r\n");
    }
    out
}

fn needs_quoting(field: &str, delimiter: char) -> bool {
    field.contains(delimiter) || field.contains('"') || field.contains('\r') || field.contains('\n')
}

fn write_field(field: &str, delimiter: char) -> String {
    if needs_quoting(field, delimiter) {
        let escaped = field.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        field.to_string()
    }
}

/// Parse CSV/TSV text into rows of fields.
///
/// Strips a leading UTF-8 BOM, accepts CRLF or bare LF row terminators, and
/// supports a quoted field containing embedded delimiters/quotes/newlines. A
/// ragged file (rows of differing field counts) is not an error here — the
/// caller decides what to do with short/long rows.
pub fn parse_csv(input: &str, delimiter: char) -> Result<Vec<Vec<String>>, String> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();
    let mut row_has_content = false;

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }

        if c == '"' && field.is_empty() {
            in_quotes = true;
            row_has_content = true;
            continue;
        }
        if c == delimiter {
            row.push(std::mem::take(&mut field));
            row_has_content = true;
            continue;
        }
        if c == '\r' {
            // Swallow, wait for the paired '\n' (or a bare '\r', treated the
            // same as '\n' below via the next iteration falling through).
            if chars.peek() == Some(&'\n') {
                continue;
            }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
            row_has_content = false;
            continue;
        }
        if c == '\n' {
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
            row_has_content = false;
            continue;
        }
        field.push(c);
        row_has_content = true;
    }

    if in_quotes {
        return Err("unterminated quoted field".to_string());
    }
    if row_has_content || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }

    Ok(rows)
}

/// Guess the delimiter from a header/first line: tab-separated when tabs
/// outnumber commas, comma otherwise (including a tie, since CSV is the more
/// common default).
pub fn sniff_delimiter(first_line: &str) -> char {
    let tabs = first_line.matches('\t').count();
    let commas = first_line.matches(',').count();
    if tabs > commas {
        '\t'
    } else {
        ','
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_field_with_delimiter_quote_and_newline() {
        let rows = vec![vec!["a,\"b\"\nc".to_string(), "plain".to_string()]];
        let text = write_csv(&rows, ',');
        let parsed = parse_csv(&text, ',').unwrap();
        assert_eq!(parsed, rows);
    }

    #[test]
    fn parses_a_quoted_newline_field() {
        let text = "front,back\r\n\"line1\nline2\",back1\r\n";
        let parsed = parse_csv(text, ',').unwrap();
        assert_eq!(
            parsed,
            vec![
                vec!["front".to_string(), "back".to_string()],
                vec!["line1\nline2".to_string(), "back1".to_string()],
            ]
        );
    }

    #[test]
    fn handles_a_ragged_row_without_erroring() {
        let text = "a,b,c\r\nx,y\r\n";
        let parsed = parse_csv(text, ',').unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].len(), 3);
        assert_eq!(parsed[1].len(), 2);
    }

    #[test]
    fn strips_a_leading_bom() {
        let text = "\u{feff}front,back\r\nhello,hola\r\n";
        let parsed = parse_csv(text, ',').unwrap();
        assert_eq!(parsed[0], vec!["front".to_string(), "back".to_string()]);
    }

    #[test]
    fn sniffs_tab_when_tabs_outnumber_commas() {
        assert_eq!(sniff_delimiter("front\tback\ttags"), '\t');
        assert_eq!(sniff_delimiter("front,back,tags"), ',');
        assert_eq!(sniff_delimiter("no delimiters here"), ',');
    }

    #[test]
    fn bare_lf_rows_are_accepted() {
        let text = "a,b\nc,d\n";
        let parsed = parse_csv(text, ',').unwrap();
        assert_eq!(
            parsed,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string(), "d".to_string()],
            ]
        );
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert!(parse_csv("\"unterminated", ',').is_err());
    }
}
