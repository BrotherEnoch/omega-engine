// crates/omega-ui/src/components/header.rs
use leptos::*;
use omega_frontend::render::RenderFrame;

#[component]
pub fn Header(frame: ReadSignal<RenderFrame>) -> impl IntoView {
    let ws_class = move || {
        if frame.get().ws_connected { "ws-indicator ws-live" } else { "ws-indicator ws-poll" }
    };
    let ws_label = move || {
        if frame.get().ws_connected { "LIVE" } else { frame.get().ws_status_label }
    };
    let revision = move || format!("REV {:07}", frame.get().revision);

    view! {
        <header class="header">
            <div class="header-left">
                <span class="logo">
                    <span style="color:var(--green)">"Ω "</span>
                    "OMEGA RUNTIME"
                </span>
            </div>
            <div class="header-right">
                <span class=ws_class>{ws_label}</span>
                <span class="revision">{revision}</span>
            </div>
        </header>
        <div class="subtitle-bar">"16-layer control plane monitor"</div>
    }
}