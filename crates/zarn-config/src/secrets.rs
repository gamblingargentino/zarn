use regex::Regex;
use std::sync::OnceLock;

pub const PATTERNS: &str = include_str!("../../../scripts/secret-patterns.txt");

struct Compiled {
    rules: Vec<(String, Regex)>,
    allow: Vec<String>,
}

fn compiled() -> &'static Compiled {
    static CELL: OnceLock<Compiled> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut rules = Vec::new();
        let mut allow = Vec::new();
        for line in PATTERNS.lines() {
            let Some((kind, pattern)) = line.split_once('\t') else { continue };
            if kind == "allow" {
                allow.push(pattern.trim().to_string());
            } else if let Ok(re) = Regex::new(pattern.trim()) {
                rules.push((kind.to_string(), re));
            }
        }
        Compiled { rules, allow }
    })
}

pub fn matches(value: &str) -> Option<String> {
    let c = compiled();
    if c.allow.iter().any(|token| value.contains(token.as_str())) {
        return None;
    }
    c.rules.iter().find(|(_, re)| re.is_match(value)).map(|(kind, _)| kind.clone())
}

pub fn is_reference(value: &str) -> bool {
    let v = value.trim();
    v.starts_with("env:") || v.starts_with("key:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scanner_patterns_are_applied_to_values() {
        let shaped = format!("sk-{}", "a1".repeat(12));
        assert_eq!(matches(&shaped).as_deref(), Some("openai"));
        assert_eq!(matches(&format!("Bearer {shaped}")).as_deref(), Some("openai"));
        assert_eq!(matches(&format!("AIza{}", "b".repeat(35))).as_deref(), Some("google"));
        assert_eq!(matches(&format!("API_KEY={}", "c9".repeat(16))).as_deref(), Some("assigned"));
        assert!(matches("test-key-not-real-fixture-value-that-is-long").is_none(), "fixtures stay allowed");
        assert!(matches("application/json").is_none());
        assert!(matches("0123456789abcdef0123456789abcdef01234567").is_none(), "a plain hash is not a key");
        assert!(is_reference("key:github") && is_reference("env:TOKEN") && !is_reference("value"));
    }
}
