// crates/omega-rpc/tests/http_polling.rs
//
// End-to-end tests for the HTTP transport path.
//
// Each test starts a tiny in-process HTTP/1.1 JSON-RPC server on 127.0.0.1 (random
// port) and points the REAL `OmegaRpcClient` / `run_*_stream` functions at it, so the
// whole path is exercised: URL validation → alloy HTTP provider → chain-ID check →
// polling loop → broadcast events. Only the public API is used.
//
// What is covered:
//   - block polling: clean successors, parent-hash mismatch, same-height replacement,
//     gap back-fill in order, recovery from HTTP 429
//   - fee oracle polling: base fee conversion, no duplicate events for an unchanged tip
//   - DEX / lending log polling: contiguous ranges (no gaps, no overlap), range
//     splitting with ascending delivery, 429 retry without losing the range, `removed`
//     logs dropped, unknown contracts dropped
//   - wrong chain ID is a fatal error
//
// Configuration: polling intervals come from OMEGA_POLL_* environment variables, which
// are process-global. They are set ONCE (see `init_env`) to the same fast values for
// every test in this file, so parallel tests cannot disagree.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use alloy_primitives::Address;
use omega_rpc::{
    run_dex_sync_stream, run_fee_oracle_stream, run_lending_protocol_stream, DexSyncEvent,
    FeeOracleEvent, LendingProtocol, LendingProtocolEvent, OmegaRpcClient, RpcClientConfig,
    RpcClientError, RpcRateLimiter, RpcTransport,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

const CHAIN_ID: u64 = 42_161;
const SYNC_TOPIC: &str = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";
const AAVE_POOL: &str = "0x794a61358d6845594f94dc1db02a252b5b4814ad";
const POOL_A: &str = "0x00000000000000000000000000000000000000a1";
const UNKNOWN_CONTRACT: &str = "0x00000000000000000000000000000000000000ff";

// ─────────────────────────────────────────────────────────────────────────────
// Environment
// ─────────────────────────────────────────────────────────────────────────────

static INIT: Once = Once::new();

fn init_env() {
    INIT.call_once(|| {
        std::env::set_var("OMEGA_POLL_BLOCKS_MS", "50");
        std::env::set_var("OMEGA_POLL_LOGS_MS", "200");
        std::env::set_var("OMEGA_LOGS_MAX_RANGE", "1000");
        std::env::set_var("OMEGA_POLL_MAX_CATCHUP", "50");
        std::env::set_var("OMEGA_LOGS_CONFIRMATIONS", "0");
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Mock chain state
// ─────────────────────────────────────────────────────────────────────────────

fn block_hash(n: u64, tag: u8) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&n.to_be_bytes());
    h[8] = tag;
    h[31] = 0xAB;
    h
}

fn hex32(b: &[u8; 32]) -> String {
    let body: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("0x{body}")
}

fn zero32() -> String {
    format!("0x{}", "00".repeat(32))
}

fn block_json(n: u64, tag: u8, parent_tag: u8) -> Value {
    json!({
        "hash": hex32(&block_hash(n, tag)),
        "parentHash": hex32(&block_hash(n.saturating_sub(1), parent_tag)),
        "sha3Uncles": zero32(),
        "miner": format!("0x{}", "00".repeat(20)),
        "stateRoot": zero32(),
        "transactionsRoot": zero32(),
        "receiptsRoot": zero32(),
        "logsBloom": format!("0x{}", "00".repeat(256)),
        "difficulty": "0x0",
        "number": format!("0x{n:x}"),
        "gasLimit": "0x1c9c380",
        "gasUsed": "0x0",
        "timestamp": format!("0x{:x}", 1_700_000_000u64 + n),
        "extraData": "0x",
        "mixHash": zero32(),
        "nonce": "0x0000000000000000",
        "baseFeePerGas": "0x3b9aca00", // 1 gwei
        "totalDifficulty": "0x0",
        "size": "0x1",
        "uncles": [],
        "transactions": []
    })
}

struct MockLog {
    block: u64,
    address: String,
    removed: bool,
    log_index: u64,
}

fn log_json(l: &MockLog) -> Value {
    json!({
        "address": l.address,
        "topics": [SYNC_TOPIC],
        "data": format!("0x{}", "00".repeat(64)),
        "blockHash": hex32(&block_hash(l.block, 0)),
        "blockNumber": format!("0x{:x}", l.block),
        "transactionHash": hex32(&block_hash(l.block, 7)),
        "transactionIndex": "0x0",
        "logIndex": format!("0x{:x}", l.log_index),
        "removed": l.removed
    })
}

struct State {
    chain_id: u64,
    /// block number -> (own tag, parent tag)
    blocks: Mutex<BTreeMap<u64, (u8, u8)>>,
    head: AtomicU64,
    logs: Mutex<Vec<MockLog>>,
    /// every eth_getLogs (from, to) the server received, including rejected ones
    log_calls: Mutex<Vec<(u64, u64)>>,
    /// number of upcoming eth_getBlockByNumber calls to answer with HTTP 429
    block_429s: AtomicUsize,
    /// number of upcoming eth_getLogs calls to answer with HTTP 429
    log_429s: AtomicUsize,
    /// reject eth_getLogs spans wider than this (0 = unlimited)
    max_log_span: AtomicU64,
}

impl State {
    fn new(chain_id: u64) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            blocks: Mutex::new(BTreeMap::new()),
            head: AtomicU64::new(0),
            logs: Mutex::new(Vec::new()),
            log_calls: Mutex::new(Vec::new()),
            block_429s: AtomicUsize::new(0),
            log_429s: AtomicUsize::new(0),
            max_log_span: AtomicU64::new(0),
        })
    }

    fn set_block(&self, n: u64, tag: u8, parent_tag: u8) {
        self.blocks.lock().unwrap().insert(n, (tag, parent_tag));
    }

    fn set_head(&self, n: u64) {
        self.head.store(n, Ordering::SeqCst);
    }

    fn add_log(&self, block: u64, address: &str, removed: bool) {
        let mut logs = self.logs.lock().unwrap();
        let log_index = logs.len() as u64;
        logs.push(MockLog {
            block,
            address: address.to_string(),
            removed,
            log_index,
        });
    }

    fn log_calls(&self) -> Vec<(u64, u64)> {
        self.log_calls.lock().unwrap().clone()
    }
}

