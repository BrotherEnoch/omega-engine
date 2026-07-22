use leptos::*;
use omega_frontend::render::RenderFrame;

#[component]
pub fn ObsPanel(frame: ReadSignal<RenderFrame>) -> impl IntoView {
    let profit_splits = move || frame.with(|f| f.obs_metrics.profit_splits);
    let gas_reverts   = move || frame.with(|f| f.obs_metrics.gas_model_reverts);
    let ceiling_esc   = move || frame.with(|f| f.obs_metrics.ceiling_escalations);
    let emerg_skip    = move || frame.with(|f| f.obs_metrics.emergency_bundles_skipped);
    let reorg_risk    = move || frame.with(|f| f.obs_metrics.la_reorg_risks);
    let sim_errors    = move || frame.with(|f| f.obs_metrics.simulation_errors);
    let vs_gaps       = move || frame.with(|f| f.obs_metrics.ws_gaps);
    let entries       = move || frame.with(|f| f.obs_entries.clone());
    let has_entries   = move || frame.with(|f| !f.obs_entries.is_empty());

    view! {
        <section class="obs-panel" style="width:100%;margin-top:1rem;background:#0d1a0d;border:1px solid #1a3a1a;border-radius:4px;overflow:hidden;">
            <div style="padding:0.35rem 0.8rem;border-bottom:1px solid #1a3a1a;">
                <span style="font-family:monospace;font-size:0.65rem;letter-spacing:0.14em;color:#4a7c4a;">"TRADING ENGINE TELEMETRY"</span>
            </div>
            <div style="display:flex;flex-direction:row;align-items:center;padding:0.45rem 0.8rem;border-bottom:1px solid #1a3a1a;">
                <div style="display:flex;flex-direction:column;align-items:center;min-width:6rem;">
                    <span style="font-family:monospace;font-size:0.95rem;color:#4caf50;">{profit_splits}</span>
                    <span style="font-family:monospace;font-size:0.5rem;color:#4a7c4a;">"PROFIT SPLITS"</span>
                </div>
                <span style="color:#2a4a2a;padding:0 0.3rem;">" | "</span>
                <div style="display:flex;flex-direction:column;align-items:center;min-width:6rem;">
                    <span style="font-family:monospace;font-size:0.95rem;color:#ffa000;">{gas_reverts}</span>
                    <span style="font-family:monospace;font-size:0.5rem;color:#4a7c4a;">"GAS REVERTS"</span>
                </div>
                <span style="color:#2a4a2a;padding:0 0.3rem;">" | "</span>
                <div style="display:flex;flex-direction:column;align-items:center;min-width:6rem;">
                    <span style="font-family:monospace;font-size:0.95rem;color:#ff6f00;">{ceiling_esc}</span>
                    <span style="font-family:monospace;font-size:0.5rem;color:#4a7c4a;">"CEILING ESC"</span>
                </div>
                <span style="color:#2a4a2a;padding:0 0.3rem;">" | "</span>
                <div style="display:flex;flex-direction:column;align-items:center;min-width:6rem;">
                    <span style="font-family:monospace;font-size:0.95rem;color:#f44336;">{emerg_skip}</span>
                    <span style="font-family:monospace;font-size:0.5rem;color:#4a7c4a;">"EMERG SKIP"</span>
                </div>
                <span style="color:#2a4a2a;padding:0 0.3rem;">" | "</span>
                <div style="display:flex;flex-direction:column;align-items:center;min-width:6rem;">
                    <span style="font-family:monospace;font-size:0.95rem;color:#ff9800;">{reorg_risk}</span>
                    <span style="font-family:monospace;font-size:0.5rem;color:#4a7c4a;">"REORG RISK"</span>
                </div>
                <span style="color:#2a4a2a;padding:0 0.3rem;">" | "</span>
                <div style="display:flex;flex-direction:column;align-items:center;min-width:6rem;">
                    <span style="font-family:monospace;font-size:0.95rem;color:#f44336;">{sim_errors}</span>
                    <span style="font-family:monospace;font-size:0.5rem;color:#4a7c4a;">"SIM ERRORS"</span>
                </div>
                <span style="color:#2a4a2a;padding:0 0.3rem;">" | "</span>
                <div style="display:flex;flex-direction:column;align-items:center;min-width:6rem;">
                    <span style="font-family:monospace;font-size:0.95rem;color:#7e57c2;">{vs_gaps}</span>
                    <span style="font-family:monospace;font-size:0.5rem;color:#4a7c4a;">"VS GAPS"</span>
                </div>
            </div>
            <div style="padding:0.45rem 0.8rem;min-height:2.5rem;">
                {move || if !has_entries() {
                    view! {
                        <p style="font-family:monospace;font-size:0.6rem;color:#3a5a3a;text-align:center;margin:0;">
                            "No trading events recorded yet -- engine may be idle or telemetry events (ProfitSplit, GasModelReverted, etc.) not yet emitted by the backend."
                        </p>
                    }.into_view()
                } else {
                    view! {
                        <ul style="list-style:none;padding:0;margin:0;">
                            <For
                                each=entries
                                key=|e| e.ts.clone()
                                children=move |e| {
                                    let kind    = e.kind;
                                    let ts      = e.ts.clone();
                                    let summary = e.summary.clone();
                                    let detail  = e.detail.clone();
                                    view! {
                                        <li style="display:flex;gap:0.6rem;font-family:monospace;font-size:0.6rem;padding:2px 0;border-bottom:1px solid #0a150a;">
                                            <span style="color:#3a5a3a;">{ts}</span>
                                            <span style="color:#4caf50;">{kind}</span>
                                            <span style="color:#8a9a8a;">{summary}</span>
                                            {detail.map(|d| view! {
                                                <span style="color:#5a7a5a;">{d}</span>
                                            })}
                                        </li>
                                    }
                                }
                            />
                        </ul>
                    }.into_view()
                }}
            </div>
        </section>
    }
}
