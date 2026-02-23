//! Markdown sanitization for Telegram.
//!
//! Telegram's Markdown parser supports only a limited subset of formatting
//! and chokes on tables, headers, and other standard markdown constructs.
//! This module converts standard markdown into Telegram-compatible form:
//!
//! - Tables -> plain text rows (separator lines stripped)
//! - `## Headers` -> just the header text (# prefix stripped)
//! - `---` horizontal rules -> stripped entirely
//! - `**bold**` -> `*bold*` (Telegram Markdown uses single asterisks)
//! - Code blocks (triple backticks) -> preserved as-is

/// Sanitize standard markdown for Telegram's limited markdown parser.
///
/// Processes the text line-by-line, skipping content inside fenced code blocks.
pub fn sanitize_for_telegram(text: &str) -> String {
    let mut lines_out = Vec::new();
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Track fenced code blocks — pass through unchanged.
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            lines_out.push(line.to_owned());
            continue;
        }
        if in_code_block {
            lines_out.push(line.to_owned());
            continue;
        }

        // Horizontal rules: 3+ of -, *, or _ (with optional spaces).
        if is_horizontal_rule(trimmed) {
            continue;
        }

        // Table separator lines: |---|---|
        if is_table_separator(trimmed) {
            continue;
        }

        // Table data rows: | col1 | col2 |
        if is_table_row(trimmed) {
            let plain = convert_table_row(trimmed);
            lines_out.push(convert_bold(&plain));
            continue;
        }

        // Headers: # ... ## ... ### ...
        if let Some(header_text) = strip_header(trimmed) {
            lines_out.push(convert_bold(header_text));
            continue;
        }

        // Regular line — just convert bold.
        lines_out.push(convert_bold(line));
    }

    lines_out.join("\n")
}

/// Check if a line is a markdown horizontal rule.
///
/// Matches 3+ of the same character (-, *, _) with optional spaces between.
fn is_horizontal_rule(line: &str) -> bool {
    let stripped: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    if stripped.len() < 3 {
        return false;
    }
    let first = stripped.as_bytes()[0];
    matches!(first, b'-' | b'*' | b'_') && stripped.bytes().all(|b| b == first)
}

/// Check if a line is a table separator (e.g., `|---|---|`).
///
/// These lines contain only `|`, `-`, `:`, and spaces.
fn is_table_separator(line: &str) -> bool {
    if !line.contains('|') || !line.contains('-') {
        return false;
    }
    line.chars()
        .all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Check if a line looks like a table data row (starts and ends with `|`).
fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 1
}

/// Convert a table row to plain text by stripping pipes and trimming cells.
fn convert_table_row(line: &str) -> String {
    let trimmed = line.trim();
    let inner = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or(trimmed);
    inner
        .split('|')
        .map(|cell| cell.trim())
        .collect::<Vec<_>>()
        .join("  ")
}

/// Strip markdown header prefix (`#`, `##`, `###`, etc.) and return the text.
///
/// Requires a space after the `#` sequence (CommonMark spec), so `#hashtag`
/// is NOT treated as a header.
fn strip_header(line: &str) -> Option<&str> {
    if !line.starts_with('#') {
        return None;
    }
    let after_hashes = line.trim_start_matches('#');
    // CommonMark requires a space (or empty) after # for headers.
    if !after_hashes.is_empty() && !after_hashes.starts_with(' ') {
        return None;
    }
    Some(after_hashes.trim_start())
}