fn take_one(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
            if v > 0 {
                Some(v - 1)
            } else {
                None
            }
        })
        .is_ok()
}

fn hex_u64(v: &Value) -> u64 {
    u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16).unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Mock JSON-RPC server
// ─────────────────────────────────────────────────────────────────────────────

enum Reply {
    Result(Value),
    RpcError(i64, String),
    Http(u16, String),
}

fn handle(st: &State, method: &str, params: &Value) -> Reply {
    match method {
        "eth_chainId" => Reply::Result(json!(format!("0x{:x}", st.chain_id))),
        "eth_blockNumber" => Reply::Result(json!(format!("0x{:x}", st.head.load(Ordering::SeqCst)))),
        "eth_getBlockByNumber" => {
            if take_one(&st.block_429s) {
                return Reply::Http(429, "rate limited".to_string());
            }
            let tag = params[0].as_str().unwrap_or("latest");
            let n = if tag == "latest" {
                st.head.load(Ordering::SeqCst)
            } else {
                u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap_or(0)
            };
            match st.blocks.lock().unwrap().get(&n) {
                Some((t, p)) => Reply::Result(block_json(n, *t, *p)),
                None => Reply::Result(Value::Null),
            }
        }
        "eth_getLogs" => {
            let from = hex_u64(&params[0]["fromBlock"]);
            let to = hex_u64(&params[0]["toBlock"]);
            st.log_calls.lock().unwrap().push((from, to));
            if take_one(&st.log_429s) {
                return Reply::Http(429, "rate limited".to_string());
            }
            let max_span = st.max_log_span.load(Ordering::SeqCst);
            if max_span > 0 && to.saturating_sub(from) + 1 > max_span {
                return Reply::RpcError(-32005, "query returned more than 10000 results".to_string());
            }
            let logs = st.logs.lock().unwrap();
            let matching: Vec<Value> = logs
                .iter()
                .filter(|l| l.block >= from && l.block <= to)
                .map(log_json)
                .collect();
            Reply::Result(Value::Array(matching))
        }
        _ => Reply::RpcError(-32601, format!("method not found: {method}")),
    }
}

