// ops/control-plane/src/obs_bridge.rs
//
// Observability bridge — drains the shared `EventRingBuffer` (populated
// by engine crates via `omega-observability`) and republishes mapped
// `WsEvent`s onto `state.ws_tx`, so dashboard clients connected to
// `/ws/events` see live trading telemetry alongside governance events.
//
// In standalone control-plane mode (no live engine attached) the ring
// buffer is always empty and this task is effectively a no-op.  When
// the full engine is wired in, the drain loop below should be extended
// to map concrete `OmegaEvent` variants to the appropriate `WsEvent`
// variants and call `state.publish(mapped)`.

use std::sync::Arc;
use std::time::Duration;

use crate::AppState;

/// Spawn the observability bridge as a background Tokio task.
///
/// Called once from `main()` after `AppState` is constructed.
/// The task runs until the process exits.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        loop {
            ticker.tick().await;
            // TODO: drain state.obs_buffer, map OmegaEvent → WsEvent,
            // and call state.publish(mapped_event) for each.
            // Requires the full engine to populate the EventRingBuffer.
            //
            // Example once OmegaEvent variants are known:
            //   for raw in state.obs_buffer.drain() {
            //       if let Some(ws_event) = map_omega_event(raw) {
            //           state.publish(ws_event);
            //       }
            //   }
            let _ = &state.obs_buffer; // suppress unused-field lint
        }
    });
}
