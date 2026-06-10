//! Shared control-plane contracts used by the backend and Rust/WASM frontend.
//!
//! `proto` is generated from `ops/control-plane/proto/omega_control.proto`.
//! `rest` and `ws` hold the JSON contracts that are not represented in that
//! proto file today.

pub mod proto;
pub mod rest;
pub mod routes;
pub mod ws;
