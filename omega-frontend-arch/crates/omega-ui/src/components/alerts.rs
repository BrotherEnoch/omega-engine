// crates/omega-ui/src/components/alerts.rs
//
// Alert banner list. Renders zero, one, or more banners depending on
// active engine conditions (halted layers, gas model pause, vault timelock).

use leptos::*;
use omega_frontend::render::{RenderFrame, Severity};

#[component]
pub fn Alerts(frame: ReadSignal<RenderFrame>) -> impl IntoView {
    let alerts = move || frame.get().alerts;

    view! {
        <div class="alert-zone">
            <For
                each=alerts
                key=|a| a.message.clone()
                children=|alert| {
                    let cls = match alert.severity {
                        Severity::Critical => "alert alert-critical",
                        Severity::Warning  => "alert alert-warning",
                        _                  => "alert alert-warning",
                    };
                    let badge = match alert.severity {
                        Severity::Critical => "▲ CRITICAL",
                        _                  => "▲ WARNING",
                    };
                    view! {
                        <div class=cls>
                            <span class="alert-badge">{badge}</span>
                            <span>{alert.message}</span>
                        </div>
                    }
                }
            />
        </div>
    }
}