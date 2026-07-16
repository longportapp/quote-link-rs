use crate::{packets::Command, tls_utils::new_tls_context_builder};
use anyhow::Context;
use duration_str::deserialize_duration;
use paste::paste;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// 需要至少: 1 个 packet + MAX_UDP_PACKET_SIZE (udf buf size) 才能满足, + 1024
// ≈16Mi
// Changes:
// 虽然根据协议, 这是理论最大值, 但 overhead 过大, 减半为 8Mi + 8 个Bytes
//  * 头部字段 (bits): typ(2)+reserved(2)+command(4)+id(24)+status(8)+body_len(24)
//  * Header 大小: 8 bytes
//  * 最大 body: 2²⁴-1 = 16,777,215 B
const DEFAULT_MAX_PACKET_SIZE: usize = (1 << 23) + 8;
// 流窗口大小 64Mb
const DEFAULT_MAX_STREAM_WINDOW: u64 = 64 * (1 << 20);
// 连接窗口大小 64Mb
const DEFAULT_MAX_CONNECTION_WINDOW: u64 = 64 * (1 << 20);
// 连接空闲超时时间, in milliseconds
const DEFAULT_MAX_IDLE_TIMEOUT: u64 = 10000;
// 数据缓冲区大小 64Mi
const DEFAULT_MAX_DATA_BUF_SIZE: u64 = 64 * (1 << 20);
// 最大流连接数
pub(crate) const DEFAULT_MAX_STREAM_CONN: u64 = 100;
// 最大 UDP 数据包大小 ≈64Kb
pub(crate) const DEFAULT_MAX_UDP_PACKET_SIZE: usize = (1 << 16) - 1;
// 链接最大激活 Connection ID 数, quiche 默认 2
pub(crate) const DEFAULT_ACTIVE_CONNECTION_ID_LIMIT: u64 = 8;
// 最大 ACK 延迟时间
const DEFAULT_MAX_ACK_DELAY: u64 = 25;
// 发送 UDP payload 大小上限, 以太网 MTU(1500) - IP头(20) - UDP头(8) - QUIC头(~122) ≈ 1350 (quiche 默认即 1200)
// 避免 IP 分片: 超过 MTU 的 UDP 包会被 IP 层切片, 任一片丢失则整包重传, 且部分网络设备会直接丢弃分片 UDP
const DEFAULT_MAX_SEND_UDP_PAYLOAD_SIZE: usize = 1200;

