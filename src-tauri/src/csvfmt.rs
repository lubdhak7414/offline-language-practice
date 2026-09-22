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
//!
//! Formula guard (not RFC 4180 — spreadsheet safety), **comma-separated
//! only**: a field that begins with `=`, `+`, `-`, `@`, tab or CR is written
//! with a leading apostrophe, because Excel, LibreOffice and Sheets evaluate
//! such a cell as a formula even when it is quoted. Tab-separated output is
//! for Anki, which imports every field verbatim and would show the
//! apostrophe on the card, so it is left alone — see [`guards_formulas`].
//! [`strip_formula_guard`] undoes exactly that shape on import. The parser
//! itself stays byte-faithful; stripping is the importer's choice.

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

/// Leading characters a spreadsheet treats as the start of a formula — the
/// OWASP CSV-injection list.
///
/// Quoting is not enough: a quoted `"=1+1"` is evaluated too. Only changing
/// the first character stops it, which is why Excel itself writes a leading
/// apostrophe for a literal.
const FORMULA_LEAD: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Whether output with this delimiter gets the formula guard.
///
/// Comma-separated is the spreadsheet format, where the guard is consumed and
/// invisible. Tab-separated is what Anki imports, verbatim — a card reading
/// `-ing endings` would arrive as `'-ing endings`. One rule, used by both the
/// writer and the importer, so they cannot disagree.
pub fn guards_formulas(delimiter: char) -> bool {
    delimiter == ','
}

fn is_formula_lead(field: &str) -> bool {
    field
        .chars()
        .next()
        .is_some_and(|c| FORMULA_LEAD.contains(&c))
}

fn write_field(field: &str, delimiter: char) -> String {
    // The apostrophe is part of the value as far as CSV is concerned, so it
    // goes inside any quotes; the receiving spreadsheet is what consumes it.
    let guarded = if guards_formulas(delimiter) && is_formula_lead(field) {
        format!("'{field}")
    } else {
        field.to_string()
    };
    if needs_quoting(&guarded, delimiter) {
        let escaped = guarded.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        guarded
    }
}

/// Undo the formula guard [`write_csv`] applies.
///
/// Only the exact shape this module writes — an apostrophe immediately
/// followed by a formula lead — so a field that merely starts with an
/// apostrophe (`'tis`) survives a round trip untouched.
pub fn strip_formula_guard(field: &str) -> &str {
    match field.strip_prefix('\'') {
        Some(rest) if is_formula_lead(rest) => rest,
        _ => field,
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
    fn a_leading_formula_character_is_neutralised() {
        for field in ["=1+1", "+1", "-1", "@SUM(A1)"] {
            assert_eq!(write_field(field, ','), format!("'{field}"));
        }
    }

    #[test]
    fn tab_and_carriage_return_leads_are_guarded_too() {
        assert_eq!(write_field("\t=1+1", ','), "'\t=1+1");
        assert_eq!(
            write_field("\r=1", ','),
            "\"'\r=1\"",
            "CR also forces quoting"
        );
    }

    #[test]
    fn tab_separated_output_is_verbatim_for_anki() {
        for field in ["=1+1", "-ing endings", "@home"] {
            assert_eq!(write_field(field, '\t'), field);
        }
        assert!(!guards_formulas('\t'));
    }

    #[test]
    fn an_interior_formula_character_is_left_alone() {
        assert_eq!(write_field("a=b", ','), "a=b");
        assert_eq!(write_field("x-1", ','), "x-1");
        assert_eq!(write_field("", ','), "");
    }

    #[test]
    fn the_guard_sits_inside_the_quotes() {
        assert_eq!(write_field("=1,2", ','), "\"'=1,2\"");
        assert_eq!(write_field("=\"x\"", ','), "\"'=\"\"x\"\"\"");
    }

    #[test]
    fn the_guard_round_trips_and_only_its_own_shape_is_stripped() {
        let rows = vec![vec![
            "=1+1".to_string(),
            "'tis".to_string(),
            "'".to_string(),
        ]];
        let parsed = parse_csv(&write_csv(&rows, ','), ',').unwrap();
        assert_eq!(parsed[0][0], "'=1+1", "the parser stays byte-faithful");
        assert_eq!(strip_formula_guard(&parsed[0][0]), "=1+1");
        assert_eq!(
            strip_formula_guard(&parsed[0][1]),
            "'tis",
            "not the guard shape"
        );
        assert_eq!(
            strip_formula_guard(&parsed[0][2]),
            "'",
            "bare apostrophe kept"
        );
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