/// Convert `**bold**` to `*bold*` for Telegram Markdown.
fn convert_bold(line: &str) -> String {
    line.replace("**", "*")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Table tests ---

    #[test]
    fn table_basic() {
        let input = "\
| Name | Age | City |
|------|-----|------|
| Alice | 30 | NYC |
| Bob | 25 | LA |";
        let expected = "\
Name  Age  City
Alice  30  NYC
Bob  25  LA";
        assert_eq!(sanitize_for_telegram(input), expected);
    }

    #[test]
    fn table_with_alignment() {
        let input = "\
| Left | Center | Right |
|:-----|:------:|------:|
| a    | b      | c     |";
        let expected = "\
Left  Center  Right
a  b  c";
        assert_eq!(sanitize_for_telegram(input), expected);
    }

    #[test]
    fn table_single_column() {
        let input = "\
| Item |
|------|
| Foo |";
        let expected = "\
Item
Foo";
        assert_eq!(sanitize_for_telegram(input), expected);
    }

    // --- Header tests ---

    #[test]
    fn header_level_1() {
        assert_eq!(sanitize_for_telegram("# Title"), "Title");
    }

    #[test]
    fn header_level_2() {
        assert_eq!(sanitize_for_telegram("## Section"), "Section");
    }

    #[test]
    fn header_level_3() {
        assert_eq!(sanitize_for_telegram("### Subsection"), "Subsection");
    }

    #[test]
    fn header_level_4() {
        assert_eq!(sanitize_for_telegram("#### Deep"), "Deep");
    }

    #[test]
    fn header_level_6() {
        assert_eq!(sanitize_for_telegram("###### Level 6"), "Level 6");
    }

    #[test]
    fn header_not_hashtag() {
        // #hashtag (no space) should NOT be treated as a header.
        assert_eq!(sanitize_for_telegram("#hashtag"), "#hashtag");
    }

    // --- Horizontal rule tests ---

    #[test]
    fn hr_dashes() {
        assert_eq!(sanitize_for_telegram("---"), "");
    }

    #[test]
    fn hr_asterisks() {
        assert_eq!(sanitize_for_telegram("***"), "");
    }

    #[test]
    fn hr_underscores() {
        assert_eq!(sanitize_for_telegram("___"), "");
    }

    #[test]
    fn hr_spaced_dashes() {
        assert_eq!(sanitize_for_telegram("- - -"), "");
    }

    #[test]
    fn hr_long() {
        assert_eq!(sanitize_for_telegram("----------"), "");
    }

    #[test]
    fn hr_two_dashes_not_hr() {
        // Only 2 dashes — not a valid HR.
        assert_eq!(sanitize_for_telegram("--"), "--");
    }

    // --- Bold conversion tests ---

    #[test]
    fn bold_single() {
        assert_eq!(sanitize_for_telegram("**bold**"), "*bold*");
    }

    #[test]
    fn bold_inline() {
        assert_eq!(
            sanitize_for_telegram("some **bold** text"),
            "some *bold* text"
        );
    }

    #[test]
    fn bold_multiple() {
        assert_eq!(
            sanitize_for_telegram("**a** and **b**"),
            "*a* and *b*"
        );
    }

    #[test]
    fn bold_in_header() {
        assert_eq!(
            sanitize_for_telegram("## **Important** Section"),
            "*Important* Section"
        );
    }

    #[test]
    fn single_asterisk_preserved() {
        // Single asterisks (italic) should pass through unchanged.
        assert_eq!(sanitize_for_telegram("*italic*"), "*italic*");
    }

    // --- Code block tests ---

    #[test]
    fn code_block_preserved() {
        let input = "\
```rust
fn main() {
    let x = **y;
    println!(\"# not a header\");
    // | not | a | table |
}
```";
        assert_eq!(sanitize_for_telegram(input), input);
    }

    #[test]
    fn code_block_with_surrounding_text() {
        let input = "\
## Header

Some **bold** text.

```
| table | inside | code |
---
# header inside code
```

More **bold** text.";
        let expected = "\
Header

Some *bold* text.

```
| table | inside | code |
---
# header inside code
```

More *bold* text.";
        assert_eq!(sanitize_for_telegram(input), expected);
    }

    // --- Mixed content tests ---

    #[test]
    fn mixed_content() {
        let input = "\
## Status Report

Here is the **summary**:

| Task | Status |
|------|--------|
| Build | Done |
| Test | Pending |

---

See the results above.";
        let expected = "\
Status Report

Here is the *summary*:

Task  Status
Build  Done
Test  Pending


See the results above.";
        assert_eq!(sanitize_for_telegram(input), expected);
    }

    // --- Edge cases ---

    #[test]
    fn empty_input() {
        assert_eq!(sanitize_for_telegram(""), "");
    }

    #[test]
    fn no_markdown() {
        let input = "Just plain text with no markdown formatting at all.";
        assert_eq!(sanitize_for_telegram(input), input);
    }

    #[test]
    fn only_whitespace_lines() {
        let input = "line one\n\nline two";
        assert_eq!(sanitize_for_telegram(input), input);
    }

    #[test]
    fn multiline_no_special() {
        let input = "Hello\nWorld\nFoo";
        assert_eq!(sanitize_for_telegram(input), input);
    }

    #[test]
    fn pipe_in_regular_text() {
        // A pipe in regular text (not a table) should be left alone.
        let input = "Use foo | bar for piping";
        assert_eq!(sanitize_for_telegram(input), input);
    }

    #[test]
    fn header_with_only_hashes() {
        // A bare `#` on a line (empty header).
        assert_eq!(sanitize_for_telegram("#"), "");
    }
}
