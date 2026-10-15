use crate::provider::Event;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cached: f64,
}

impl Rates {
    pub fn new(input: f64, output: f64, cached: f64) -> Self {
        Rates {
            input,
            output,
            cached,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cached == 0
    }

    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cached += other.cached;
    }

    pub fn cost(&self, rates: &Rates) -> f64 {
        let uncached = self.input.saturating_sub(self.cached) as f64;
        (uncached * rates.input + self.cached as f64 * rates.cached + self.output as f64
            * rates.output)
            / 1_000_000.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct CostTable {
    entries: Vec<(String, Rates)>,
}

impl CostTable {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn set(&mut self, prefix: impl Into<String>, rates: Rates) {
        let prefix = prefix.into();
        self.entries.retain(|(p, _)| *p != prefix);
        self.entries.push((prefix, rates));
        self.entries
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    pub fn lookup(&self, model: &str) -> Option<Rates> {
        self.entries
            .iter()
            .find(|(prefix, _)| model.starts_with(prefix.as_str()))
            .map(|(_, rates)| *rates)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct TurnCost {
    pub usage: Usage,
    pub usd: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct CostTracker {
    table: CostTable,
    model: String,
    turns: Vec<TurnCost>,
    current: Usage,
}

impl CostTracker {
    pub fn new(table: CostTable, model: impl Into<String>) -> Self {
        CostTracker {
            table,
            model: model.into(),
            turns: Vec::new(),
            current: Usage::default(),
        }
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    pub fn rates(&self) -> Option<Rates> {
        self.table.lookup(&self.model)
    }

    pub fn observe(&mut self, event: &Event) {
        if let Event::Usage {
            input,
            output,
            cached,
        } = event
        {
            self.current.add(&Usage {
                input: u64::from(*input),
                output: u64::from(*output),
                cached: u64::from(*cached),
            });
        }
    }

    pub fn end_turn(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let usage = std::mem::take(&mut self.current);
        let usd = self.table.lookup(&self.model).map(|r| usage.cost(&r));
        self.turns.push(TurnCost { usage, usd });
    }

    pub fn turns(&self) -> &[TurnCost] {
        &self.turns
    }

    pub fn total(&self) -> TurnCost {
        let mut usage = Usage::default();
        for turn in &self.turns {
            usage.add(&turn.usage);
        }
        usage.add(&self.current);
        let usd = self.table.lookup(&self.model).map(|r| usage.cost(&r));
        TurnCost { usage, usd }
    }

    pub fn report(&self) -> String {
        let mut out = String::new();
        for (i, turn) in self.turns.iter().enumerate() {
            let cost = match turn.usd {
                Some(usd) => format!("${usd:.4}"),
                None => "no rate configured".to_string(),
            };
            out.push_str(&format!(
                "turn {}: {} in / {} out / {} cached  {}\n",
                i + 1,
                turn.usage.input,
                turn.usage.output,
                turn.usage.cached,
                cost
            ));
        }
        let total = self.total();
        let cost = match total.usd {
            Some(usd) => format!("${usd:.4}"),
            None => format!("no rate configured for {}", self.model),
        };
        out.push_str(&format!(
            "total: {} in / {} out / {} cached  {}",
            total.usage.input, total.usage.output, total.usage.cached, cost
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> CostTable {
        let mut table = CostTable::empty();
        table.set("test-", Rates::new(1.0, 2.0, 0.5));
        table.set("test-big", Rates::new(10.0, 20.0, 5.0));
        table
    }

    #[test]
    fn the_longest_matching_prefix_wins() {
        let table = table();
        assert_eq!(table.lookup("test-small").unwrap().input, 1.0);
        assert_eq!(table.lookup("test-big-1").unwrap().input, 10.0);
        assert!(table.lookup("other").is_none());
    }

    #[test]
    fn cached_tokens_are_billed_at_the_cached_rate_not_twice() {
        let usage = Usage {
            input: 1_000_000,
            output: 0,
            cached: 400_000,
        };
        let cost = usage.cost(&Rates::new(1.0, 2.0, 0.5));
        assert!((cost - (0.6 + 0.2)).abs() < 1e-9);
    }

    #[test]
    fn usage_events_accumulate_across_a_turn() {
        let mut tracker = CostTracker::new(table(), "test-small");
        tracker.observe(&Event::Usage {
            input: 100,
            output: 10,
            cached: 0,
        });
        tracker.observe(&Event::Usage {
            input: 50,
            output: 5,
            cached: 20,
        });
        tracker.end_turn();

        assert_eq!(tracker.turns().len(), 1);
        assert_eq!(tracker.total().usage.input, 150);
        assert_eq!(tracker.total().usage.cached, 20);
    }

    #[test]
    fn an_unknown_model_reports_no_cost_rather_than_zero() {
        let mut tracker = CostTracker::new(CostTable::empty(), "mystery");
        tracker.observe(&Event::Usage {
            input: 100,
            output: 10,
            cached: 0,
        });
        tracker.end_turn();
        assert!(tracker.total().usd.is_none());
        assert!(tracker.report().contains("no rate configured"));
    }

    #[test]
    fn an_empty_turn_is_not_recorded() {
        let mut tracker = CostTracker::new(table(), "test-small");
        tracker.end_turn();
        tracker.end_turn();
        assert!(tracker.turns().is_empty());
    }
}
