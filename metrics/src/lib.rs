use std::sync::Arc;

use anyhow::Context;
use axum::body::Body;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use once_cell::unsync::Lazy;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::registry::Registry;
use tokio::signal;

pub mod general;
pub mod process;
pub mod receive;
pub mod send;
pub mod state_metric;

const NAMESPACE_QUOTE_MONITOR: &str = "quote_monitor";
const DEFAULT_METRIC_SERVER_AT: &str = "0.0.0.0:9102";
const DEFAULT_METRIC_ROUTE_PATH: &str = "/metrics";

/// Create a metric registry.
///
/// quote-public/metric rs version
pub async fn register_and_server_at(
    mut addr: &str,
    mut path: &str,
    reg_fns: Vec<fn(&mut Registry)>,
) {
    let mut reg = Registry::with_prefix(NAMESPACE_QUOTE_MONITOR);
    general::register(&mut reg);
    receive::register(&mut reg);
    process::register(&mut reg);
    send::register(&mut reg);

    for f in reg_fns {
        f(&mut reg)
    }

    if path.is_empty() {
        tracing::info!("metric server path is empty, fallback to {DEFAULT_METRIC_ROUTE_PATH}");
        path = DEFAULT_METRIC_ROUTE_PATH;
    }
    let router = Router::new().route(path, get(MetricHandler::new(reg)));

    if addr.is_empty() {
        tracing::info!("metric server addr is empty, fallback to {DEFAULT_METRIC_SERVER_AT}");
        addr = DEFAULT_METRIC_SERVER_AT
    }
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("register metric server listen failed")
        .unwrap();

    let _ = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|err| {
            tracing::error!("metric server serve err return: {err:#}");
        });
}

// Ref: https://github.com/tokio-rs/axum/blob/main/examples/graceful-shutdown/src/main.rs
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("metric server get shutdown signal, end its life")
}

#[derive(Clone)]
struct MetricHandler(Arc<Registry>);

impl MetricHandler {
    pub fn new(reg: Registry) -> Self {
        Self(Arc::new(reg))
    }
}

impl<S> axum::handler::Handler<(), S> for MetricHandler {
    type Future = futures::future::Ready<Response>;

    fn call(self, _req: axum::http::Request<Body>, _state: S) -> Self::Future {
        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &self.0).unwrap();
        futures::future::ready(buf.into_response())
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub(crate) struct StreamLabel {
    pub upstream: String,
    pub sample: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub(crate) struct StreamWithMsgLabel {
    pub upstream: String,
    pub sample: String,
    pub msg: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub(crate) struct LogLabel {
    pub msg: String,
}

#[allow(unused_doc_comments)]
pub(crate) const LATENCY_BUCKET: Lazy<Vec<f64>> = Lazy::new(|| {
    vec![
        /// 0.2 delta
        0.2,
        0.4,
        0.6,
        0.8,
        1.0,
        /// 2 delta
        2.0,
        4.0,
        6.0,
        8.0,
        10.0,
        12.0,
        14.0,
        16.0,
        18.0,
        20.0,
        /// 5 delta
        25.0,
        30.0,
        35.0,
        40.0,
        45.0,
        50.0,
        55.0,
        60.0,
        65.0,
        70.0,
        75.0,
        80.0,
        85.0,
        90.0,
        95.0,
        100.0,
        105.0,
        110.0,
        115.0,
        120.0,
        125.0,
        130.0,
        135.0,
        140.0,
        145.0,
        150.0,
        /// 10 delta
        160.0,
        170.0,
        180.0,
        190.0,
        200.0,
        210.0,
        220.0,
        230.0,
        240.0,
        250.0,
        260.0,
        270.0,
        280.0,
        290.0,
        300.0,
        310.0,
        320.0,
        330.0,
        340.0,
        350.0,
        360.0,
        370.0,
        380.0,
        390.0,
        400.0,
        410.0,
        420.0,
        430.0,
        440.0,
        450.0,
        460.0,
        470.0,
        480.0,
        490.0,
        500.0,
        /// 50 delta
        550.0,
        600.0,
        650.0,
        700.0,
        750.0,
        800.0,
        850.0,
        900.0,
        950.0,
        1000.0,
        1050.0,
        1100.0,
        1150.0,
        1200.0,
        1250.0,
        1300.0,
        1350.0,
        1400.0,
        1450.0,
        1500.0,
        1550.0,
        1600.0,
        /// 300 delta
        1900.0,
        2200.0,
        2500.0,
        2800.0,
        3100.0,
        3400.0,
        3700.0,
        4000.0,
        4300.0,
        4600.0,
        4900.0,
        5000.0,
        /// 500 delta
        5500.0,
        6000.0,
        6500.0,
        7000.0,
        7500.0,
        8000.0,
        8500.0,
        9000.0,
        9500.0,
        /// max
        10000.0,
    ]
});
