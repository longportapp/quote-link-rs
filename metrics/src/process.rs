use std::time::Duration;

use once_cell::sync::Lazy;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;

use crate::{LATENCY_BUCKET, StreamLabel, StreamWithMsgLabel};

pub fn operation_count(upstream: &str, sample: &str) {
    OPERATION_COUNTER
        .get_or_create(&StreamLabel {
            upstream: upstream.to_string(),
            sample: sample.to_string(),
        })
        .inc();
}

pub fn exception_count(upstream: &str, sample: &str, msg: &str) {
    EXCEPTION_COUNTER
        .get_or_create(&StreamWithMsgLabel {
            upstream: upstream.to_string(),
            sample: sample.to_string(),
            msg: msg.to_string(),
        })
        .inc();
}

pub fn cost_micro_sec(upstream: &str, sample: &str, cost: Duration) {
    COST_MICRO_SEC_HISTOGRAM
        .get_or_create(&StreamLabel {
            upstream: upstream.to_string(),
            sample: sample.to_string(),
        })
        .observe(cost.as_micros() as f64)
}

pub fn cost_milli_sec(upstream: &str, sample: &str, cost: Duration) {
    COST_MILLI_SEC_HISTOGRAM
        .get_or_create(&StreamLabel {
            upstream: upstream.to_string(),
            sample: sample.to_string(),
        })
        .observe(cost.as_millis() as f64)
}

pub(crate) fn register(reg: &mut Registry) {
    reg.register(
        "data_handle_operation",
        "Operation in data handle process",
        OPERATION_COUNTER.clone(),
    );
    reg.register(
        "data_handle_exception",
        "How many exception case during handle data",
        EXCEPTION_COUNTER.clone(),
    );
    reg.register(
        "data_handle_cost_microsecond",
        "When handle data finish, record time.Now - time.Begin",
        COST_MICRO_SEC_HISTOGRAM.clone(),
    );
    reg.register(
        "data_handle_cost_millisecond",
        "When handle data finish, record time.Now - time.Begin",
        COST_MILLI_SEC_HISTOGRAM.clone(),
    );
}

static OPERATION_COUNTER: Lazy<Family<StreamLabel, Counter>> = Lazy::new(Family::default);

static EXCEPTION_COUNTER: Lazy<Family<StreamWithMsgLabel, Counter>> = Lazy::new(Family::default);

static COST_MICRO_SEC_HISTOGRAM: Lazy<Family<StreamLabel, Histogram, fn() -> Histogram>> =
    Lazy::new(|| {
        Family::new_with_constructor(|| Histogram::new(LATENCY_BUCKET.clone().into_iter()))
    });

static COST_MILLI_SEC_HISTOGRAM: Lazy<Family<StreamLabel, Histogram, fn() -> Histogram>> =
    Lazy::new(|| {
        Family::new_with_constructor(|| Histogram::new(LATENCY_BUCKET.clone().into_iter()))
    });
