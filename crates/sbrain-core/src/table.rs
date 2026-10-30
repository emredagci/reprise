//! Markdown tables as the vault keeps them: a heading and text above, one table, nothing after.
//! Cells hold one line; `|` in a cell is written `\|`.

/// The rows of the first table in `text` (header and separator left out), each with as many cells as
/// the header; text outside the table is ignored.
pub fn rows(text: &str) -> Vec<Vec<String>> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.starts_with('|'));
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let width = cells(header).len();
    lines
        .skip(1)
        .take_while(|l| l.starts_with('|'))
        .map(|l| {
            let mut c = cells(l);
            c.resize(width, String::new());
            c
        })
        .collect()
}

fn cells(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|');
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => out.push(std::mem::take(&mut cell).trim().to_owned()),
            c => cell.push(c),
        }
    }
    out.push(cell.trim().to_owned());
    out
}

/// One cell: on one line, `|` escaped.
pub fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// `head` (the text above the table), then the table.
pub fn render(head: &str, columns: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = format!("{}\n\n", head.trim_end());
    out.push_str(&format!("| {} |\n", columns.join(" | ")));
    out.push_str(&format!(
        "|{}\n",
        columns.iter().map(|_| " --- |").collect::<String>()
    ));
    for row in rows {
        let row: Vec<String> = row.iter().map(|c| cell(c)).collect();
        out.push_str(&format!("| {} |\n", row.join(" | ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_pipes_and_empty_cells() {
        let rows_in = vec![
            vec!["a|b".to_owned(), String::new(), "c".to_owned()],
            vec!["x".to_owned(), "two\nlines".to_owned(), String::new()],
        ];
        let text = render("# T\n\nAbout it.", &["One", "Two", "Three"], &rows_in);
        assert!(
            text.starts_with("# T\n\nAbout it.\n\n| One | Two | Three |\n| --- | --- | --- |\n")
        );
        let back = rows(&text);
        assert_eq!(back[0], ["a|b", "", "c"]);
        assert_eq!(back[1], ["x", "two lines", ""]);
        assert!(rows("# Nothing yet\n").is_empty());
        assert_eq!(rows("| A | B |\n| --- | --- |\n| 1 |\n")[0], ["1", ""]);
    }
}
