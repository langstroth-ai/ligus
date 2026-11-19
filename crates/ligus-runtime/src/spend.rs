//! What an API turn costs: the provider's reported token usage times a
//! built-in price table, charged to the turn's [`SpendMeter`] so the
//! per-turn `maxSpendUsd` budget cancels the turn once it is reached.
//!
//! The table holds Anthropic's first-party list prices (USD per million
//! tokens; cache reads at 0.1x and cache writes at 1.25x the input price).
//! A model the table doesn't know is priced at its highest rate, so an
//! unknown model can only end a budgeted turn early, never let it overspend.
//! `LIGUS_MODEL_PRICES` (device-local, `prefix=in/out,…`, USD per million
//! tokens) adds or overrides entries, e.g. `gpt-4o=2.5/10`.
//!
//! [`SpendMeter`]: crate::agent_turn::SpendMeter

use crate::agent_turn::SpendError;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
}

/// Token usage one request reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Uncached input tokens.
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

/// Model id prefixes and their prices, most specific first.
const TABLE: &[(&str, Price)] = &[
    ("claude-fable-5", Price { input: 10.0, output: 50.0 }),
    ("claude-mythos-5", Price { input: 10.0, output: 50.0 }),
    ("claude-opus-5-5", Price { input: 4.0, output: 20.0 }),
    ("claude-opus-5", Price { input: 5.0, output: 25.0 }),
    ("claude-opus-4-8", Price { input: 5.0, output: 25.0 }),
    ("claude-opus-4-7", Price { input: 5.0, output: 25.0 }),
    ("claude-opus-4-6", Price { input: 5.0, output: 25.0 }),
    ("claude-opus-4-5", Price { input: 5.0, output: 25.0 }),
    ("claude-opus-4", Price { input: 15.0, output: 75.0 }),
    ("claude-3-opus", Price { input: 15.0, output: 75.0 }),
    ("claude-sonnet-5", Price { input: 2.0, output: 10.0 }),
    ("claude-sonnet-4", Price { input: 3.0, output: 15.0 }),
    ("claude-3-7-sonnet", Price { input: 3.0, output: 15.0 }),
    ("claude-3-5-sonnet", Price { input: 3.0, output: 15.0 }),
    // Its long-prompt (over 100K tokens) rate, so a long prompt is never
    // under-counted.
    ("claude-haiku-5-5", Price { input: 0.5, output: 2.5 }),
    ("claude-haiku-4-5", Price { input: 1.0, output: 5.0 }),
    ("claude-3-5-haiku", Price { input: 0.8, output: 4.0 }),
    ("claude-3-haiku", Price { input: 0.25, output: 1.25 }),
];

/// The price for an unknown model: the table's highest.
pub const FALLBACK: Price = Price { input: 15.0, output: 75.0 };

fn env_prices() -> Vec<(String, Price)> {
    let Ok(v) = ligus_core::compat::var("LIGUS_MODEL_PRICES") else { return Vec::new() };
    v.split(',')
        .filter_map(|e| {
            let (k, p) = e.split_once('=')?;
            let (i, o) = p.split_once('/')?;
            let price = Price { input: i.trim().parse().ok()?, output: o.trim().parse().ok()? };
            (price.input.is_finite() && price.output.is_finite() && price.input >= 0.0 && price.output >= 0.0)
                .then(|| (k.trim().to_ascii_lowercase(), price))
        })
        .collect()
}

/// The price of `model` (a provider prefix such as `anthropic/` or
/// `anthropic.` is ignored).
pub fn price_for(model: &str) -> Price {
    let m = model.trim().to_ascii_lowercase();
    let m = m.rsplit(['/']).next().unwrap_or(&m).trim_start_matches("anthropic.").to_string();
    let mut overrides = env_prices();
    overrides.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    if let Some((_, p)) = overrides.iter().find(|(k, _)| m.starts_with(k.as_str())) {
        return *p;
    }
    TABLE.iter().find(|(k, _)| m.starts_with(k)).map(|(_, p)| *p).unwrap_or(FALLBACK)
}

/// USD for `usage` at `price`.
pub fn cost(price: Price, u: Usage) -> f64 {
    let per = |n: u64, rate: f64| n as f64 * rate / 1_000_000.0;
    per(u.input, price.input) + per(u.output, price.output) + per(u.cache_write, price.input * 1.25) + per(u.cache_read, price.input * 0.1)
}

/// Charge `usage` of `model` to the current turn's meter.
pub fn charge(model: &str, usage: Usage) -> Result<(), SpendError> {
    crate::agent_turn::charge_spend(cost(price_for(model), usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_unknown_models_are_priced() {
        assert_eq!(price_for("claude-opus-5-5"), Price { input: 4.0, output: 20.0 });
        assert_eq!(price_for("anthropic/claude-sonnet-4-6"), Price { input: 3.0, output: 15.0 });
        assert_eq!(price_for("claude-opus-4-1-20250805"), Price { input: 15.0, output: 75.0 });
        assert_eq!(price_for("some-new-model"), FALLBACK);
        let c = cost(Price { input: 4.0, output: 20.0 }, Usage { input: 1_000_000, output: 100_000, cache_write: 0, cache_read: 1_000_000 });
        assert!((c - (4.0 + 2.0 + 0.4)).abs() < 1e-9, "{c}");
    }
}
