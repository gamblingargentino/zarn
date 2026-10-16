use crate::Config;
use zarn_core::cost::{CostTable, Rates};

pub const DEFAULTS: &[(&str, f64, f64, f64)] = &[
    ("claude-fable-5-1", 10.0, 50.0, 0.25),
    ("claude-fable-5", 10.0, 50.0, 1.0),
    ("claude-opus-5-5", 4.0, 20.0, 0.20),
    ("claude-opus-5", 5.0, 25.0, 0.50),
    ("claude-opus-4-8", 5.0, 25.0, 0.50),
    ("claude-opus-4-7", 5.0, 25.0, 0.50),
    ("claude-opus-4-6", 5.0, 25.0, 0.50),
    ("claude-sonnet-5-5", 2.0, 10.0, 0.20),
    ("claude-sonnet-5", 2.0, 10.0, 0.20),
    ("claude-sonnet-4-6", 3.0, 15.0, 0.30),
    ("claude-haiku-5-5", 0.10, 0.50, 0.01),
    ("claude-haiku-4-5", 1.0, 5.0, 0.10),
    ("gpt-5", 1.25, 10.0, 0.125),
    ("gpt-5-mini", 0.25, 2.0, 0.025),
    ("gemini-2.5-pro", 1.25, 10.0, 0.125),
    ("gemini-2.5-flash", 0.30, 2.50, 0.03),
];

pub fn defaults() -> CostTable {
    let mut table = CostTable::empty();
    for (prefix, input, output, cached) in DEFAULTS {
        table.set(*prefix, Rates::new(*input, *output, *cached));
    }
    table
}

pub fn table(cfg: &Config) -> CostTable {
    let mut table = defaults();
    for (prefix, rates) in &cfg.pricing {
        table.set(prefix.clone(), *rates);
    }
    table
}

pub fn label(table: &CostTable, model: &str) -> String {
    match table.lookup(model) {
        Some(r) => format!(
            "${} in / ${} out / ${} cached per million tokens",
            r.input, r.output, r.cached
        ),
        None => format!("no rate configured for {model}; add it under [pricing]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_models_are_priced_out_of_the_box() {
        let table = defaults();
        assert_eq!(table.lookup("claude-sonnet-5-5").unwrap().input, 2.0);
        assert_eq!(table.lookup("gpt-5-mini").unwrap().output, 2.0);
        assert_eq!(table.lookup("gpt-5").unwrap().output, 10.0);
        assert!(table.lookup("some-local-model").is_none());
    }

    #[test]
    fn user_config_overrides_a_default_and_adds_new_models() {
        let cfg: Config = toml::from_str(
            "[pricing.\"claude-sonnet-5-5\"]\ninput = 9.0\noutput = 9.0\n\n[pricing.\"llama\"]\ninput = 0.1\noutput = 0.2\ncached = 0.0\n",
        )
        .unwrap();
        let table = table(&cfg);
        assert_eq!(table.lookup("claude-sonnet-5-5").unwrap().input, 9.0);
        assert_eq!(table.lookup("claude-sonnet-5-5").unwrap().cached, 0.0);
        assert_eq!(table.lookup("llama-3.3-70b").unwrap().output, 0.2);
    }

    #[test]
    fn an_unpriced_model_says_so() {
        assert!(label(&defaults(), "mystery").contains("no rate configured"));
    }
}
