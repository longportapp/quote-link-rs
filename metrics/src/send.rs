use once_cell::sync::Lazy;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;
use std::time::Duration;

use crate::{DownstreamLabel, LATENCY_BUCKET};

pub fn send_count(downstream: &str, sample: &str) {
    SEND_COUNTER
        .get_or_create(&DownstreamLabel {
            downstream: downstream.to_string(),
            sample: sample.to_string(),
        })
        .inc();
}

pub fn send_bytes_count(downstream: &str, sample: &str, bytes_size: u64) {
    SEND_BYTES_COUNTER
        .get_or_create(&DownstreamLabel {
            downstream: downstream.to_string(),
            sample: sample.to_string(),
        })
        .inc_by(bytes_size);
}

pub fn send_status_count(downstream: &str, sample: &str) {
    SEND_STATUS_COUNTER
        .get_or_create(&DownstreamLabel {
            downstream: downstream.to_string(),
            sample: sample.to_string(),
        })
        .inc();
}

pub fn send_at_latency(downstream: &str, sample: &str, cost: Duration) {
    SEND_LATENCY_HISTOGRAM
        .get_or_create(&DownstreamLabel {
            downstream: downstream.to_string(),
            sample: sample.to_string(),
        })
        .observe(cost.as_millis() as f64)
}

pub(crate) fn register(reg: &mut Registry) {
    reg.register(
        "downstream_data_send",
        "How many data send to downstream",
        SEND_COUNTER.clone(),
    );
    reg.register(
        "downstream_data_send_status",
        "Status of downstream send data process",
        SEND_STATUS_COUNTER.clone(),
    );
    reg.register(
        "downstream_data_send_bytes",
        "How many data send to upstream",
        SEND_BYTES_COUNTER.clone(),
    );
    reg.register(
        "downstream_data_send_latency_millisecond",
        "When send quote data to downstream, record time.Now - data.TimeSequence",
        SEND_LATENCY_HISTOGRAM.clone(),
    );
}

#[allow(clippy::type_complexity)]
pub(crate) static SEND_COUNTER: Lazy<Family<DownstreamLabel, Counter, fn() -> Counter>> =
    Lazy::new(Family::<DownstreamLabel, Counter>::default);

#[allow(clippy::type_complexity)]
pub(crate) static SEND_BYTES_COUNTER: Lazy<Family<DownstreamLabel, Counter, fn() -> Counter>> =
    Lazy::new(Family::<DownstreamLabel, Counter>::default);

#[allow(clippy::type_complexity)]
pub(crate) static SEND_STATUS_COUNTER: Lazy<Family<DownstreamLabel, Counter, fn() -> Counter>> =
    Lazy::new(Family::<DownstreamLabel, Counter>::default);

#[allow(clippy::type_complexity)]
pub(crate) static SEND_LATENCY_HISTOGRAM: Lazy<
    Family<DownstreamLabel, Histogram, fn() -> Histogram>,
> = Lazy::new(|| {
    Family::<DownstreamLabel, Histogram>::new_with_constructor(|| {
        Histogram::new(LATENCY_BUCKET.clone().into_iter())
    })
});
