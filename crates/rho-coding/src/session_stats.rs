//! Lifetime activity and usage totals for an active session branch (port of tau
//! `tau_coding/session_stats.py`).
//!
//! Aggregates every original-branch message — including messages later replaced
//! by compaction — into turn / tool-call counts, prompt / output token totals,
//! and a best-effort estimated USD cost. The cost estimate is `None` unless
//! *every* billable response has resolvable pricing (matching tau's
//! all-or-nothing `has_complete_pricing` gate).

use indexmap::IndexMap;

use rho_agent::messages::AgentMessage;
use rho_agent::session::entries::SessionEntry;

const TOKENS_PER_MILLION: f64 = 1_000_000.0;

/// Resolves per-million-token rates for one response, keyed by
/// `input`/`output`/`cacheRead`/`cacheWrite`. Returns `None` when the model has
/// no known pricing (tau `PricingResolver`).
pub type PricingResolver<'a> = dyn Fn(&str, &str, i64) -> Option<IndexMap<String, f64>> + 'a;

/// Cumulative activity and billed usage for one active branch (tau
/// `SessionStats`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SessionStats {
    /// Number of user/custom turns.
    pub turn_count: usize,
    /// Number of tool calls issued across assistant responses.
    pub tool_call_count: usize,
    /// Prompt tokens (input + cache-read + cache-write).
    pub input_tokens: i64,
    /// Output (completion) tokens.
    pub output_tokens: i64,
    /// Best-effort estimated USD cost, or `None` when pricing is incomplete.
    pub estimated_cost: Option<f64>,
}

/// Aggregate original branch messages, including messages replaced by compaction
/// (tau `calculate_session_stats`).
#[must_use]
pub fn calculate_session_stats(
    entries: &[SessionEntry],
    pricing: &PricingResolver<'_>,
) -> SessionStats {
    let mut turn_count = 0usize;
    let mut tool_call_count = 0usize;
    let mut input_tokens = 0i64;
    let mut output_tokens = 0i64;
    let mut estimated_cost = 0.0f64;
    let mut has_billable_usage = false;
    let mut has_complete_pricing = true;

    for entry in entries {
        let SessionEntry::Message(message_entry) = entry else {
            continue;
        };
        let message = &message_entry.message;
        match message {
            AgentMessage::User(_) | AgentMessage::Custom(_) => {
                turn_count += 1;
            }
            AgentMessage::Assistant(assistant) => {
                tool_call_count += assistant.tool_calls().len();
                let usage = &assistant.usage;
                let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
                input_tokens += prompt_tokens;
                output_tokens += usage.output;
                if prompt_tokens == 0 && usage.output == 0 {
                    continue;
                }

                has_billable_usage = true;
                let rates = pricing(&assistant.provider, &assistant.model, prompt_tokens);
                match rates {
                    None => {
                        if usage.cost.total > 0.0 {
                            estimated_cost += usage.cost.total;
                        } else {
                            has_complete_pricing = false;
                        }
                    }
                    Some(rates) => {
                        estimated_cost += response_cost(
                            usage.input,
                            usage.output,
                            usage.cache_read,
                            usage.cache_write,
                            &rates,
                        );
                    }
                }
            }
            // ToolResult and any other non-turn message shapes are ignored, as in
            // tau (which only branches on User/Custom/Assistant).
            _ => {}
        }
    }

    SessionStats {
        turn_count,
        tool_call_count,
        input_tokens,
        output_tokens,
        estimated_cost: if has_billable_usage && has_complete_pricing {
            Some(estimated_cost)
        } else {
            None
        },
    }
}

/// Calculate one response's estimated USD cost from per-million-token rates (tau
/// `_response_cost`).
#[allow(clippy::cast_precision_loss)] // token counts are far below f64's 2^52 exact-integer bound
fn response_cost(
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    rates: &IndexMap<String, f64>,
) -> f64 {
    let rate = |key: &str| rates.get(key).copied().unwrap_or(0.0);
    (input_tokens as f64 * rate("input")
        + output_tokens as f64 * rate("output")
        + cache_read_tokens as f64 * rate("cacheRead")
        + cache_write_tokens as f64 * rate("cacheWrite"))
        / TOKENS_PER_MILLION
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_agent::messages::{AssistantMessage, CustomMessage, Usage, UserMessage};
    use rho_agent::session::entries::MessageEntry;

    fn rates(input: f64, output: f64) -> IndexMap<String, f64> {
        let mut map = IndexMap::new();
        map.insert("input".to_string(), input);
        map.insert("output".to_string(), output);
        map
    }

    fn user_entry(text: &str) -> SessionEntry {
        SessionEntry::Message(MessageEntry::new(AgentMessage::User(UserMessage::new(
            text,
        ))))
    }

    fn custom_entry() -> SessionEntry {
        SessionEntry::Message(MessageEntry::new(AgentMessage::Custom(CustomMessage::new(
            "note", "hi",
        ))))
    }

    fn assistant_entry(provider: &str, model: &str, usage: Usage) -> SessionEntry {
        let assistant = AssistantMessage::new(Vec::new())
            .with_provider(provider)
            .with_model(model)
            .with_usage(usage);
        SessionEntry::Message(MessageEntry::new(AgentMessage::Assistant(assistant)))
    }

    #[test]
    fn counts_turns_and_tool_calls() {
        let entries = vec![
            user_entry("hi"),
            custom_entry(),
            assistant_entry(
                "anthropic",
                "claude",
                Usage {
                    input: 100,
                    output: 50,
                    ..Usage::default()
                },
            ),
        ];
        let stats = calculate_session_stats(&entries, &|_p, _m, _t| None);
        assert_eq!(stats.turn_count, 2);
        assert_eq!(stats.input_tokens, 100);
        assert_eq!(stats.output_tokens, 50);
    }

    #[test]
    fn estimates_cost_from_rates() {
        let entries = vec![assistant_entry(
            "anthropic",
            "claude",
            Usage {
                input: 1_000_000,
                output: 1_000_000,
                ..Usage::default()
            },
        )];
        let stats = calculate_session_stats(&entries, &|_p, _m, _t| Some(rates(3.0, 15.0)));
        assert_eq!(stats.estimated_cost, Some(18.0));
    }

    #[test]
    fn missing_pricing_without_billed_cost_yields_none() {
        let entries = vec![assistant_entry(
            "anthropic",
            "claude",
            Usage {
                input: 100,
                output: 50,
                ..Usage::default()
            },
        )];
        let stats = calculate_session_stats(&entries, &|_p, _m, _t| None);
        assert_eq!(stats.estimated_cost, None);
    }

    #[test]
    fn falls_back_to_billed_cost_when_no_rates() {
        let mut usage = Usage {
            input: 100,
            output: 50,
            ..Usage::default()
        };
        usage.cost.total = 0.25;
        let entries = vec![assistant_entry("anthropic", "claude", usage)];
        let stats = calculate_session_stats(&entries, &|_p, _m, _t| None);
        assert_eq!(stats.estimated_cost, Some(0.25));
    }

    #[test]
    fn zero_usage_response_is_not_billable() {
        let entries = vec![assistant_entry("anthropic", "claude", Usage::default())];
        let stats = calculate_session_stats(&entries, &|_p, _m, _t| None);
        assert_eq!(stats.estimated_cost, None);
        assert_eq!(stats.tool_call_count, 0);
    }
}