pub fn read_from_filepath<T: for<'de> Deserialize<'de>>(path: &str) -> anyhow::Result<T> {
    let content = std::fs::read_to_string(path)?;
    serde_yaml::from_str(&content).context(format!("serde yaml parse err, raw: {content}"))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkClientConfig {
    pub addr: String,
    pub username: String,
    pub password: String,
    #[serde(default = "pod_name")]
    pub client_name: String,
    #[serde(default)]
    pub subscribes: Vec<Subscribe>,
    pub backup_config: Option<BackupConfig>,
    #[serde(default)]
    pub healthy_config: HealthyConfig,
    #[serde(default)]
    pub channel_config: ChannelConfig,
    #[serde(default)]
    pub buffer_config: BufferConfig,
    #[serde(default)]
    pub quiche_config: QuicheConfig,
    #[serde(default)]
    pub timeout_config: TimeoutConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkServerConfig {
    pub listen_addr: String,
    #[serde(default)]
    pub healthy_config: HealthyConfig,
    #[serde(default)]
    pub channel_config: ChannelConfig,
    #[serde(default)]
    pub buffer_config: BufferConfig,
    #[serde(default)]
    pub quiche_config: QuicheConfig,
    #[serde(default)]
    pub timeout_config: TimeoutConfig,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthyConfig {
    #[serde(
        default = "default_heartbeat_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub heartbeat_interval: Duration,
    #[serde(
        default = "default_heartbeat_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub heartbeat_timeout: Duration,
    #[serde(
        default = "default_auth_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub auth_timeout: Duration,
    // retry times 为 0 时, 表示无重试次数限制
    #[serde(default)]
    pub retry_times: u32,
    #[serde(
        default = "default_retry_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub retry_interval: Duration,
}

impl Default for HealthyConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: default_heartbeat_interval(),
            heartbeat_timeout: default_heartbeat_timeout(),
            auth_timeout: default_auth_timeout(),
            retry_times: 0,
            retry_interval: default_retry_interval(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupConfig {
    pub addr: String,
    #[serde(default = "default_failover_threshold")]
    pub failover_threshold: u32,
}

pub fn default_failover_threshold() -> u32 {
    3
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscribe {
    pub cmd: Command,
    #[serde(default)]
    pub sub_all: bool,
    #[serde(default)]
    pub span_index: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MultiAddrMode {
    // 如果 DNS 解析有多个地址, 只使用一个 (arbitrary not random) 连接
    #[default]
    OnlyOne,
    ColdBackup,
    HotBackup,
    // Wait to do: "多路合并" 暂不支持
    // Merge,
}

fn pod_name() -> String {
    std::env::var("POD_NAME").unwrap_or_default()
}

fn default_heartbeat_interval() -> Duration {
    Duration::from_millis(500)
}

fn default_heartbeat_timeout() -> Duration {
    Duration::from_secs(3)
}

fn default_auth_timeout() -> Duration {
    Duration::from_secs(10)
}

fn default_retry_interval() -> Duration {
    Duration::from_secs(3)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuicheConfig {
    pub application_protos: Option<Vec<String>>,
    pub enable_pacing: Option<bool>,
    pub discover_pmtu: Option<bool>,
    pub enable_early_data: Option<bool>,
    pub verify_peer: Option<bool>,
    pub disable_active_migration: Option<bool>,
    pub max_stream_window: Option<u64>,
    pub max_connection_window: Option<u64>,
    pub max_idle_timeout: Option<u64>,
    pub max_recv_udp_payload_size: Option<usize>,
    pub max_send_udp_payload_size: Option<usize>,
    pub initial_max_data: Option<u64>,
    pub initial_max_stream_data_uni: Option<u64>,
    pub initial_max_stream_data_bidi_local: Option<u64>,
    pub initial_max_stream_data_bidi_remote: Option<u64>,
    pub initial_max_streams_bidi: Option<u64>,
    pub initial_max_streams_uni: Option<u64>,
    pub active_connection_id_limit: Option<u64>,
    pub max_ack_delay: Option<u64>,
}

impl Default for QuicheConfig {
    fn default() -> Self {
        Self {
            // 协议协商, 取决于 server 端支持的协议
            application_protos: Some(vec!["quic-echo-example".to_string()]),
            enable_pacing: Some(true),
            // 是否开启 path MTU discovery, default is false
            discover_pmtu: Some(false),
            enable_early_data: Some(false),
            verify_peer: Some(false),
            disable_active_migration: Some(true),
            max_stream_window: Some(DEFAULT_MAX_STREAM_WINDOW),
            max_connection_window: Some(DEFAULT_MAX_CONNECTION_WINDOW),
            max_idle_timeout: Some(DEFAULT_MAX_IDLE_TIMEOUT),
            max_recv_udp_payload_size: Some(DEFAULT_MAX_UDP_PACKET_SIZE),
            max_send_udp_payload_size: Some(DEFAULT_MAX_SEND_UDP_PAYLOAD_SIZE),
            initial_max_data: Some(DEFAULT_MAX_DATA_BUF_SIZE),
            initial_max_stream_data_uni: Some(DEFAULT_MAX_DATA_BUF_SIZE),
            initial_max_stream_data_bidi_local: Some(DEFAULT_MAX_DATA_BUF_SIZE),
            initial_max_stream_data_bidi_remote: Some(DEFAULT_MAX_DATA_BUF_SIZE),
            initial_max_streams_bidi: Some(DEFAULT_MAX_STREAM_CONN),
            initial_max_streams_uni: Some(DEFAULT_MAX_STREAM_CONN),
            active_connection_id_limit: Some(DEFAULT_ACTIVE_CONNECTION_ID_LIMIT),
            max_ack_delay: Some(DEFAULT_MAX_ACK_DELAY),
        }
    }
}

pub fn link_recommend_quiche_config() -> quiche::Config {
    recommend_quiche_config(&[b"quic-echo-example"])
}

pub fn recommend_quiche_config(protos: &[&[u8]]) -> quiche::Config {
    let mut cfg: quiche::Config = QuicheConfig::default().into();
    // 协议协商, 取决于 server 端支持的协议
    cfg.set_application_protos(protos).unwrap();

    cfg
}

macro_rules! set_if_some {
    ($dest_cfg:expr, $source_cfg:expr, $field:ident, direct) => {
        if let Some(value) = $source_cfg.$field {
            $dest_cfg.$field(value);
        }
    };

    ($dest_cfg:expr, $source_cfg:expr, $field:ident, with_set) => {
        paste! {
            if let Some(value) = $source_cfg.$field {
                $dest_cfg.[<set_ $field>](value);
            }
        }
    };
}

impl From<&QuicheConfig> for quiche::Config {
    fn from(config: &QuicheConfig) -> quiche::Config {
        let mut cfg = quiche::Config::with_boring_ssl_ctx_builder(
            quiche::PROTOCOL_VERSION,
            new_tls_context_builder().unwrap(),
        )
        .unwrap();

        if let Some(application_protos) = &config.application_protos {
            let application_protos_bytes: Vec<&[u8]> =
                application_protos.iter().map(|s| s.as_bytes()).collect();
            cfg.set_application_protos(&application_protos_bytes)
                .unwrap();
        };

        set_if_some!(cfg, config, enable_pacing, direct);
        set_if_some!(cfg, config, verify_peer, direct);
        set_if_some!(cfg, config, disable_active_migration, with_set);
        set_if_some!(cfg, config, max_stream_window, with_set);
        set_if_some!(cfg, config, max_connection_window, with_set);
        set_if_some!(cfg, config, max_idle_timeout, with_set);
        set_if_some!(cfg, config, max_recv_udp_payload_size, with_set);
        set_if_some!(cfg, config, max_send_udp_payload_size, with_set);
        set_if_some!(cfg, config, initial_max_data, with_set);
        set_if_some!(cfg, config, initial_max_stream_data_uni, with_set);
        set_if_some!(cfg, config, initial_max_stream_data_bidi_local, with_set);
        set_if_some!(cfg, config, initial_max_stream_data_bidi_remote, with_set);
        set_if_some!(cfg, config, initial_max_streams_bidi, with_set);
        set_if_some!(cfg, config, initial_max_streams_uni, with_set);
        set_if_some!(cfg, config, active_connection_id_limit, with_set);
        set_if_some!(cfg, config, max_ack_delay, with_set);

        cfg
    }
}

impl From<QuicheConfig> for quiche::Config {
    fn from(config: QuicheConfig) -> quiche::Config {
        (&config).into()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferConfig {
    // 读取 UDP 数据包的缓冲区大小
    #[serde(default = "default_max_udp_read_size")]
    pub max_udp_read_size: usize,
    // 写入 UDP 数据包的缓冲区大小
    #[serde(default = "default_max_udp_write_size")]
    pub max_udp_write_size: usize,
    // 读取对端 stream 数据包的缓冲区大小
    #[serde(default = "default_max_stream_read_size")]
    pub max_stream_read_size: usize,
    // 写入对端 stream 数据包的缓冲区大小
    #[serde(default = "default_max_stream_send_size")]
    pub max_stream_send_size: usize,
    // 最大连接数
    #[serde(default = "default_peer_conn_init_size")]
    pub peer_conn_init_size: usize,
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            max_udp_read_size: default_max_udp_read_size(),
            max_udp_write_size: default_max_udp_write_size(),
            max_stream_read_size: default_max_stream_read_size(),
            max_stream_send_size: default_max_stream_send_size(),
            peer_conn_init_size: default_peer_conn_init_size(),
        }
    }
}

fn default_max_udp_read_size() -> usize {
    DEFAULT_MAX_UDP_PACKET_SIZE
}

fn default_max_udp_write_size() -> usize {
    DEFAULT_MAX_UDP_PACKET_SIZE
}

fn default_max_stream_read_size() -> usize {
    DEFAULT_MAX_PACKET_SIZE
}

fn default_max_stream_send_size() -> usize {
    DEFAULT_MAX_PACKET_SIZE
}

fn default_peer_conn_init_size() -> usize {
    64
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConfig {
    // 接收通道, 用于接收数据
    #[serde(default = "default_recv_chan_size")]
    pub recv_chan_size: usize,
    // 发送通道, 用于传输发送数据
    #[serde(default = "default_send_chan_size")]
    pub send_chan_size: usize,
    // 状态通道, 用于传输连接状态
    #[serde(default = "default_signal_chan_size")]
    pub signal_chan_size: usize,
    // 处理通道, 用于处理数据
    #[serde(default = "default_process_chan_size")]
    pub process_chan_size: usize,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            recv_chan_size: default_recv_chan_size(),
            send_chan_size: default_send_chan_size(),
            signal_chan_size: default_signal_chan_size(),
            process_chan_size: default_process_chan_size(),
        }
    }
}

fn default_recv_chan_size() -> usize {
    8192
}

fn default_send_chan_size() -> usize {
    8192
}

fn default_signal_chan_size() -> usize {
    1024
}

fn default_process_chan_size() -> usize {
    8192
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeoutConfig {
    #[serde(
        default = "default_request_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub request: Duration,
    #[serde(
        default = "default_response_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub response: Duration,
    #[serde(
        default = "default_idle_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub idle_interval: Duration,
    #[serde(
        default = "default_handshake_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub handshake_timeout: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            request: default_request_timeout(),
            response: default_response_timeout(),
            idle_interval: default_idle_interval(),
            handshake_timeout: default_handshake_timeout(),
        }
    }
}

fn default_request_timeout() -> Duration {
    Duration::from_secs(3)
}

fn default_response_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_idle_interval() -> Duration {
    Duration::from_millis(DEFAULT_MAX_IDLE_TIMEOUT)
}

fn default_handshake_timeout() -> Duration {
    Duration::from_secs(3)
}
