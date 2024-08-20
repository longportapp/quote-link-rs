use std::time::Duration;

use anyhow::Context;
use duration_str::deserialize_duration;
use serde::{Deserialize, Serialize};

use crate::packets::Command;

// 需要至少: 1 个 packet + MAX_UDP_PACKET_SIZE (udf buf size) 才能满足, + 1024
pub(crate) const MAX_PACKET_SIZE: usize = ((1 << 24) - 1 + 8 + 8) + ((1 << 16) - 1) + 1024;

#[allow(dead_code)]
pub(crate) const DEFAULT_LINK_FILEPATH: &str = "/longbridge/configmap/link.yml";

pub fn get_from_filepath(path: &str) -> anyhow::Result<ClientConfig> {
    let content = std::fs::read_to_string(path)?;
    serde_yaml::from_str(&content).context(format!("serde yaml parse err, raw: {content}"))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientConfig {
    pub addr: String,
    pub username: String,
    pub password: String,
    #[serde(default = "pod_name")]
    pub client_name: String,
    // retry times 为 0 时, 表示无重试次数限制
    #[serde(default)]
    pub retry_times: i32,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_duration")]
    pub timeout: Duration,
    #[serde(default)]
    pub subscribe_cmds: Vec<Command>,
    #[serde(default)]
    pub buf_mode: BufMode,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum BufMode {
    #[default]
    ParseFirst,
    ReadFirst,
}

fn pod_name() -> String {
    std::env::var("POD_NAME").unwrap_or_default()
}

fn default_timeout() -> Duration {
    Duration::from_secs(5)
}
