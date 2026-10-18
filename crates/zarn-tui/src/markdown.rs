use crate::theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let mut set = ThemeSet::load_defaults();
    set.themes
        .remove("base16-ocean.dark")
        .or_else(|| set.themes.values().next().cloned())
        .unwrap_or_default()
});

pub fn render(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut fence: Option<(String, Vec<String>)> = None;
    let mut table: Vec<String> = Vec::new();

    for raw in text.split('\n') {
        let trimmed = raw.trim_start();
        if fence.is_none() && trimmed.starts_with('|') {
            table.push(trimmed.trim_end().to_string());
            continue;
        }
        if !table.is_empty() {
            out.extend(table_block(&std::mem::take(&mut table)));
        }
        if let Some(rest) = trimmed.strip_prefix("```") {
            match fence.take() {
                None => fence = Some((rest.trim().to_string(), Vec::new())),
                Some((lang, body)) => out.extend(code_block(&lang, &body)),
            }
            continue;
        }
        match fence.as_mut() {
            Some((_, body)) => body.push(raw.to_string()),
            None => out.push(block_line(raw)),
        }
    }

    if !table.is_empty() {
        out.extend(table_block(&table));
    }
    if let Some((lang, body)) = fence {
        out.extend(code_block(&lang, &body));
    }

    out
}

fn block_line(raw: &str) -> Line<'static> {
    let trimmed = raw.trim_start();
    let indent = raw.len() - trimmed.len();
    let pad = " ".repeat(indent.min(12) / 2 * 2);

    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        let title = trimmed[hashes..].trim();
        let mut style = Style::default().add_modifier(Modifier::BOLD);
        if hashes <= 2 {
            style = style.fg(theme::info());
        }
        let mut spans: Vec<Span<'static>> = inline(title)
            .into_iter()
            .map(|s| Span::styled(s.content.to_string(), s.style.patch(style)))
            .collect();
        if hashes == 1 {
            spans.iter_mut().for_each(|s| s.style = s.style.add_modifier(Modifier::UNDERLINED));
        }
        return Line::from(spans);
    }

    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() >= 3
        && (compact.chars().all(|c| c == '-')
            || compact.chars().all(|c| c == '*')
            || compact.chars().all(|c| c == '_'))
    {
        return Line::from(Span::styled("─".repeat(40), Style::default().fg(theme::dim())));
    }

    if let Some(rest) = trimmed.strip_prefix('>') {
        let mut spans = vec![Span::styled(format!("{pad}│ "), Style::default().fg(theme::dim()))];
        spans.extend(
            inline(rest.strip_prefix(' ').unwrap_or(rest))
                .into_iter()
                .map(|s| Span::styled(s.content.to_string(), s.style.add_modifier(Modifier::ITALIC))),
        );
        return Line::from(spans);
    }

    if let Some(rest) = bullet(trimmed) {
        let (mark, rest) = match rest.strip_prefix("[ ] ").or_else(|| rest.strip_prefix("[ ]")) {
            Some(r) => ("☐ ", r),
            None => match ["[x] ", "[X] "].iter().find_map(|p| rest.strip_prefix(p)) {
                Some(r) => ("☑ ", r),
                None => (if indent >= 2 { "◦ " } else { "• " }, rest),
            },
        };
        let mut spans = vec![Span::raw(pad), Span::styled(mark, Style::default().fg(theme::info()))];
        spans.extend(inline(rest));
        return Line::from(spans);
    }

    if let Some((number, rest)) = numbered(trimmed) {
        let mut spans = vec![
            Span::raw(pad),
            Span::styled(format!("{number}. "), Style::default().fg(theme::info())),
        ];
        spans.extend(inline(rest));
        return Line::from(spans);
    }

    Line::from(inline(raw))
}

fn bullet(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    let marker = chars.next()?;
    if !matches!(marker, '-' | '*' | '+') {
        return None;
    }
    let rest = &line[1..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    Some(rest.trim_start())
}

fn numbered(line: &str) -> Option<(&str, &str)> {
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    Some((&line[..digits], rest.trim_start()))
}

fn table_cells(row: &str) -> Vec<String> {
    let inner = row.trim().trim_start_matches('|').trim_end_matches('|');
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_rule_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
        })
}