fn answer_one(st: &State, req: &Value) -> Result<Value, (u16, String)> {
    let id = req["id"].clone();
    let method = req["method"].as_str().unwrap_or("");
    match handle(st, method, &req["params"]) {
        Reply::Result(v) => Ok(json!({"jsonrpc": "2.0", "id": id, "result": v})),
        Reply::RpcError(code, message) => {
            Ok(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
        }
        Reply::Http(status, body) => Err((status, body)),
    }
}

async fn serve_conn(mut sock: TcpStream, st: Arc<State>) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];

    let header_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let content_len = headers
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);

    while buf.len() < header_end + content_len {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }

    let end = (header_end + content_len).min(buf.len());
    let body: Value = serde_json::from_slice(&buf[header_end..end]).unwrap_or(Value::Null);

    let (status, payload) = match &body {
        Value::Array(items) => {
            let mut out = Vec::new();
            let mut http_err: Option<(u16, String)> = None;
            for item in items {
                match answer_one(&st, item) {
                    Ok(v) => out.push(v),
                    Err(e) => {
                        http_err = Some(e);
                        break;
                    }
                }
            }
            match http_err {
                Some(e) => e,
                None => (200, Value::Array(out).to_string()),
            }
        }
        other => match answer_one(&st, other) {
            Ok(v) => (200, v.to_string()),
            Err(e) => e,
        },
    };

    let status_text = match status {
        200 => "OK",
        429 => "Too Many Requests",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.shutdown().await?;
    Ok(())
}

