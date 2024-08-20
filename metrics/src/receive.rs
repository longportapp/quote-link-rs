use std::time::Duration;

use once_cell::sync::Lazy;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;

use crate::{LATENCY_BUCKET, StreamLabel};

pub fn receive_count(upstream: &str, sample: &str) {
    RECEIVE_COUNTER
        .get_or_create(&StreamLabel {
            upstream: upstream.to_string(),
            sample: sample.to_string(),
        })
        .inc();
}

pub fn receive_bytes_count(upstream: &str, sample: &str, bytes_size: u64) {
    RECEIVE_BYTES_COUNTER
        .get_or_create(&StreamLabel {
            upstream: upstream.to_string(),
            sample: sample.to_string(),
        })
        .inc_by(bytes_size);
}

pub fn receive_status_count(upstream: &str, sample: &str) {
    RECEIVE_STATUS_COUNTER
        .get_or_create(&StreamLabel {
            upstream: upstream.to_string(),
            sample: sample.to_string(),
        })
        .inc();
}

pub fn receive_at_latency(upstream: &str, sample: &str, cost: Duration) {
    RECEIVE_LATENCY_HISTOGRAM
        .get_or_create(&StreamLabel {
            upstream: upstream.to_string(),
            sample: sample.to_string(),
        })
        .observe(cost.as_millis() as f64)
}

pub(crate) fn register(reg: &mut Registry) {
    reg.register(
        "upstream_data_receive_total",
        "How many data received from upstream",
        RECEIVE_COUNTER.clone(),
    );
    reg.register(
        "upstream_data_receive_bytes",
        "How many data received from upstream",
        RECEIVE_BYTES_COUNTER.clone(),
    );
    reg.register(
        "upstream_data_receive_status",
        "Status of upstream receive process",
        RECEIVE_STATUS_COUNTER.clone(),
    );
    reg.register(
        "upstream_data_receive_latency_millisecond",
        "When handle data finish, record time.Now - time.Begin",
        RECEIVE_LATENCY_HISTOGRAM.clone(),
    );
}

pub(crate) static RECEIVE_COUNTER: Lazy<Family<StreamLabel, Counter, fn() -> Counter>> =
    Lazy::new(Family::<StreamLabel, Counter>::default);

pub(crate) static RECEIVE_BYTES_COUNTER: Lazy<Family<StreamLabel, Counter, fn() -> Counter>> =
    Lazy::new(Family::<StreamLabel, Counter>::default);

pub(crate) static RECEIVE_STATUS_COUNTER: Lazy<Family<StreamLabel, Counter, fn() -> Counter>> =
    Lazy::new(Family::<StreamLabel, Counter>::default);

pub(crate) static RECEIVE_LATENCY_HISTOGRAM: Lazy<
    Family<StreamLabel, Histogram, fn() -> Histogram>,
> = Lazy::new(|| {
    Family::<StreamLabel, Histogram>::new_with_constructor(|| {
        Histogram::new(LATENCY_BUCKET.clone().into_iter())
    })
});
