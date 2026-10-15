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

    for raw in text.split('\n') {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            match fence.take() {
                None => fence = Some((rest.trim().to_string(), Vec::new())),
                Some((lang, body)) => out.extend(code_block(&lang, &body)),
            }
            continue;
        }
        match fence.as_mut() {
            Some((_, body)) => body.push(raw.to_string()),
            None => out.push(Line::from(inline(raw))),
        }
    }

    if let Some((lang, body)) = fence {
        out.extend(code_block(&lang, &body));
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
            spans.push(Span::styled(
                chars[i + 2..end].iter().collect::<String>(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            i = end + 2;
        } else if let Some(end) = closing(&chars, i, "`") {
            flush(&mut plain, &mut spans);
            spans.push(Span::styled(
                chars[i + 1..end].iter().collect::<String>(),
                Style::default().fg(Color::Cyan),
            ));
            i = end + 1;
        } else if let Some(end) = closing(&chars, i, "*") {
            flush(&mut plain, &mut spans);
            spans.push(Span::styled(
                chars[i + 1..end].iter().collect::<String>(),
                Style::default().add_modifier(Modifier::ITALIC),
            ));
            i = end + 1;
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

fn closing(chars: &[char], start: usize, marker: &str) -> Option<usize> {
    let m: Vec<char> = marker.chars().collect();
    if start + m.len() >= chars.len() || chars[start..].len() < m.len() {
        return None;
    }
    if chars[start..start + m.len()] != m[..] {
        return None;
    }
    if marker == "*" && chars.get(start + 1) == Some(&'*') {
        return None;
    }

    let mut i = start + m.len();
    while i + m.len() <= chars.len() {
        if chars[i..i + m.len()] == m[..] && i > start + m.len() {
            if marker == "*" && chars.get(i + 1) == Some(&'*') {
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
