//! M6 family (d): a scripted 500-turn FakeProvider session, for RSS sampling.
//!
//! Runs the agent harness through a fixed number of turns against the in-process
//! [`FakeProvider`] (no network, no tools), accumulating the full transcript in
//! memory — the tau counterpart is `tools/bench/tau_rss_session.py`. Neither
//! program prints timing; peak resident set size is captured by running each
//! under `/usr/bin/time -l` (see `tools/bench/rss.sh`). The comparison is
//! deliberately allocator-honest: it measures the steady-state footprint of an
//! accumulating transcript + per-event snapshots, not a synthetic stress test.
//!
//! Turn count is overridable via argv[1] (default 500).
#![allow(missing_docs, clippy::doc_markdown)]

use std::sync::Arc;

use futures::StreamExt;
use rho_agent::fake::FakeProvider;
use rho_agent::harness::{AgentHarness, AgentHarnessConfig};
use rho_agent::messages::{AssistantContent, AssistantMessage, TextContent};
use rho_agent::provider::ModelProvider;
use rho_agent::provider_events::{
    AssistantDoneEvent, AssistantMessageEvent, AssistantStartEvent, DoneReason,
};

/// One scripted assistant turn: `start` then a `done` carrying a short reply.
fn scripted_turn(text: &str) -> Vec<AssistantMessageEvent> {
    let message = AssistantMessage::new(vec![AssistantContent::Text(TextContent::new(text))])
        .with_model("fake");
    vec![
        AssistantMessageEvent::Start(AssistantStartEvent::new(
            AssistantMessage::new(Vec::new()).with_model("fake"),
        )),
        AssistantMessageEvent::Done(AssistantDoneEvent::new(DoneReason::Stop, message)),
    ]
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let turns: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(500);

    // One canned stream per provider call: one for the initial prompt plus one
    // per continuation turn.
    let streams: Vec<Vec<AssistantMessageEvent>> = (0..turns)
        .map(|i| scripted_turn(&format!("reply {i}")))
        .collect();
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::new(streams));

    let config = AgentHarnessConfig::new(provider, "fake", "You are Tau.");
    let harness = AgentHarness::new(config, Vec::new());

    // Turn 1 is a fresh user prompt; the rest continue the same transcript, so
    // messages accumulate across all `turns` provider calls.
    let mut stream = harness.prompt("go").expect("prompt starts");
    while stream.next().await.is_some() {}
    for _ in 1..turns {
        let mut stream = harness.continue_().expect("continue turn");
        while stream.next().await.is_some() {}
    }

    // Print the final transcript size so the run can't be optimized away and so
    // the sampler can sanity-check it drove the expected number of turns.
    println!("turns={turns} messages={}", harness.messages().len());
}
