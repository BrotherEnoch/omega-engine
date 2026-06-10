// omega-frontend-arch/crates/omega-ui/src/sync_adapter.rs
#![allow(dead_code)]
use std::pin::Pin;
use std::future::Future;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

use omega_control_contracts::error::FrontendError;
use omega_frontend::sync::SyncConfig;

type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

pub struct FetchClient {
    pub base_url:     String,
    pub bearer_token: Option<String>,
}

impl FetchClient {
    pub fn new(config: &SyncConfig) -> Self {
        Self { base_url: config.base_url.clone(), bearer_token: config.bearer_token.clone() }
    }

    fn auth_header(&self) -> Option<String> {
        self.bearer_token.as_ref().map(|t| format!("Bearer {t}"))
    }

    pub fn get_local(&self, url: &str, _auth: Option<&str>) -> LocalFuture<Result<String, FrontendError>> {
        let url      = url.to_string();
        let auth_hdr = self.auth_header();
        Box::pin(async move {
            let opts = RequestInit::new();
            opts.set_method("GET");
            opts.set_mode(RequestMode::Cors);
            let request = Request::new_with_str_and_init(&url, &opts)
                .map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            if let Some(hdr) = auth_hdr {
                request.headers().set("Authorization", &hdr)
                    .map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            }
            let window   = web_sys::window().unwrap();
            let resp_val = JsFuture::from(window.fetch_with_request(&request))
                .await.map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            let resp: Response = resp_val.dyn_into()
                .map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            if !resp.ok() {
                return Err(FrontendError::WsConnection(format!("HTTP {}", resp.status())));
            }
            let text = JsFuture::from(
                resp.text().map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?
            ).await.map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            text.as_string().ok_or_else(|| FrontendError::WsConnection("not string".into()))
        })
    }

    pub fn post_local(&self, url: &str, body: Option<&str>, _auth: Option<&str>) -> LocalFuture<Result<String, FrontendError>> {
        let url      = url.to_string();
        let body     = body.map(|s| s.to_string());
        let auth_hdr = self.auth_header();
        Box::pin(async move {
            let opts = RequestInit::new();
            opts.set_method("POST");
            opts.set_mode(RequestMode::Cors);
            if let Some(ref b) = body { opts.set_body(&JsValue::from_str(b)); }
            let request = Request::new_with_str_and_init(&url, &opts)
                .map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            request.headers().set("Content-Type", "application/json")
                .map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            if let Some(hdr) = auth_hdr {
                request.headers().set("Authorization", &hdr)
                    .map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            }
            let window   = web_sys::window().unwrap();
            let resp_val = JsFuture::from(window.fetch_with_request(&request))
                .await.map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            let resp: Response = resp_val.dyn_into()
                .map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            let text = JsFuture::from(
                resp.text().map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?
            ).await.map_err(|e| FrontendError::WsConnection(format!("{e:?}")))?;
            text.as_string().ok_or_else(|| FrontendError::WsConnection("not string".into()))
        })
    }
}

use leptos::spawn_local;

pub fn start_health_poll(
    base_url: String,
    bearer_token: Option<String>,
    on_snapshot: impl Fn(omega_control_contracts::rest::HealthSnapshot) + 'static,
) {
    let client      = Rc::new(FetchClient { base_url, bearer_token });
    let on_snapshot = Rc::new(on_snapshot);   // Rc so each tick gets a cheap clone

    gloo_timers::callback::Interval::new(2_000, move || {
        let client      = Rc::clone(&client);
        let on_snapshot = Rc::clone(&on_snapshot);
        let url = format!("{}/api/v1/health", client.base_url);
        spawn_local(async move {
            if let Ok(body) = client.get_local(&url, None).await {
                if let Ok(snap) = serde_json::from_str(&body) {
                    on_snapshot(snap);
                }
            }
        });
    }).forget();
}

