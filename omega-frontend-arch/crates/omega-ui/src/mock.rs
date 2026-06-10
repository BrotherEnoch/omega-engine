// crates/omega-ui/src/mock.rs
use chrono::Utc;

use omega_control_contracts::{
    health::{HealthStatus, LayerId},
    rest::{HealthSnapshot, LayerHealthEntry},
};
use omega_frontend::state::EngineState;

pub struct MockAdapter;

impl MockAdapter {
    pub fn new() -> Self { Self }

    pub fn initial_state(&self) -> EngineState {
        let entries: &[(LayerId, HealthStatus, Option<&str>)] = &[
            (LayerId::SystemHealth,    HealthStatus::Ok,       None),
            (LayerId::ExternalData,    HealthStatus::Ok,       None),
            (LayerId::Eil,             HealthStatus::Ok,       None),
            (LayerId::Risk,            HealthStatus::Degraded, Some("latency_spike: 94ms p99")),
            (LayerId::Security,        HealthStatus::Ok,       None),
            (LayerId::ChaosGuard,      HealthStatus::Ok,       None),
            (LayerId::Dag,             HealthStatus::Ok,       None),
            (LayerId::Zk,              HealthStatus::Ok,       None),
            (LayerId::HotPath,         HealthStatus::Halted,   Some("relay_timeout: no response in 500ms")),
            (LayerId::Strategy,        HealthStatus::Ok,       None),
            (LayerId::Flashloan,       HealthStatus::Ok,       None),
            (LayerId::Orchestrator,    HealthStatus::Ok,       None),
            (LayerId::Relay,           HealthStatus::Ok,       None),
            (LayerId::Vault,           HealthStatus::Ok,       None),
            (LayerId::Observability,   HealthStatus::Ok,       None),
            (LayerId::LossAttribution, HealthStatus::Degraded, Some("GAS_MODEL_CEILING_REACHED: 101 hits on ARBITRUM_LA")),
        ];
        build_state(entries, 1)
    }

    pub fn tick(current: EngineState) -> EngineState {
        static COUNTER: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(0);
        let t = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let risk_status = if t % 6 < 3 { HealthStatus::Degraded } else { HealthStatus::Ok };
        let la_status   = if t % 8 < 4 { HealthStatus::Degraded } else { HealthStatus::Ok };
        let hp_status   = match t % 10 {
            0..=2 => HealthStatus::Halted,
            3..=5 => HealthStatus::Degraded,
            _     => HealthStatus::Ok,
        };

        let entries: &[(LayerId, HealthStatus, Option<&str>)] = &[
            (LayerId::SystemHealth,    HealthStatus::Ok, None),
            (LayerId::ExternalData,    HealthStatus::Ok, None),
            (LayerId::Eil,             HealthStatus::Ok, None),
            (LayerId::Risk,            risk_status,      Some("latency_spike: 94ms p99")),
            (LayerId::Security,        HealthStatus::Ok, None),
            (LayerId::ChaosGuard,      HealthStatus::Ok, None),
            (LayerId::Dag,             HealthStatus::Ok, None),
            (LayerId::Zk,              HealthStatus::Ok, None),
            (LayerId::HotPath,         hp_status,        Some("relay_timeout: no response in 500ms")),
            (LayerId::Strategy,        HealthStatus::Ok, None),
            (LayerId::Flashloan,       HealthStatus::Ok, None),
            (LayerId::Orchestrator,    HealthStatus::Ok, None),
            (LayerId::Relay,           HealthStatus::Ok, None),
            (LayerId::Vault,           HealthStatus::Ok, None),
            (LayerId::Observability,   HealthStatus::Ok, None),
            (LayerId::LossAttribution, la_status,        Some("GAS_MODEL_CEILING_REACHED: 101 hits on ARBITRUM_LA")),
        ];

        let next_rev = current.revision() + 1;
        let next = build_state(entries, next_rev);
        // accept_health_snapshot returns None if revision is not newer — use next directly
        next
    }
}

fn build_state(entries: &[(LayerId, HealthStatus, Option<&str>)], _revision: u64) -> EngineState {
    let layers: Vec<LayerHealthEntry> = entries.iter().map(|(id, status, _msg)| LayerHealthEntry {
        layer:          id.backend_str().to_string(),
        state:          backend_status(*status).to_string(),
        is_operational: !matches!(status, HealthStatus::Degraded | HealthStatus::Halted),
    }).collect();

    let overall = entries.iter().fold(HealthStatus::Ok, |acc, (_, status, _)| match (acc, *status) {
        (_, HealthStatus::Halted)   | (HealthStatus::Halted,   _) => HealthStatus::Halted,
        (_, HealthStatus::Degraded) | (HealthStatus::Degraded, _) => HealthStatus::Degraded,
        _ => HealthStatus::Ok,
    });

    let snapshot = HealthSnapshot {
        generated_at:  Utc::now(),
        layers,
        system_halted: overall == HealthStatus::Halted,
    };

    EngineState::default()
        .accept_health_snapshot(&snapshot)
        .unwrap_or_default()
}

fn backend_status(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::Ok         => "HEALTHY",
        HealthStatus::Degraded   => "DEGRADED",
        HealthStatus::Halted     => "HALTED",
        HealthStatus::Recovering => "RECOVERING",
        HealthStatus::Unknown    => "UNKNOWN",
    }
}
