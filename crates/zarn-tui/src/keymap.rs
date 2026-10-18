use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyAction {
    ExpandOutput,
    HistorySearch,
    CycleMode,
    ProfilePicker,
    ExternalEditor,
    Transcript,
    SearchScrollback,
    ScrollUp,
    ScrollDown,
    HistoryPrev,
    HistoryNext,
    Newline,
    ExpandThinking,
}

pub const ACTIONS: &[(KeyAction, &str, &str, &str)] = &[
    (KeyAction::ExpandOutput, "expand_output", "ctrl+r", "expand or collapse the last tool output"),
    (KeyAction::HistorySearch, "history_search", "ctrl+f", "search your earlier inputs"),
    (KeyAction::CycleMode, "cycle_mode", "shift+tab", "switch to the next mode"),
    (KeyAction::ProfilePicker, "profile_picker", "ctrl+p", "pick a profile for this session"),
    (KeyAction::ExternalEditor, "external_editor", "ctrl+g", "edit the input in $VISUAL or $EDITOR"),
    (KeyAction::Transcript, "transcript", "ctrl+o", "show every tool output in full"),
    (KeyAction::SearchScrollback, "search", "ctrl+s", "search the scrollback (n and N move)"),
    (KeyAction::ScrollUp, "scroll_up", "pageup", "scroll the conversation up"),
    (KeyAction::ScrollDown, "scroll_down", "pagedown", "scroll the conversation down"),
    (KeyAction::HistoryPrev, "history_prev", "up", "previous input"),
    (KeyAction::HistoryNext, "history_next", "down", "next input"),
    (KeyAction::Newline, "newline", "shift+enter", "insert a new line"),
    (KeyAction::ExpandThinking, "expand_thinking", "ctrl+t", "show or hide model reasoning"),
];

pub const RESERVED: &[&str] = &["ctrl+c", "enter", "esc", "backspace", "tab"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

pub fn parse(spec: &str) -> Result<Key, String> {
    let lower = spec.trim().to_lowercase();
    let mut parts: Vec<&str> = lower.split('+').map(str::trim).collect();
    let Some(last) = parts.pop() else {
        return Err(format!("'{spec}' is empty"));
    };
    let mut key = Key {
        code: KeyCode::Null,
        ctrl: false,
        alt: false,
        shift: false,
    };
    for modifier in parts {
        match modifier {
            "ctrl" | "control" => key.ctrl = true,
            "alt" | "meta" | "option" => key.alt = true,
            "shift" => key.shift = true,
            other => return Err(format!("'{spec}': unknown modifier '{other}'")),
        }
    }
    key.code = match last {
        "enter" | "return" => KeyCode::Enter,
        "tab" if key.shift => {
            key.shift = true;
            KeyCode::BackTab
        }
        "tab" => KeyCode::Tab,
        "backtab" => {
            key.shift = true;
            KeyCode::BackTab
        }
        "esc" | "escape" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "space" => KeyCode::Char(' '),
        f if f.starts_with('f') && f.len() > 1 && f[1..].parse::<u8>().is_ok() => {
            KeyCode::F(f[1..].parse().unwrap_or(1))
        }
        c if c.chars().count() == 1 => KeyCode::Char(c.chars().next().unwrap_or(' ')),
        other => return Err(format!("'{spec}': unknown key '{other}'")),
    };
    Ok(key)
}

pub fn from_event(event: &KeyEvent) -> Key {
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    let code = match event.code {
        KeyCode::Char(c) if event.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyCode::Char(c.to_ascii_lowercase())
        }
        other => other,
    };
    Key {
        code,
        ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
        alt: event.modifiers.contains(KeyModifiers::ALT),
        shift: shift && !matches!(code, KeyCode::Char(_)) || code == KeyCode::BackTab,
    }
}

pub fn action_name(action: KeyAction) -> &'static str {
    ACTIONS
        .iter()
        .find(|(a, ..)| *a == action)
        .map(|(_, name, ..)| *name)
        .unwrap_or("unknown")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    bindings: BTreeMap<KeyAction, Vec<(String, Key)>>,
    pub problems: Vec<String>,
}

impl Default for Keymap {
    fn default() -> Self {
        let bindings = ACTIONS
            .iter()
            .map(|(action, _, spec, _)| {
                let key = parse(spec).expect("default key");
                (*action, vec![(spec.to_string(), key)])
            })
            .collect();
        Keymap {
            bindings,
            problems: Vec::new(),
        }
    }
}

impl Keymap {
    pub fn path() -> Option<PathBuf> {
        zarn_config::paths::user_dir().map(|d| d.join("keybindings.toml"))
    }

    pub fn load() -> Self {
        match Self::path() {
            Some(path) => Self::load_from(&path),
            None => Self::default(),
        }
    }

