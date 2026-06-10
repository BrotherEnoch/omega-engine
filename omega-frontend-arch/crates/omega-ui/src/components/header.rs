// crates/omega-ui/src/components/header.rs
use leptos::*;
use omega_frontend::render::{RenderFrame, Severity};

#[component]
pub fn Header(frame: ReadSignal<RenderFrame>) -> impl IntoView {
    let overall_class = move || match frame.get().overall_severity {
        Severity::Ok       => "overall-status status-ok",
        Severity::Warning  => "overall-status status-warn",
        Severity::Critical => "overall-status status-halt",
        Severity::Unknown  => "overall-status status-unk",
    };
    let overall_text = move || match frame.get().overall_severity {
        Severity::Ok       => "OPERATIONAL",
        Severity::Warning  => "DEGRADED",
        Severity::Critical => "HALTED",
        Severity::Unknown  => "INITIALISING",
    };
    let ws_class  = move || if frame.get().ws_connected { "ws-indicator ws-live" } else { "ws-indicator ws-poll" };
    let ws_label  = move || frame.get().ws_status_label;
    let revision  = move || format!("REV {:06}", frame.get().revision);

    view! {
        <header class="header">
            <div class="header-left">
                <span class="logo">"ΩMEGA"</span>
                <span class="logo-sub">"ENGINE v12.0 — CONTROL PLANE"</span>
            </div>
            <div class="header-center">
                <span class="overall-label">"SYSTEM STATUS"</span>
                <span class=overall_class>{overall_text}</span>
            </div>
            <div class="header-right">
                <span class=ws_class>{move || format!("● {}", ws_label())}</span>
                <span class="revision">{revision}</span>
            </div>
        </header>
    }
}