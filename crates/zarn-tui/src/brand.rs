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

pub fn header(model: &str) -> Vec<Line<'static>> {
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
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("   "),
                Span::styled(text, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect()
}

pub fn frame(tick: usize) -> Span<'static> {
    Span::styled(FRAMES[tick % FRAMES.len()], Style::default().fg(ACCENT))
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
    fn consecutive_ticks_give_different_frames_and_wrap() {
        assert_ne!(frame(0).content, frame(1).content);
        assert_ne!(frame(1).content, frame(2).content);
        assert_eq!(frame(0).content, frame(FRAMES.len()).content);
        assert_eq!(frame(0).style.fg, Some(ACCENT));
    }
}
