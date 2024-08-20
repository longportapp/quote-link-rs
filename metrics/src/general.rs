use once_cell::sync::Lazy;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;

use crate::LogLabel;

pub fn info_msg_count(msg: &str) {
    INFO_MSG_COUNTER
        .get_or_create(&LogLabel {
            msg: msg.to_string(),
        })
        .inc();
}

pub fn warning_msg_count(msg: &str) {
    WARNING_MSG_COUNTER
        .get_or_create(&LogLabel {
            msg: msg.to_string(),
        })
        .inc();
}

pub fn fatal_msg_count(msg: &str) {
    FATAL_MSG_COUNTER
        .get_or_create(&LogLabel {
            msg: msg.to_string(),
        })
        .inc();
}

pub fn msg_gauge(msg: &str, set_to: i64) {
    MSG_GAUGE
        .get_or_create(&LogLabel {
            msg: msg.to_string(),
        })
        .set(set_to);
}

pub(crate) fn register(reg: &mut Registry) {
    reg.register(
        "info_msg_count",
        "How many info msg",
        INFO_MSG_COUNTER.clone(),
    );
    reg.register(
        "warning_msg_count",
        "How many warning msg",
        WARNING_MSG_COUNTER.clone(),
    );
    reg.register(
        "fatal_msg_count",
        "How many fatal msg",
        FATAL_MSG_COUNTER.clone(),
    );
    reg.register("gauge_msg_count", "How many gauge msg", MSG_GAUGE.clone());
}

pub(crate) static INFO_MSG_COUNTER: Lazy<Family<LogLabel, Counter>> = Lazy::new(Family::default);

pub(crate) static WARNING_MSG_COUNTER: Lazy<Family<LogLabel, Counter>> = Lazy::new(Family::default);

pub(crate) static FATAL_MSG_COUNTER: Lazy<Family<LogLabel, Counter>> = Lazy::new(Family::default);

pub(crate) static MSG_GAUGE: Lazy<Family<LogLabel, Gauge>> = Lazy::new(Family::default);