    pub fn load_from(path: &Path) -> Self {
        let mut map = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return map;
        };
        let table: toml::Table = match toml::from_str(&text) {
            Ok(t) => t,
            Err(err) => {
                map.problems.push(format!("{} is not valid TOML: {err}", path.display()));
                return map;
            }
        };
        for (name, value) in table {
            let Some((action, ..)) = ACTIONS.iter().find(|(_, n, ..)| *n == name) else {
                map.problems.push(format!("{}: unknown action '{name}'", path.display()));
                continue;
            };
            let specs: Vec<String> = match value {
                toml::Value::String(s) => vec![s],
                toml::Value::Array(items) => items
                    .into_iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                _ => {
                    map.problems.push(format!("{}: '{name}' must be a key or a list of keys", path.display()));
                    continue;
                }
            };
            let mut keys = Vec::new();
            for spec in specs {
                match parse(&spec) {
                    Ok(key) => keys.push((spec, key)),
                    Err(err) => map.problems.push(format!("{}: {err}", path.display())),
                }
            }
            map.bindings.insert(*action, keys);
        }
        map.problems.extend(map.conflicts());
        map
    }

    pub fn conflicts(&self) -> Vec<String> {
        let mut seen: std::collections::HashMap<Key, Vec<(&str, String)>> = Default::default();
        for (action, keys) in &self.bindings {
            for (spec, key) in keys {
                seen.entry(*key).or_default().push((action_name(*action), spec.clone()));
            }
        }
        let mut out = Vec::new();
        let mut keys: Vec<(&Key, &Vec<(&str, String)>)> = seen.iter().collect();
        keys.sort_by(|a, b| a.1[0].1.cmp(&b.1[0].1));
        for (key, users) in keys {
            if users.len() > 1 {
                let names: Vec<&str> = users.iter().map(|(n, _)| *n).collect();
                out.push(format!("{} is bound to both {}", users[0].1, names.join(" and ")));
            }
            for reserved in RESERVED {
                if parse(reserved).ok().as_ref() == Some(key) {
                    out.push(format!(
                        "{} ({}) is reserved by zarn and cannot be rebound",
                        reserved, users[0].0
                    ));
                }
            }
        }
        out
    }

    pub fn action(&self, event: &KeyEvent) -> Option<KeyAction> {
        let pressed = from_event(event);
        self.bindings.iter().find_map(|(action, keys)| {
            keys.iter()
                .any(|(_, key)| {
                    *key == pressed
                        || (key.code == KeyCode::Enter
                            && pressed.code == KeyCode::Enter
                            && key.shift
                            && (pressed.shift || pressed.alt))
                })
                .then_some(*action)
        })
    }

    pub fn label(&self, action: KeyAction) -> String {
        self.bindings
            .get(&action)
            .and_then(|k| k.first())
            .map(|(spec, _)| spec.clone())
            .unwrap_or_else(|| "unbound".into())
    }

    pub fn help(&self) -> String {
        ACTIONS
            .iter()
            .map(|(action, name, _, what)| format!("{:<12} {name:<16} {what}", self.label(*action)))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn checks() -> Vec<zarn_config::doctor::Check> {
    use zarn_config::doctor::{Check, Status};
    let map = Keymap::load();
    let path = Keymap::path().map(|p| p.display().to_string()).unwrap_or_default();
    if map.problems.is_empty() {
        vec![Check::new("keybindings", Status::Ok, format!("no conflicts ({path})"))]
    } else {
        map.problems
            .iter()
            .map(|p| {
                Check::new(
                    "keybindings",
                    Status::Warn,
                    format!("{p}; edit {path} to fix it"),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn the_defaults_bind_ctrl_r_to_expand_and_ctrl_f_to_history() {
        let map = Keymap::default();
        assert_eq!(map.action(&ctrl('r')), Some(KeyAction::ExpandOutput));
        assert_eq!(map.action(&ctrl('f')), Some(KeyAction::HistorySearch));
        assert_eq!(
            map.action(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(KeyAction::CycleMode)
        );
        assert_eq!(
            map.action(&KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            Some(KeyAction::Newline)
        );
        assert_eq!(map.action(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), None);
        assert!(map.conflicts().is_empty());
    }

    #[test]
    fn a_user_file_overrides_the_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.toml");
        std::fs::write(&path, "expand_output = \"ctrl+e\"\nhistory_search = [\"ctrl+h\", \"f3\"]\n").unwrap();
        let map = Keymap::load_from(&path);
        assert!(map.problems.is_empty(), "{:?}", map.problems);
        assert_eq!(map.action(&ctrl('e')), Some(KeyAction::ExpandOutput));
        assert_eq!(map.action(&ctrl('r')), None);
        assert_eq!(map.action(&KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE)), Some(KeyAction::HistorySearch));
        assert!(map.help().contains("ctrl+e"));
    }

    #[test]
    fn conflicts_and_bad_entries_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.toml");
        std::fs::write(
            &path,
            "expand_output = \"ctrl+f\"\ntranscript = \"ctrl+c\"\nnope = \"ctrl+x\"\nsearch = \"hyper+q\"\n",
        )
        .unwrap();
        let map = Keymap::load_from(&path);
        let text = map.problems.join("\n");
        assert!(text.contains("ctrl+f is bound to both expand_output and history_search"), "{text}");
        assert!(text.contains("ctrl+c (transcript) is reserved"), "{text}");
        assert!(text.contains("unknown action 'nope'"), "{text}");
        assert!(text.contains("unknown modifier 'hyper'"), "{text}");
    }
}
