// omega-frontend-arch/crates/omega-ui/src/components/layer_grid.rs
//
// Layer health grid.
//
// Leptos <For> reuses DOM nodes when keys are stable, so children closures
// that capture a value-typed LayerRow by move will never update after the
// first render.  Instead we iterate over the stable LayerId list and read
// the current frame signal reactively inside each card closure, so every
// signal update re-evaluates the card contents.

use leptos::*;
use strum::IntoEnumIterator;
use omega_control_contracts::health::LayerId;
use omega_frontend::render::{RenderFrame, Severity};

/// Wire tag for each layer — matches the L-tag emitted by the backend's
/// WS layer_event payloads and used by layer_id_from_wire in ws.rs.
///
/// Order matches the canonical v12 LayerId enum (health.rs):
///   L00 SystemHealth, L01 ExternalData, L02 Oracle, L03 Risk, L04 Security,
///   L05 Eil, L06 Dag, L07 Zk, L08 HotPath, L09 Strategy, L10 Flashloan,
///   L11 Orchestrator, L12 Relay, L13 Vault, L14 Observability, L15 LossAttribution
fn layer_tag(layer: LayerId) -> &'static str {
    use LayerId::*;
    match layer {
        SystemHealth    => "L00",
        ExternalData    => "L01",
        Oracle          => "L02",
        Risk            => "L03",
        Security        => "L04",
        Eil             => "L05",
        Dag             => "L06",
        Zk              => "L07",
        HotPath         => "L08",
        Strategy        => "L09",
        Flashloan       => "L10",
        Orchestrator    => "L11",
        Relay           => "L12",
        Vault           => "L13",
        Observability   => "L14",
        LossAttribution => "L15",
    }
}

fn layer_label(layer: LayerId) -> &'static str {
    use LayerId::*;
    match layer {
        SystemHealth    => "Health FSM",
        ExternalData    => "RPC / Nodes",
        Oracle          => "Oracle Feeds",
        Risk            => "Risk Engine",
        Security        => "Security",
        Eil             => "Compliance / EIL",
        Dag             => "DAG Planner",
        Zk              => "ZK Prover",
        HotPath         => "Hot Path",
        Strategy        => "Strategy",
        Flashloan       => "Flash Loan",
        Orchestrator    => "Orchestrator",
        Relay           => "Relay",
        Vault           => "Vault",
        Observability   => "Observability",
        LossAttribution => "Loss Attribution",
    }
}

#[component]
pub fn LayerGrid(frame: ReadSignal<RenderFrame>) -> impl IntoView {
    // Stable list — never changes, so we can collect once and iterate.
    let layers: Vec<LayerId> = LayerId::iter().collect();

    let cards: Vec<_> = layers
        .into_iter()
        .map(|layer| {
            let ltag   = layer_tag(layer);
            let llabel = layer_label(layer);

            // All reactive reads happen inside these closures, which are
            // re-evaluated every time `frame` signal fires.
            let card_class = move || {
                let sev = frame.get()
                    .layers
                    .iter()
                    .find(|r| r.layer == layer)
                    .map(|r| r.severity)
                    .unwrap_or(Severity::Unknown);
                match sev {
                    Severity::Ok       => "layer-card status-ok",
                    Severity::Warning  => "layer-card status-warn",
                    Severity::Critical => "layer-card status-halt",
                    Severity::Unknown  => "layer-card status-unk",
                }
            };

            let tag_text = move || {
                let sev = frame.get()
                    .layers
                    .iter()
                    .find(|r| r.layer == layer)
                    .map(|r| r.severity)
                    .unwrap_or(Severity::Unknown);
                match sev {
                    Severity::Ok       => "HEALTHY",
                    Severity::Warning  => "DEGRADED",
                    Severity::Critical => "HALTED",
                    Severity::Unknown  => "UNKNOWN",
                }
            };

            let message = move || {
                frame.get()
                    .layers
                    .iter()
                    .find(|r| r.layer == layer)
                    .and_then(|r| r.message.clone())
                    .filter(|m| {
                        // Guard: some runtime layers emit the status string
                        // ("healthy", "HEALTHY") as their message when no tick
                        // message is available. Don't display those.
                        let m = m.to_ascii_lowercase();
                        m != "healthy" && m != "unknown" && m != "degraded"
                            && m != "halted" && m != "recovering" && !m.is_empty()
                    })
                    .unwrap_or_default()
            };

            view! {
                <div class=card_class>
                    <div class="layer-info">
                        <div class="layer-name">
                            <span class="layer-id">{ltag}</span>
                            <span class="layer-label">{llabel}</span>
                        </div>
                        <div class="layer-msg">{message}</div>
                    </div>
                    <span class="layer-tag">{tag_text}</span>
                </div>
            }
        })
        .collect();

    view! {
        <div class="section-label">"SUPERVISOR"</div>
        <div class="layer-grid">
            {cards}
        </div>
    }
}