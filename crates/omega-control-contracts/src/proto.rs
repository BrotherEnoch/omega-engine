//! Prost message types matching `ops/control-plane/proto/omega_control.proto`.
//!
//! These are kept in a shared crate so frontend code can consume the same
//! binary contract shape as backend gRPC. In environments where `protoc` is
//! available, this module can be regenerated from the proto without changing
//! downstream imports.

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Empty {}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CommandResult {
    #[prost(bool, tag = "1")]
    pub ok: bool,
    #[prost(string, tag = "2")]
    pub message: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct HealthReport {
    #[prost(message, repeated, tag = "1")]
    pub layers: ::prost::alloc::vec::Vec<LayerHealth>,
    #[prost(bool, tag = "2")]
    pub system_halted: bool,
    #[prost(string, tag = "3")]
    pub generated_at: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LayerHealth {
    #[prost(string, tag = "1")]
    pub layer_id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub state: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub reason: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct HealthEvent {
    #[prost(string, tag = "1")]
    pub layer_id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub from: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub to: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub reason: ::prost::alloc::string::String,
    #[prost(string, tag = "5")]
    pub timestamp: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PnLRequest {
    #[prost(string, tag = "1")]
    pub chain_id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub strategy_id: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PnLReport {
    #[prost(double, tag = "1")]
    pub gross_profit_eth: f64,
    #[prost(double, tag = "2")]
    pub gas_cost_eth: f64,
    #[prost(double, tag = "3")]
    pub net_profit_eth: f64,
    #[prost(double, tag = "4")]
    pub dao_fee_eth: f64,
    #[prost(double, tag = "5")]
    pub pil_share_eth: f64,
    #[prost(string, tag = "6")]
    pub period: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LatencyReport {
    #[prost(message, repeated, tag = "1")]
    pub layers: ::prost::alloc::vec::Vec<LayerLatency>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LayerLatency {
    #[prost(string, tag = "1")]
    pub layer_id: ::prost::alloc::string::String,
    #[prost(double, tag = "2")]
    pub p50_us: f64,
    #[prost(double, tag = "3")]
    pub p95_us: f64,
    #[prost(double, tag = "4")]
    pub p99_us: f64,
    #[prost(double, tag = "5")]
    pub budget_us: f64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct QueueReport {
    #[prost(int32, tag = "1")]
    pub microtx_slots: i32,
    #[prost(int32, tag = "2")]
    pub normal_slots: i32,
    #[prost(int32, tag = "3")]
    pub zk_queue_depth: i32,
    #[prost(int32, tag = "4")]
    pub relay_queue_depth: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WinRateReport {
    #[prost(message, repeated, tag = "1")]
    pub relays: ::prost::alloc::vec::Vec<RelayWinRate>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RelayWinRate {
    #[prost(string, tag = "1")]
    pub relay: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub strategy: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub chain: ::prost::alloc::string::String,
    #[prost(double, tag = "4")]
    pub rate_24h: f64,
    #[prost(int64, tag = "5")]
    pub sample_count: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct StrategyId {
    #[prost(string, tag = "1")]
    pub id: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LayerIdMsg {
    #[prost(string, tag = "1")]
    pub id: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RolloutTier {
    #[prost(double, tag = "1")]
    pub fraction: f64,
}
