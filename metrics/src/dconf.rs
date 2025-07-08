use once_cell::sync::Lazy;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::registry::Registry;

pub fn dconf_client_config_count(
    app_name: &str,
    namespace: &str,
    env: &str,
    version: &str,
    id: &str,
) {
    DCONF_CLIENT_CONFIG_COUNTER
        .get_or_create(&DconfLabel {
            app_name: app_name.to_string(),
            namespace: namespace.to_string(),
            env: env.to_string(),
            version: version.to_string(),
            id: id.to_string(),
            msg: "".to_string(),
        })
        .inc();
}

pub fn dconf_client_reconnect_conut(
    app_name: &str,
    namespace: &str,
    env: &str,
    version: &str,
    id: &str,
    msg: &str,
) {
    DCONF_CLIENT_RECONNECT_COUNTER
        .get_or_create(&DconfLabel {
            app_name: app_name.to_string(),
            namespace: namespace.to_string(),
            env: env.to_string(),
            version: version.to_string(),
            id: id.to_string(),
            msg: msg.to_string(),
        })
        .inc();
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub(crate) struct DconfLabel {
    pub app_name: String,
    pub namespace: String,
    pub env: String,
    pub version: String,
    pub id: String,
    pub msg: String,
}

pub(crate) fn register(reg: &mut Registry) {
    reg.register(
        "dconf_client_rs_config_count",
        "information about client current configuration",
        DCONF_CLIENT_CONFIG_COUNTER.clone(),
    );
    reg.register(
        "dconf_client_rs_reconnect",
        "how many times dconf client reconnect occur",
        DCONF_CLIENT_RECONNECT_COUNTER.clone(),
    );
}

pub(crate) static DCONF_CLIENT_CONFIG_COUNTER: Lazy<Family<DconfLabel, Counter>> =
    Lazy::new(Family::default);

pub(crate) static DCONF_CLIENT_RECONNECT_COUNTER: Lazy<Family<DconfLabel, Counter>> =
    Lazy::new(Family::default);