struct MockServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    async fn start(state: Arc<State>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock rpc listener");
        let addr = listener.local_addr().expect("listener addr");
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let st = state.clone();
                tokio::spawn(async move {
                    let _ = serve_conn(sock, st).await;
                });
            }
        });
        Self {
            url: format!("http://{addr}"),
            task,
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────────

async fn recv<T: Clone>(rx: &mut broadcast::Receiver<T>, secs: u64) -> T {
    tokio::time::timeout(Duration::from_secs(secs), rx.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("broadcast channel closed")
}

async fn assert_no_event<T: Clone>(rx: &mut broadcast::Receiver<T>, millis: u64) {
    let r = tokio::time::timeout(Duration::from_millis(millis), rx.recv()).await;
    assert!(r.is_err(), "expected no event, but one arrived");
}

fn limiter() -> Arc<RpcRateLimiter> {
    Arc::new(RpcRateLimiter::new())
}

async fn connect_client(url: &str) -> OmegaRpcClient {
    OmegaRpcClient::connect(RpcClientConfig::new(url, 100, CHAIN_ID))
        .await
        .unwrap_or_else(|e| panic!("connect over http failed: {e}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Block polling
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn block_poll_emits_blocks_and_flags_reorgs() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_block(100, 0, 0);
    st.set_head(100);
    let server = MockServer::start(st.clone()).await;

    let client = connect_client(&server.url).await;
    assert_eq!(client.transport(), RpcTransport::Http);

    let mut rx = client.subscribe_blocks();
    let c = client.clone();
    let task = tokio::spawn(async move { c.run_block_subscription().await });

    let e = recv(&mut rx, 5).await;
    assert_eq!(e.number, 100);
    assert!(!e.is_reorg_or_stale, "first block must not be flagged");
    assert_eq!(e.base_fee_gwei, Some(1));

    // A clean successor: parent hash matches the previous block.
    st.set_block(101, 0, 0);
    st.set_head(101);
    let e = recv(&mut rx, 5).await;
    assert_eq!(e.number, 101);
    assert!(!e.is_reorg_or_stale, "clean successor must not be flagged");

    // A successor whose parent hash does NOT match block 101: a reorg.
    st.set_block(102, 0, 9);
    st.set_head(102);
    let e = recv(&mut rx, 5).await;
    assert_eq!(e.number, 102);
    assert!(e.is_reorg_or_stale, "parent-hash mismatch must be flagged");

    // The tip block at the same height is replaced by a different block.
    st.set_block(102, 1, 0);
    let e = recv(&mut rx, 5).await;
    assert_eq!(e.number, 102);
    assert_eq!(e.hash.0, block_hash(102, 1));
    assert!(e.is_reorg_or_stale, "same-height replacement must be flagged");

    task.abort();
}

#[tokio::test]
async fn block_poll_backfills_missed_blocks_in_order() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_block(110, 0, 0);
    st.set_head(110);
    let server = MockServer::start(st.clone()).await;

    let client = connect_client(&server.url).await;
    let mut rx = client.subscribe_blocks();
    let c = client.clone();
    let task = tokio::spawn(async move { c.run_block_subscription().await });

    assert_eq!(recv(&mut rx, 5).await.number, 110);

    // The chain jumps four blocks between two polls. Blocks are published before the
    // head moves, so the poller always sees a consistent view.
    for n in 111..=114 {
        st.set_block(n, 0, 0);
    }
    st.set_head(114);

    for expected in 111..=114 {
        let e = recv(&mut rx, 5).await;
        assert_eq!(e.number, expected, "blocks must arrive in ascending order");
        assert!(!e.is_reorg_or_stale, "contiguous back-fill must not be flagged");
    }

    task.abort();
}

#[tokio::test]
async fn block_poll_recovers_from_http_429() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_block(200, 0, 0);
    st.set_head(200);
    st.block_429s.store(2, Ordering::SeqCst);
    let server = MockServer::start(st.clone()).await;

    let client = connect_client(&server.url).await;
    let mut rx = client.subscribe_blocks();
    let c = client.clone();
    let task = tokio::spawn(async move { c.run_block_subscription().await });

    // Two throttled responses → roughly 1s + 2s of backoff before the block arrives.
    let e = recv(&mut rx, 20).await;
    assert_eq!(e.number, 200);
    assert_eq!(
        st.block_429s.load(Ordering::SeqCst),
        0,
        "both throttled responses must have been consumed"
    );

    task.abort();
}

#[tokio::test]
async fn wrong_chain_id_is_a_fatal_connect_error() {
    init_env();
    let st = State::new(1); // endpoint claims to be Ethereum mainnet
    let server = MockServer::start(st).await;

    let res = OmegaRpcClient::connect(RpcClientConfig::new(&server.url, 100, CHAIN_ID)).await;
    let err = match res {
        Ok(_) => panic!("connecting to the wrong chain must fail"),
        Err(e) => e,
    };
    assert!(err.is_fatal(), "a chain-ID mismatch must be fatal");
    assert!(matches!(
        err,
        RpcClientError::ChainIdMismatch {
            expected: 42_161,
            actual: 1
        }
    ));
}

// ─────────────────────────────────────────────────────────────────────────────
// Fee oracle polling
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn fee_oracle_poll_emits_once_per_new_block() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_block(300, 0, 0);
    st.set_head(300);
    let server = MockServer::start(st.clone()).await;

    let (tx, mut rx) = broadcast::channel::<FeeOracleEvent>(64);
    let task = tokio::spawn(run_fee_oracle_stream(
        server.url.clone(),
        CHAIN_ID,
        limiter(),
        tx,
    ));

    let e: FeeOracleEvent = recv(&mut rx, 5).await;
    assert_eq!(e.block_number, 300);
    assert_eq!(e.base_fee_gwei, 1);
    assert_eq!(e.chain_id, CHAIN_ID);

    // Same tip on following polls: no duplicate events.
    assert_no_event(&mut rx, 400).await;

    st.set_block(301, 0, 0);
    st.set_head(301);
    let e: FeeOracleEvent = recv(&mut rx, 5).await;
    assert_eq!(e.block_number, 301);

    task.abort();
}

// ─────────────────────────────────────────────────────────────────────────────
// Log polling
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn dex_poll_uses_contiguous_ranges_without_gaps_or_overlap() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_head(200);
    st.add_log(200, POOL_A, false);
    st.add_log(201, POOL_A, false);
    st.add_log(203, POOL_A, false);
    let server = MockServer::start(st.clone()).await;

    let (tx, mut rx) = broadcast::channel::<DexSyncEvent>(64);
    let task = tokio::spawn(run_dex_sync_stream(
        server.url.clone(),
        CHAIN_ID,
        limiter(),
        tx,
    ));

    let pool_a: Address = POOL_A.parse().expect("pool address");

    // The poller starts at the tip: no historical backfill.
    let e = recv(&mut rx, 5).await;
    assert_eq!(e.log.block_number, Some(200));
    assert_eq!(e.pool, pool_a);

    st.set_head(203);
    assert_eq!(recv(&mut rx, 5).await.log.block_number, Some(201));
    assert_eq!(recv(&mut rx, 5).await.log.block_number, Some(203));

    assert_eq!(
        st.log_calls(),
        vec![(200, 200), (201, 203)],
        "each block must be queried exactly once"
    );

    task.abort();
}

