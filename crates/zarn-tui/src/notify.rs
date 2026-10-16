use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const LONG_TURN: Duration = Duration::from_secs(10);

type Sink = Arc<Mutex<dyn Write + Send>>;

pub struct Notifier {
    pub enabled: bool,
    pub long_turn: Duration,
    tty: bool,
    titles: bool,
    out: Sink,
}

impl Notifier {
    pub fn stdout() -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        Notifier {
            enabled: true,
            long_turn: LONG_TURN,
            tty: std::io::stdout().is_terminal(),
            titles: supports_title(&term),
            out: Arc::new(Mutex::new(std::io::stdout())),
        }
    }

    pub fn with_writer(tty: bool, titles: bool, out: Arc<Mutex<dyn Write + Send>>) -> Self {
        Notifier {
            enabled: true,
            long_turn: LONG_TURN,
            tty,
            titles,
            out,
        }
    }

    pub fn sequence(&self, title: &str) -> Option<String> {
        if !self.enabled || !self.tty {
            return None;
        }
        let mut seq = String::from("\x07");
        if self.titles {
            let clean: String = title.chars().filter(|c| !c.is_control()).collect();
            seq.push_str(&format!("\x1b]2;{clean}\x07"));
        }
        Some(seq)
    }

    pub fn notify(&self, title: &str) -> bool {
        let Some(seq) = self.sequence(title) else {
            return false;
        };
        match self.out.lock() {
            Ok(mut out) => out.write_all(seq.as_bytes()).and_then(|_| out.flush()).is_ok(),
            Err(_) => false,
        }
    }

    pub fn turn_finished(&self, elapsed: Duration) -> bool {
        elapsed > self.long_turn && self.notify("zarn: turn finished")
    }

    pub fn permission(&self, focused: Option<bool>, tool: &str) -> bool {
        focused != Some(true) && self.notify(&format!("zarn: {tool} needs permission"))
    }
}

pub fn supports_title(term: &str) -> bool {
    !matches!(term, "" | "dumb" | "linux")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(tty: bool) -> (Notifier, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        (Notifier::with_writer(tty, true, buf.clone()), buf)
    }

    #[test]
    fn nothing_is_written_when_stdout_is_not_a_terminal() {
        let (n, buf) = capture(false);
        assert!(!n.turn_finished(Duration::from_secs(60)));
        assert!(!n.permission(Some(false), "bash"));
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn long_turns_and_unfocused_prompts_ring_and_set_the_title() {
        let (n, buf) = capture(true);
        assert!(!n.turn_finished(Duration::from_secs(3)));
        assert!(buf.lock().unwrap().is_empty());
        assert!(n.turn_finished(Duration::from_secs(11)));
        assert_eq!(
            String::from_utf8(buf.lock().unwrap().clone()).unwrap(),
            "\x07\x1b]2;zarn: turn finished\x07"
        );
        buf.lock().unwrap().clear();
        assert!(!n.permission(Some(true), "bash"));
        assert!(n.permission(None, "bash"));
        assert!(n.permission(Some(false), "bash"));
    }

    #[test]
    fn disabled_or_titleless_terminals_get_less() {
        let (mut n, buf) = capture(true);
        n.enabled = false;
        assert!(!n.turn_finished(Duration::from_secs(60)));
        assert!(buf.lock().unwrap().is_empty());
        let plain = Notifier::with_writer(true, false, buf.clone());
        assert_eq!(plain.sequence("x").as_deref(), Some("\x07"));
        assert!(!supports_title("dumb"));
        assert!(supports_title("xterm-256color"));
    }
}
