use std::sync::mpsc;
use std::sync::mpsc::Receiver;

use prost::Message;

use crate::config;
use crate::config::ClientConfig;
use crate::config::MAX_PACKET_SIZE;
use crate::packets::*;
use crate::protos::{AuthResponse, LinkError};
use crate::transport::Transport;

// in milliseconds
const DEFAULT_MAX_IDLE_TIMEOUT: u64 = 2 * 1000; // link-go default value
const DEFAULT_MAX_STREAM_CONN: u64 = 100;
const DEFAULT_MAX_DATA_BUF_SIZE: u64 = 256 * (1 << 10 << 10); // 256Mi
const DEFAULT_MAX_RECV_SEND_QUEUE_SIZE: usize = 1 << 16; // 65536 最大值
const DEFAULT_MAX_STREAM_WINDOW_SIZE: u64 = 4 * (1 << 30) ; // 4G 与 go client 设置同样的参数

pub struct QuicheConfigBuilder {
    quiche_config: quiche::Config,
}

impl QuicheConfigBuilder {
    pub fn new() -> Self {
        Self {
            quiche_config: quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap(),
        }
    }

    pub fn build_in_recommend(mut self) -> anyhow::Result<quiche::Config> {
        // TODO linux 调优参数
        // self.quiche_config.enable_pacing(true);
        // self.quiche_config.enable_early_data();
        //
        self.quiche_config.enable_dgram(
            true,
            DEFAULT_MAX_RECV_SEND_QUEUE_SIZE,
            DEFAULT_MAX_RECV_SEND_QUEUE_SIZE,
        );
        self.quiche_config.verify_peer(false);
        self.quiche_config.set_disable_active_migration(true);

        self.quiche_config.set_max_stream_window(DEFAULT_MAX_STREAM_WINDOW_SIZE);
        self.quiche_config
            .set_application_protos(&[b"quic-echo-example"])?;
        self.quiche_config
            .set_max_idle_timeout(DEFAULT_MAX_IDLE_TIMEOUT);
        self.quiche_config
            .set_max_recv_udp_payload_size(MAX_PACKET_SIZE);
        self.quiche_config
            .set_max_send_udp_payload_size(MAX_PACKET_SIZE);
        self.quiche_config
            .set_initial_max_data(DEFAULT_MAX_DATA_BUF_SIZE);
        self.quiche_config
            .set_initial_max_stream_data_bidi_local(DEFAULT_MAX_DATA_BUF_SIZE);
        self.quiche_config
            .set_initial_max_stream_data_bidi_remote(DEFAULT_MAX_DATA_BUF_SIZE);
        self.quiche_config
            .set_initial_max_streams_bidi(DEFAULT_MAX_STREAM_CONN);
        self.quiche_config
            .set_initial_max_streams_uni(DEFAULT_MAX_STREAM_CONN);

        Ok(self.quiche_config)
    }
}

pub struct Client {
    client_config: config::ClientConfig,
}

impl Client {
    pub fn new(client_config: ClientConfig) -> Self {
        Client { client_config }
    }

    pub fn connect(&self, quiche_config: quiche::Config) -> anyhow::Result<Receiver<Vec<Packet>>> {
        let (trans, push_rx) = Transport::new(self.client_config.clone(), quiche_config);
        let trans = Box::leak(Box::new(trans));
        let (auth_tx, auth_rx) = mpsc::channel();

        trans.connect_with_retry(auth_tx);

        let response = auth_rx.recv()?;
        match response.status {
            ResponseStatus::Success => {
                let msg = AuthResponse::decode(response.body.as_ref());
                tracing::info!(
                    "connect success addr: {} msg: {msg:?}",
                    self.client_config.addr
                );
            }
            _ => {
                let msg = LinkError::decode(response.body.as_ref());
                tracing::error!(
                    "connect failed addr: {} msg {msg:?}",
                    self.client_config.addr
                );
                return Err(anyhow::anyhow!(format!("connect failed msg: {msg:?}")));
            }
        }

        Ok(push_rx)
    }
}

pub enum ClientEvent {}
