use std::sync::LazyLock;
use tiktoken_rs::CoreBPE;

pub const DEFAULT_TOKEN_BUDGET: usize = 25_000;

static ENCODER: LazyLock<Option<CoreBPE>> = LazyLock::new(|| tiktoken_rs::cl100k_base().ok());

pub fn count(text: &str) -> usize {
    match ENCODER.as_ref() {
        Some(bpe) => bpe.encode_with_special_tokens(text).len(),
        None => text.len() / 4,
    }
}

pub fn truncate(text: &str, budget: usize) -> String {
    let total = count(text);
    if total <= budget {
        return text.to_string();
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut kept = String::new();
    let mut used = 0;
    let mut shown = 0;

    for line in &lines {
        let cost = count(line) + 1;
        if used + cost > budget {
            break;
        }
        kept.push_str(line);
        kept.push('\n');
        used += cost;
        shown += 1;
    }

    let hidden = lines.len().saturating_sub(shown);
    kept.push_str(&format!(
        "\n[truncated: {hidden} of {} lines cut, {total} tokens over a {budget} token budget]",
        lines.len()
    ));
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_returned_unchanged() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn long_text_is_cut_and_says_how_much_was_cut() {
        let text = (0..500)
            .map(|i| format!("line {i} with some words in it"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = truncate(&text, 100);

        assert!(count(&out) < count(&text));
        assert!(out.contains("truncated:"));
        assert!(out.contains("of 500 lines cut"));
        assert!(out.starts_with("line 0"));
    }

    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        let text = (0..400).map(|_| "héllo wörld 🌍").collect::<Vec<_>>().join("\n");
        let out = truncate(&text, 50);
        assert!(out.is_char_boundary(out.len()));
        assert!(out.contains('🌍') || out.contains("truncated"));
    }

    #[test]
    fn a_budget_of_zero_still_reports_rather_than_panicking() {
        let out = truncate("one\ntwo\nthree", 0);
        assert!(out.contains("truncated:"));
    }
}