fn plain_width(text: &str) -> usize {
    inline(text).iter().map(|s| s.content.chars().count()).sum()
}

fn table_block(rows: &[String]) -> Vec<Line<'static>> {
    let parsed: Vec<Vec<String>> = rows.iter().map(|r| table_cells(r)).collect();
    let columns = parsed.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    for row in parsed.iter().filter(|r| !is_rule_row(r)) {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(plain_width(cell).min(40));
        }
    }
    let border = Style::default().fg(theme::dim());
    let header = parsed.len() > 1 && is_rule_row(&parsed[1]);
    let mut out = Vec::new();
    for (index, row) in parsed.iter().enumerate() {
        if is_rule_row(row) {
            let line: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
            out.push(Line::from(Span::styled(format!("  {}", line.join("─┼─")), border)));
            continue;
        }
        let mut spans = vec![Span::raw("  ")];
        for (i, width) in widths.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", border));
            }
            let cell = row.get(i).cloned().unwrap_or_default();
            let used = plain_width(&cell);
            for span in inline(&cell) {
                let style = if header && index == 0 {
                    span.style.add_modifier(Modifier::BOLD)
                } else {
                    span.style
                };
                spans.push(Span::styled(span.content.to_string(), style));
            }
            if used < *width {
                spans.push(Span::raw(" ".repeat(width - used)));
            }
        }
        out.push(Line::from(spans));
    }
    out
}

fn code_block(lang: &str, body: &[String]) -> Vec<Line<'static>> {
    let source = format!("{}\n", body.join("\n"));
    let syntax = SYNTAXES
        .find_syntax_by_token(lang)
        .or_else(|| SYNTAXES.find_syntax_by_first_line(&source))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, &THEME);
    let mut lines = Vec::new();

    for line in LinesWithEndings::from(&source) {
        let spans = match highlighter.highlight_line(line, &SYNTAXES) {
            Err(_) => vec![Span::raw(line.trim_end_matches('\n').to_string())],
            Ok(ranges) => ranges
                .into_iter()
                .map(|(style, piece)| {
                    Span::styled(
                        piece.trim_end_matches('\n').to_string(),
                        Style::default().fg(Color::Rgb(
                            style.foreground.r,
                            style.foreground.g,
                            style.foreground.b,
                        )),
                    )
                })
                .collect(),
        };
        lines.push(Line::from(prefixed(spans)));
    }

    if lines.last().map(|l| l.width() == 0).unwrap_or(false) {
        lines.pop();
    }
    lines
}

fn prefixed(mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out = vec![Span::styled("  ", Style::default())];
    out.append(&mut spans);
    out
}

pub fn inline(text: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut i = 0;

    let flush = |plain: &mut String, spans: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            spans.push(Span::raw(std::mem::take(plain)));
        }
    };

    while i < chars.len() {
        if let Some(end) = closing(&chars, i, "**") {
            flush(&mut plain, &mut spans);
            for span in inline(&chars[i + 2..end].iter().collect::<String>()) {
                spans.push(Span::styled(
                    span.content.to_string(),
                    span.style.add_modifier(Modifier::BOLD),
                ));
            }
            i = end + 2;
        } else if let Some(end) = closing(&chars, i, "`") {
            flush(&mut plain, &mut spans);
            spans.push(Span::styled(
                chars[i + 1..end].iter().collect::<String>(),
                Style::default().fg(theme::info()),
            ));
            i = end + 1;
        } else if let Some(end) = closing(&chars, i, "*") {
            flush(&mut plain, &mut spans);
            spans.push(Span::styled(
                chars[i + 1..end].iter().collect::<String>(),
                Style::default().add_modifier(Modifier::ITALIC),
            ));
            i = end + 1;
        } else if let Some((label, url, next)) = link(&chars, i) {
            flush(&mut plain, &mut spans);
            spans.push(Span::styled(
                label.clone(),
                Style::default().fg(theme::info()).add_modifier(Modifier::UNDERLINED),
            ));
            if url != label {
                spans.push(Span::styled(format!(" ({url})"), Style::default().fg(theme::dim())));
            }
            i = next;
        } else {
            plain.push(chars[i]);
            i += 1;
        }
    }

    flush(&mut plain, &mut spans);
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

