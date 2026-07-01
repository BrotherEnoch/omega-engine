// crates/omega-ui/src/components/footer.rs
//
// Summary bar: live counts of operational / degraded / halted layers,
// styled as the reference design's summary-bar + bottom footer strip.

use leptos::*;
use omega_frontend::render::{RenderFrame, Severity};

#[component]
pub fn Footer(frame: ReadSignal<RenderFrame>) -> impl IntoView {
    let ok_count   = move || frame.get().layers.iter().filter(|r| r.severity == Severity::Ok).count();
    let warn_count = move || frame.get().layers.iter().filter(|r| r.severity == Severity::Warning).count();
    let halt_count = move || frame.get().layers.iter().filter(|r| r.severity == Severity::Critical).count();
    let total      = move || frame.get().layers.len();

    view! {
        <footer class="footer">
            <div class="foot-item">
                <span class="dot-ok">"●"</span>
                {move || format!("{} HEALTHY", ok_count())}
            </div>
            <span class="foot-sep">"│"</span>
            <div class="foot-item">
                <span class="dot-warn">"●"</span>
                {move || format!("{} DEGRADED", warn_count())}
            </div>
            <span class="foot-sep">"│"</span>
            <div class="foot-item">
                <span class="dot-halt">"●"</span>
                {move || format!("{} HALTED", halt_count())}
            </div>
            <span class="foot-sep">"│"</span>
            <div class="foot-item" style="color:var(--label);font-weight:400">
                {move || format!("{} TOTAL", total())}
            </div>
            <span class="foot-spacer"></span>
            <span class="foot-copy">"OMEGA ENGINE v12 — CONTROL PLANE"</span>
        </footer>
    }
}