#[tokio::test]
async fn dex_poll_splits_wide_ranges_and_delivers_in_ascending_order() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_head(300);
    st.max_log_span.store(2, Ordering::SeqCst);
    for block in 301..=307 {
        st.add_log(block, POOL_A, false);
    }
    let server = MockServer::start(st.clone()).await;

    let (tx, mut rx) = broadcast::channel::<DexSyncEvent>(64);
    let task = tokio::spawn(run_dex_sync_stream(
        server.url.clone(),
        CHAIN_ID,
        limiter(),
        tx,
    ));

    // Let the first poll (range 300..300, no logs) happen, then jump the head.
    tokio::time::sleep(Duration::from_millis(500)).await;
    st.set_head(307);

    for expected in 301..=307u64 {
        let e = recv(&mut rx, 5).await;
        assert_eq!(
            e.log.block_number,
            Some(expected),
            "logs must arrive in ascending block order after splitting"
        );
    }

    let calls = st.log_calls();
    assert!(
        calls.contains(&(301, 307)),
        "the wide range must be tried first: {calls:?}"
    );
    assert!(
        calls.contains(&(307, 307)),
        "the range must be split down until accepted: {calls:?}"
    );

    task.abort();
}

#[tokio::test]
async fn dex_poll_retries_the_same_range_after_429() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_head(400);
    st.add_log(400, POOL_A, false);
    st.log_429s.store(1, Ordering::SeqCst);
    let server = MockServer::start(st.clone()).await;

    let (tx, mut rx) = broadcast::channel::<DexSyncEvent>(64);
    let task = tokio::spawn(run_dex_sync_stream(
        server.url.clone(),
        CHAIN_ID,
        limiter(),
        tx,
    ));

    let e = recv(&mut rx, 20).await;
    assert_eq!(e.log.block_number, Some(400));
    assert_eq!(
        st.log_calls(),
        vec![(400, 400), (400, 400)],
        "after a 429 the SAME range must be retried (no block may be skipped)"
    );

    task.abort();
}

#[tokio::test]
async fn dex_poll_drops_removed_logs() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_head(500);
    st.add_log(500, POOL_A, true); // the node says this one was orphaned
    st.add_log(501, POOL_A, false);
    let server = MockServer::start(st.clone()).await;

    let (tx, mut rx) = broadcast::channel::<DexSyncEvent>(64);
    let task = tokio::spawn(run_dex_sync_stream(
        server.url.clone(),
        CHAIN_ID,
        limiter(),
        tx,
    ));

    tokio::time::sleep(Duration::from_millis(500)).await;
    st.set_head(501);

    let e = recv(&mut rx, 5).await;
    assert_eq!(
        e.log.block_number,
        Some(501),
        "the removed log at block 500 must not be forwarded"
    );

    task.abort();
}

#[tokio::test]
async fn lending_poll_classifies_aave_and_drops_unknown_contracts() {
    init_env();
    let st = State::new(CHAIN_ID);
    st.set_head(600);
    st.add_log(600, AAVE_POOL, false);
    st.add_log(601, UNKNOWN_CONTRACT, false);
    let server = MockServer::start(st.clone()).await;

    let (tx, mut rx) = broadcast::channel::<LendingProtocolEvent>(64);
    let task = tokio::spawn(run_lending_protocol_stream(
        server.url.clone(),
        CHAIN_ID,
        limiter(),
        tx,
    ));

    let e = recv(&mut rx, 5).await;
    assert_eq!(e.protocol, LendingProtocol::AaveV3);
    assert_eq!(e.log.block_number, Some(600));

    st.set_head(601);
    assert_no_event(&mut rx, 800).await;

    task.abort();
}