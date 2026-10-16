use std::io::IsTerminal;
use zarn_config::doctor::{Check, Status};

pub fn color_depth() -> (Status, String) {
    let colorterm = std::env::var("COLORTERM").unwrap_or_default().to_lowercase();
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        (Status::Ok, "truecolor".into())
    } else if std::env::var("WT_SESSION").is_ok() {
        (Status::Ok, "truecolor (Windows Terminal)".into())
    } else if term.contains("256color") {
        (Status::Ok, "256 colors; accent colors are approximated".into())
    } else if term == "dumb" {
        (Status::Warn, "TERM=dumb; colors and the TUI will not render well".into())
    } else if cfg!(windows) {
        (Status::Ok, "Windows console".into())
    } else {
        (Status::Warn, format!("basic colors (TERM={term}); set COLORTERM=truecolor if your terminal supports it"))
    }
}

pub fn keyboard_enhanced() -> bool {
    std::io::stdout().is_terminal()
        && crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
}

pub fn checks(known_keyboard: Option<bool>) -> Vec<Check> {
    let mut out = Vec::new();
    let tty = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();
    out.push(if tty {
        Check::new("terminal", Status::Ok, "interactive")
    } else {
        Check::new("terminal", Status::Warn, "not a TTY; only zarn --print works here")
    });
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let status = if cols < 60 || rows < 16 { Status::Warn } else { Status::Ok };
        out.push(Check::new("terminal size", status, format!("{cols}x{rows}")));
    }
    let (status, detail) = color_depth();
    out.push(Check::new("colors", status, detail));
    if tty {
        let enhanced = match known_keyboard {
            Some(known) => known,
            None => {
                let raw = crossterm::terminal::enable_raw_mode().is_ok();
                let enhanced = keyboard_enhanced();
                if raw {
                    let _ = crossterm::terminal::disable_raw_mode();
                }
                enhanced
            }
        };
        out.push(if enhanced {
            Check::new("keyboard", Status::Ok, "enhanced keys; shift+enter inserts a newline")
        } else {
            Check::new(
                "keyboard",
                Status::Warn,
                "no keyboard enhancement; use alt+enter for a newline",
            )
        });
    }
    out
}