fn link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    let close = (start + 1..chars.len()).find(|&j| chars[j] == ']')?;
    if chars.get(close + 1) != Some(&'(') || close == start + 1 {
        return None;
    }
    let end = (close + 2..chars.len()).find(|&j| chars[j] == ')')?;
    let label: String = chars[start + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    if url.is_empty() || url.contains(char::is_whitespace) {
        return None;
    }
    Some((label, url, end + 1))
}

fn closing(chars: &[char], start: usize, marker: &str) -> Option<usize> {
    let m: Vec<char> = marker.chars().collect();
    if start + m.len() >= chars.len() || chars[start..].len() < m.len() {
        return None;
    }
    if chars[start..start + m.len()] != m[..] {
        return None;
    }
    let emphasis = marker != "`";
    if marker == "*" && chars.get(start + 1) == Some(&'*') {
        return None;
    }
    if emphasis && chars.get(start + m.len()).is_none_or(|c| c.is_whitespace()) {
        return None;
    }
    if emphasis && start > 0 && chars[start - 1].is_alphanumeric() && marker == "*" {
        return None;
    }

    let mut i = start + m.len();
    while i + m.len() <= chars.len() {
        if chars[i..i + m.len()] == m[..] && i > start + m.len() {
            if marker == "*" && (chars.get(i + 1) == Some(&'*') || chars[i - 1] == '*') {
                i += 1;
                continue;
            }
            if emphasis && chars[i - 1].is_whitespace() {
                i += 1;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn bold_italic_and_code_are_styled_without_their_markers() {
        let spans = inline("a **b** c *d* e `f`");
        assert_eq!(plain(&spans), "a b c d e f");
        assert!(
            spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::ITALIC))
        );
    }

    fn lines(text: &str) -> Vec<String> {
        render(text)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn gemini_style_bullets_headings_and_numbers_render_without_their_markers() {
        let out = lines("## Plan\n*   **Step:** read the file\n    *   nested *point*\n- dash item\n1. first\n2) second\n> quoted\n---");
        assert_eq!(out[0], "Plan");
        assert_eq!(out[1], "• Step: read the file");
        assert_eq!(out[2], "    ◦ nested point");
        assert_eq!(out[3], "• dash item");
        assert_eq!(out[4], "1. first");
        assert_eq!(out[5], "2. second");
        assert_eq!(out[6], "│ quoted");
        assert!(out[7].starts_with("───"));
        let bold = render("*   **Step:** x");
        assert!(bold[0].spans.iter().any(|s| s.content == "Step:" && s.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn tables_line_up_and_links_show_their_target() {
        let out = lines("| name | n |\n|---|---|\n| a | 10 |\n| bbb | 2 |\nsee [docs](https://x.dev)");
        assert_eq!(out[0], "  name │ n ");
        assert_eq!(out[1], "  ─────┼───");
        assert_eq!(out[2], "  a    │ 10");
        assert_eq!(out[3], "  bbb  │ 2 ");
        assert_eq!(out[4], "see docs (https://x.dev)");
    }

    #[test]
    fn stars_around_spaces_or_inside_words_are_not_italics() {
        assert_eq!(plain(&inline("a * b * c")), "a * b * c");
        assert_eq!(plain(&inline("2*3*4")), "2*3*4");
        assert_eq!(plain(&inline("*a* and **b**")), "a and b");
    }

    #[test]
    fn an_unclosed_marker_stays_literal() {
        let spans = inline("2 * 3 is not italic");
        assert_eq!(plain(&spans), "2 * 3 is not italic");
    }

    #[test]
    fn a_fenced_block_survives_an_unknown_language() {
        let lines = render("before\n```wat\nx = 1\n```\nafter");
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text[0], "before");
        assert!(text.iter().any(|l| l.contains("x = 1")));
        assert_eq!(text.last().unwrap(), "after");
    }

    #[test]
    fn an_unterminated_fence_still_renders_its_body() {
        let lines = render("```rust\nfn main() {}");
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("fn main()"));
    }
}
