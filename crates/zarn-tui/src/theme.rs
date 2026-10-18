use ratatui::style::Color;
use std::cell::Cell;

pub const NAMES: [&str; 4] = ["dark", "light", "colorblind", "auto"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub name: &'static str,
    pub dim: Color,
    pub warn: Color,
    pub ok: Color,
    pub error: Color,
    pub info: Color,
    pub added: Color,
    pub removed: Color,
}

pub const DARK: Palette = Palette {
    name: "dark",
    dim: Color::DarkGray,
    warn: Color::Yellow,
    ok: Color::Green,
    error: Color::Red,
    info: Color::Cyan,
    added: Color::Green,
    removed: Color::Red,
};

pub const LIGHT: Palette = Palette {
    name: "light",
    dim: Color::Indexed(242),
    warn: Color::Indexed(130),
    ok: Color::Indexed(28),
    error: Color::Indexed(160),
    info: Color::Indexed(25),
    added: Color::Indexed(28),
    removed: Color::Indexed(160),
};

pub const COLORBLIND: Palette = Palette {
    name: "colorblind",
    dim: Color::Indexed(245),
    warn: Color::Indexed(214),
    ok: Color::Indexed(33),
    error: Color::Indexed(208),
    info: Color::Indexed(39),
    added: Color::Indexed(33),
    removed: Color::Indexed(208),
};

impl Default for Palette {
    fn default() -> Self {
        DARK
    }
}

pub fn background_is_light(colorfgbg: Option<&str>) -> Option<bool> {
    let bg = colorfgbg?.rsplit(';').next()?.trim().parse::<u8>().ok()?;
    Some(matches!(bg, 7 | 9..=15))
}

pub fn resolve(setting: Option<&str>, colorfgbg: Option<&str>) -> Result<Palette, String> {
    match setting.unwrap_or("auto") {
        "dark" => Ok(DARK),
        "light" => Ok(LIGHT),
        "colorblind" => Ok(COLORBLIND),
        "auto" => Ok(match background_is_light(colorfgbg) {
            Some(true) => LIGHT,
            _ => DARK,
        }),
        other => Err(format!(
            "'{other}' is not a theme; use one of {}",
            NAMES.join(", ")
        )),
    }
}

thread_local! {
    static CURRENT: Cell<Palette> = const { Cell::new(DARK) };
}

pub fn set(palette: Palette) {
    CURRENT.with(|c| c.set(palette));
}

pub fn current() -> Palette {
    CURRENT.with(|c| c.get())
}

pub fn dim() -> Color {
    current().dim
}

pub fn warn() -> Color {
    current().warn
}

pub fn ok() -> Color {
    current().ok
}

pub fn error() -> Color {
    current().error
}

pub fn info() -> Color {
    current().info
}

pub fn added() -> Color {
    current().added
}

pub fn removed() -> Color {
    current().removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_colorfgbg_and_falls_back_to_dark() {
        assert_eq!(resolve(Some("auto"), Some("0;15")).unwrap(), LIGHT);
        assert_eq!(resolve(Some("auto"), Some("15;0")).unwrap(), DARK);
        assert_eq!(resolve(None, Some("15;default;7")).unwrap(), LIGHT);
        assert_eq!(resolve(Some("auto"), None).unwrap(), DARK);
        assert_eq!(resolve(Some("auto"), Some("garbage")).unwrap(), DARK);
        assert_eq!(resolve(Some("colorblind"), None).unwrap(), COLORBLIND);
        assert!(resolve(Some("neon"), None).is_err());
    }
}
