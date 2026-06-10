// omega-frontend-arch/crates/omega-ui/src/main.rs
//
// WASM entrypoint for omega-ui (Trunk + Leptos)

#![forbid(unsafe_code)]
#![no_main]   // ← This is the key fix for the duplicate main error

use leptos::*;
use wasm_bindgen::prelude::wasm_bindgen;

use omega_ui::App;
use web_sys::console;

#[wasm_bindgen(start)]
pub fn main() {
    // Forward Rust panics to browser console
    console_error_panic_hook::set_once();

    // Runtime banner
    console::info_1(
        &format!(
            "omega-ui v{} ({}) — WASM runtime starting",
            env!("CARGO_PKG_VERSION"),
            if cfg!(debug_assertions) { "debug" } else { "release" },
        )
        .into(),
    );

    // Mount the Leptos app
    mount_to_body(|| {
        view! {
            <App />
        }
    });
}