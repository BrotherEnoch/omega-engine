// crates/omega-ui/src/components/footer.rs
//
// Status bar footer: live counts of operational / degraded / halted layers.

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
                <span>{move || format!("{:02} / {} OPERATIONAL", ok_count(), total())}</span>
            </div>
            <span class="foot-sep">"│"</span>
            <div class="foot-item">
                <span class="dot-warn">"●"</span>
                <span>{move || format!("{:02} DEGRADED", warn_count())}</span>
            </div>
            <span class="foot-sep">"│"</span>
            <div class="foot-item">
                <span class="dot-halt">"●"</span>
                <span>{move || format!("{:02} HALTED", halt_count())}</span>
            </div>
            <span class="foot-spacer"></span>
            <span class="foot-copy">"OMEGA ENGINE © 2026  —  CONFIDENTIAL  —  INSTITUTIONAL USE ONLY"</span>
        </footer>
    }
}