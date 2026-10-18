use crate::widgets::{Edit, MaskedInput, Secret};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::io::{BufRead, IsTerminal, Write};

pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn read_secret(label: &str) -> std::io::Result<Option<Secret>> {
    if !std::io::stdin().is_terminal() {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let value = line.trim().to_string();
        return Ok((!value.is_empty()).then(|| Secret::new(value)));
    }

    let mut err = std::io::stderr();
    write!(err, "{label} (input hidden): ")?;
    err.flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let _ = crossterm::execute!(err, crossterm::event::EnableBracketedPaste);
    let mut input = MaskedInput::new();
    let outcome = loop {
        let next = match event::read() {
            Ok(next) => next,
            Err(e) => break Err(e),
        };
        match next {
            Event::Paste(text) => {
                let before = input.len();
                input.paste(&text);
                let _ = write!(err, "{}", "•".repeat(input.len() - before));
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break Ok(None);
                }
                let before = input.len();
                match input.on_key(key) {
                    Edit::Submit => break Ok(Some(input.take())),
                    Edit::Cancel => break Ok(None),
                    _ => {
                        let after = input.len();
                        if after > before {
                            let _ = write!(err, "{}", "•".repeat(after - before));
                        } else if after < before {
                            let _ = write!(err, "{}", "\u{8} \u{8}".repeat(before - after));
                        }
                    }
                }
            }
            _ => {}
        }
        let _ = err.flush();
    };
    let _ = crossterm::execute!(err, crossterm::event::DisableBracketedPaste);
    crossterm::terminal::disable_raw_mode()?;
    writeln!(err)?;
    Ok(outcome?.filter(|k| !k.is_empty()).map(Secret::new))
}

pub fn read_line(label: &str, default: Option<&str>) -> std::io::Result<String> {
    let mut err = std::io::stderr();
    match default {
        Some(d) if !d.is_empty() => write!(err, "{label} [{d}]: ")?,
        _ => write!(err, "{label}: ")?,
    }
    err.flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let value = line.trim().to_string();
    Ok(if value.is_empty() {
        default.unwrap_or_default().to_string()
    } else {
        value
    })
}

pub fn choose(label: &str, options: &[(&str, &str)]) -> std::io::Result<Option<String>> {
    let mut err = std::io::stderr();
    writeln!(err, "{label}")?;
    for (i, (_, text)) in options.iter().enumerate() {
        writeln!(err, "  {}. {text}", i + 1)?;
    }
    let answer = read_line("number or name", Some("1"))?;
    if let Ok(n) = answer.parse::<usize>() {
        return Ok(options.get(n.wrapping_sub(1)).map(|(id, _)| id.to_string()));
    }
    Ok(options
        .iter()
        .find(|(id, _)| *id == answer)
        .map(|(id, _)| id.to_string()))
}
