// omega-frontend-arch/crates/omega-runtime/src/main.rs — omega-runtime entry point

mod health;
mod health_fsm;
mod observability;
mod orchestrator;
mod relay;
mod external_data;
mod eil;
mod risk_engine;
mod security;
mod chaos_guard;
mod hot_path;
mod vault;
mod dag_planner;
mod zk_prover;
mod strategy;
mod flash_loan;
mod loss_attribution;
mod registry;
mod ws_server;

use std::{sync::Arc, time::Duration};

use tokio::task::JoinSet;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).with_thread_ids(true))
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    std::panic::set_hook(Box::new(|info| {
        error!("PANIC: {info:?}");
    }));

    info!("omega-runtime starting");

    let registry = registry::Registry::new();

    // ── Subsystems ────────────────────────────────────────────────────────
    // All tasks go into a single JoinSet so we can race them against the
    // server and detect any unexpected exits.
    let mut set = JoinSet::new();

    // Infrastructure tier
    set.spawn(observability::spawn(Arc::clone(&registry)));
    set.spawn(orchestrator::spawn(Arc::clone(&registry)));
    set.spawn(relay::spawn(Arc::clone(&registry)));
    set.spawn(external_data::spawn(Arc::clone(&registry)));
    set.spawn(eil::spawn(Arc::clone(&registry)));
    set.spawn(risk_engine::spawn(Arc::clone(&registry)));
    set.spawn(security::spawn(Arc::clone(&registry)));
    set.spawn(chaos_guard::spawn(Arc::clone(&registry)));
    // Execution tier
    set.spawn(hot_path::spawn(Arc::clone(&registry)));
    set.spawn(vault::spawn(Arc::clone(&registry)));
    set.spawn(dag_planner::spawn(Arc::clone(&registry)));
    set.spawn(zk_prover::spawn(Arc::clone(&registry)));
    set.spawn(strategy::spawn(Arc::clone(&registry)));
    set.spawn(flash_loan::spawn(Arc::clone(&registry)));
    set.spawn(loss_attribution::spawn(Arc::clone(&registry)));
    // Aggregate FSM — last, watches all others
    set.spawn(health_fsm::spawn(Arc::clone(&registry)));

    // ── Registry FSM tick ─────────────────────────────────────────────────
    {
        let reg = Arc::clone(&registry);
        set.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                reg.tick();
            }
        });
    }

    // ── Diagnostic loop ───────────────────────────────────────────────────
    {
        let reg = Arc::clone(&registry);
        set.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;
                let snap = reg.snapshot();
                info!(version = snap.version, layer_count = snap.layers.len(), "registry snapshot");
                let mut layers: Vec<_> = snap.layers.iter().collect();
                layers.sort_by_key(|(k, _)| k.as_str());
                for (name, ls) in &layers {
                    info!(
                        layer = %name,
                        status = %ls.status,
                        latency_ns = ls.latency_ns,
                        message = %ls.message,
                    );
                }
            }
        });
    }

    // ── WebSocket / HTTP server ───────────────────────────────────────────
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 9001));
    let app  = ws_server::router(Arc::clone(&registry));

    info!("WebSocket server listening on ws://{addr}/ws");
    info!("Snapshot endpoint: http://{addr}/snapshot");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind port 9001");

    set.spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("axum server error");
    });

    // ── Drive the JoinSet — abort all on first unexpected exit ────────────
    while let Some(result) = set.join_next().await {
        match result {
            Ok(()) => {
                // A subsystem returned — none of these should exit in normal
                // operation. Treat it as fatal and tear everything down.
                error!("a subsystem exited unexpectedly; shutting down");
                set.abort_all();
                break;
            }
            Err(e) if e.is_panic() => {
                error!("a subsystem panicked: {e:?}; shutting down");
                set.abort_all();
                break;
            }
            Err(e) => {
                // Task was cancelled (e.g. by abort_all above) — expected
                // during shutdown, not fatal on its own.
                error!("task cancelled: {e:?}");
            }
        }
    }

    info!("omega-runtime stopped");
}