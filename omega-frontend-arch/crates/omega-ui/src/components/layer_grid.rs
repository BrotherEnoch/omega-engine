// crates/omega-ui/src/components/layer_grid.rs
//
// The 16-layer health grid. Each row shows:
//   ID | Layer name | Status label | Health bar | Message
//
// Rows animate independently:
//   - HALTED rows pulse red continuously
//   - DEGRADED rows have a faint amber tint
//   - Health bars transition width smoothly (CSS transition)
//   - HALTED status label and bar strobe via CSS animation

use leptos::*;
use omega_frontend::render::{RenderFrame, Severity};
use omega_control_contracts::health::LayerId;

// ── layer ID string — L00–L15 ───────────────────────────────────────────────
fn layer_id_str(layer: LayerId) -> &'static str {
    match layer {
        LayerId::SystemHealth    => "L00",
        LayerId::ExternalData    => "L01",
        LayerId::Eil             => "L02",
        LayerId::Risk            => "L03",
        LayerId::Security        => "L04",
        LayerId::ChaosGuard      => "L05",
        LayerId::Dag             => "L06",
        LayerId::Zk              => "L07",
        LayerId::HotPath         => "L08",
        LayerId::Strategy        => "L09",
        LayerId::Flashloan       => "L10",
        LayerId::Orchestrator    => "L11",
        LayerId::Relay           => "L12",
        LayerId::Vault           => "L13",
        LayerId::Observability   => "L14",
        LayerId::LossAttribution => "L15",
    }
}

fn sev_suffix(sev: Severity) -> &'static str {
    match sev {
        Severity::Ok       => "ok",
        Severity::Warning  => "warn",
        Severity::Critical => "halt",
        Severity::Unknown  => "unk",
    }
}

fn status_label(sev: Severity) -> &'static str {
    match sev {
        Severity::Ok       => "OPERATIONAL",
        Severity::Warning  => "DEGRADED",
        Severity::Critical => "HALTED",
        Severity::Unknown  => "UNKNOWN",
    }
}

/// Bar fill width as a percentage.
fn bar_pct(sev: Severity) -> &'static str {
    match sev {
        Severity::Ok       => "100%",
        Severity::Warning  => "55%",
        Severity::Critical => "14%",
        Severity::Unknown  => "0%",
    }
}

#[component]
pub fn LayerGrid(frame: ReadSignal<RenderFrame>) -> impl IntoView {
    let rows = move || frame.get().layers;

    view! {
        <section class="layer-section">
            // ── column header ──────────────────────────────────────────────
            <div class="grid-header">
                <span>"ID"</span>
                <span>"LAYER"</span>
                <span>"STATUS"</span>
                <span>"HEALTH"</span>
                <span>"MESSAGE"</span>
            </div>

            // ── rows ───────────────────────────────────────────────────────
            <div class="grid-body">
                <For
                    each=rows
                    key=|row| format!("{:?}", row.layer)
                    children=|row| {
                        let sev        = row.severity;
                        let row_cls    = format!("layer-row row-{}", sev_suffix(sev));
                        let stat_cls   = format!("cell-status s-{}", sev_suffix(sev));
                        let bar_cls    = format!("bar-fill b-{}", sev_suffix(sev));
                        let msg        = row.message.clone().unwrap_or_default();
                        let pct        = bar_pct(sev);
                        let id_str     = layer_id_str(row.layer);

                        view! {
                            <div class=row_cls>
                                <span class="cell-id">{id_str}</span>
                                <span class="cell-name">{row.label}</span>
                                <span class=stat_cls>{status_label(sev)}</span>
                                <span class="cell-bar">
                                    <span class="bar-track">
                                        <span
                                            class=bar_cls
                                            style=move || format!("width:{}", pct)
                                        ></span>
                                    </span>
                                </span>
                                <span class="cell-msg">{msg}</span>
                            </div>
                        }
                    }
                />
            </div>
        </section>
    }
}
