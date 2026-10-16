use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::OnceLock;

pub const ACCENT: Color = Color::Rgb(138, 43, 226);

pub const MARK: [&str; 4] = [" ██████▌", "    ▄█▀ ", "  ▄█▀   ", " ▐██████"];

pub const FRAMES: [&str; 3] = ["▀▝▄", "▀▚▄", "▀▖▄"];

fn cwd() -> &'static str {
    static CWD: OnceLock<String> = OnceLock::new();
    CWD.get_or_init(|| {
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "?".to_string())
    })
}

pub const ACCENTS: &[(&str, Color)] = &[
    ("violet", ACCENT),
    ("blue", Color::Rgb(66, 135, 245)),
    ("cyan", Color::Rgb(0, 200, 210)),
    ("green", Color::Rgb(46, 204, 113)),
    ("yellow", Color::Rgb(241, 196, 15)),
    ("orange", Color::Rgb(255, 140, 0)),
    ("red", Color::Rgb(231, 76, 60)),
    ("magenta", Color::Rgb(232, 62, 140)),
    ("white", Color::Rgb(230, 230, 230)),
];

pub fn accent(name: Option<&str>) -> Color {
    let Some(name) = name.map(str::trim) else {
        return ACCENT;
    };
    if let Some((_, color)) = ACCENTS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return *color;
    }
    parse_hex(name).unwrap_or(ACCENT)
}

pub fn parse_hex(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?))
}

pub fn header(model: &str) -> Vec<Line<'static>> {
    header_in(model, ACCENT)
}

pub fn header_in(model: &str, accent: Color) -> Vec<Line<'static>> {
    let right = [
        format!("zarn {}", env!("CARGO_PKG_VERSION")),
        model.to_string(),
        cwd().to_string(),
        String::new(),
    ];

    MARK.iter()
        .zip(right)
        .map(|(art, text)| {
            Line::from(vec![
                Span::styled(
                    (*art).to_string(),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw("   "),
                Span::styled(text, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect()
}

pub fn frame(tick: usize) -> Span<'static> {
    frame_in(tick, ACCENT)
}

pub fn frame_in(tick: usize, accent: Color) -> Span<'static> {
    Span::styled(FRAMES[tick % FRAMES.len()], Style::default().fg(accent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    const BLOCKS: &str = " █▀▄▌▐▖▗▘▝▚▞▙▛▜▟";

    #[test]
    fn the_mark_uses_only_block_drawing_characters() {
        for row in MARK.iter().chain(FRAMES.iter()) {
            for ch in row.chars() {
                assert!(BLOCKS.contains(ch), "{ch:?} in {row:?} is not a block");
            }
        }
    }

    #[test]
    fn the_mark_rows_are_the_same_width() {
        let width = MARK[0].chars().count();
        for row in MARK {
            assert_eq!(row.chars().count(), width, "{row:?}");
        }
    }

    #[test]
    fn the_header_puts_version_model_and_cwd_beside_the_mark() {
        let lines = header("claude-sonnet-5");
        assert_eq!(lines.len(), MARK.len());

        let rendered = flatten(&lines);
        assert!(rendered[0].starts_with(MARK[0]));
        assert!(rendered[0].contains(env!("CARGO_PKG_VERSION")));
        assert!(rendered[1].contains("claude-sonnet-5"));
        assert!(rendered[2].contains(&cwd().to_string()));
        assert!(rendered[3].trim_end().ends_with('█'));
    }

    #[test]
    fn the_mark_is_drawn_in_electric_violet() {
        let lines = header("m");
        assert_eq!(lines[0].spans[0].style.fg, Some(ACCENT));
        assert_ne!(lines[0].spans[2].style.fg, Some(ACCENT));
    }

    #[test]
    fn accents_resolve_by_name_or_hex_and_default_to_violet() {
        assert_eq!(accent(None), ACCENT);
        assert_eq!(accent(Some("Cyan")), Color::Rgb(0, 200, 210));
        assert_eq!(accent(Some("#0a0b0c")), Color::Rgb(10, 11, 12));
        assert_eq!(accent(Some("nonsense")), ACCENT);
    }

    #[test]
    fn consecutive_ticks_give_different_frames_and_wrap() {
        assert_ne!(frame(0).content, frame(1).content);
        assert_ne!(frame(1).content, frame(2).content);
        assert_eq!(frame(0).content, frame(FRAMES.len()).content);
        assert_eq!(frame(0).style.fg, Some(ACCENT));
    }
}
