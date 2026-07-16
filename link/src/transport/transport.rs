use crate::config::{
    BufferConfig, ChannelConfig, LinkClientConfig, LinkServerConfig, QuicheConfig, TimeoutConfig,
    DEFAULT_MAX_UDP_PACKET_SIZE,
};
use crate::packets::Packet;
use crate::server::AcceptRest;
use crate::transport::{client, link, server};
use anyhow::{Context, Result};
use quiche::ConnectionId;
use ring::hmac::Key;
use std::cmp::max;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::select;
use tokio::sync::mpsc;

pub(crate) struct Transport {
    // buffer config
    buffer_config: BufferConfig,
    // channel config
    channel_config: ChannelConfig,
    // quiche config
    quiche_config: QuicheConfig,
    // timeout config
    timeout_config: TimeoutConfig,
}

impl From<LinkClientConfig> for Transport {
    fn from(cfg: LinkClientConfig) -> Self {
        Self::from_client_config(cfg)
    }
}

impl From<LinkServerConfig> for Transport {
    fn from(cfg: LinkServerConfig) -> Self {
        Self::from_server_config(cfg)
    }
}

impl Transport {
    pub fn from_client_config(cfg: LinkClientConfig) -> Self {
        tracing::info!("Transport from client with cfg: {cfg:?}");

        Self {
            buffer_config: cfg.buffer_config,
            channel_config: cfg.channel_config,
            quiche_config: cfg.quiche_config,
            timeout_config: cfg.timeout_config,
        }
    }

    pub fn from_server_config(cfg: LinkServerConfig) -> Self {
        tracing::info!("Transport from server with cfg: {cfg:?}");
        Self {
            buffer_config: cfg.buffer_config,
            channel_config: cfg.channel_config,
            quiche_config: cfg.quiche_config,
            timeout_config: cfg.timeout_config,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn connect(
        self,
        peer_name: String,
        peer_addr: SocketAddr,
        scid: ConnectionId<'static>,
        auth_packet: Packet,
        request_tx: mpsc::Sender<Arc<Packet>>,
        request_rx: mpsc::Receiver<Arc<Packet>>,
        response_tx: mpsc::Sender<Packet>,
    ) -> Result<()> {
        let bind_addr = match peer_addr {
            SocketAddr::V4(_) => "0.0.0.0:0",
            SocketAddr::V6(_) => "[::]:0",
        }
        .parse::<SocketAddr>()?;
        let socket = Arc::new(UdpSocket::bind(bind_addr).await?);
        socket.connect(peer_addr).await?;
        let local_addr = socket.local_addr().context("unable to get local addr")?;
        let recv_info = quiche::RecvInfo {
            from: peer_addr,
            to: local_addr,
        };

        let recv_chan_size = self.channel_config.recv_chan_size;
        let send_chan_size = self.channel_config.send_chan_size;

        // QUIC conn 协议握手
        let start = Instant::now();
        let mut cfg: quiche::Config = self.quiche_config.clone().into();
        let conn = match quiche::connect(None, &scid, local_addr, peer_addr, &mut cfg) {
            Ok(conn) => conn,
            Err(err) => {
                tracing::error!("quiche connect failed: {err:#}");
                return Err(anyhow::anyhow!("quiche connect failed: {err:#}"));
            }
        };
        let conn = match client::quic_handshake(
            peer_name.clone(),
            recv_info,
            socket.clone(),
            conn,
            self.buffer_config.max_udp_read_size,
            self.timeout_config.handshake_timeout,
        )
        .await
        {
            Ok(conn) => conn,
            Err(err) => {
                tracing::error!("link client quic handshake failed: {err:#}");
                return Err(anyhow::anyhow!("quic handshake failed: {err:#}"));
            }
        };
        tracing::info!(
            "link client quic handshake client({local_addr:?})<->server:({peer_name}) ok cost {:?}",
            start.elapsed()
        );

        // send the first auth packet
        match request_tx.send(Arc::new(auth_packet)).await {
            Ok(_) => {}
            Err(err) => {
                tracing::error!("link client send first packet to request_tx error: {err:#}");
                return Err(anyhow::anyhow!(
                    "send first packet to request_tx error: {err:#}"
                ));
            }
        };

        tracing::info!(
            "link client quic client({local_addr:?})->server:({peer_name}) send first packet ok"
        );

        // Channelize all process for maximize multi-core CPU utilization
        //
        // Raw UDP socket read loop rs-routine
        // 1. UDP socket read, may block by peer / network / next chan
        //
        // async IO resource cost small
        let (udp_data_recv_tx, udp_data_recv_rx) = mpsc::channel::<Vec<u8>>(recv_chan_size);
        let udp_read_loop_task = tokio::spawn(client::udp_read_loop(
            peer_name.clone(),
            socket.clone(),
            udp_data_recv_tx,
            max(
                self.buffer_config.max_udp_read_size,
                self.quiche_config
                    .max_recv_udp_payload_size
                    .unwrap_or(DEFAULT_MAX_UDP_PACKET_SIZE),
            ),
        ));

        // QUIC conn related process, such as
        // 1. consume udp data, fill with QUIC Conn recv()
        // 2. stream_recv() from QUIC Conn, and pub those data to its producer
        // 3. consume "link data" (wait to send to QUIC Conn), and call stream_send()
        // 4. send() to generate udp data, and send to through its producer
        tracing::info!("will connect local {local_addr:?} peer: {peer_addr:?} scid: {scid:?}");

        let (quic_data_recv_tx, quic_data_recv_rx) = mpsc::channel::<Vec<u8>>(recv_chan_size);
        let (quic_data_send_tx, quic_data_send_rx) = mpsc::channel::<Vec<u8>>(send_chan_size);
        let (udp_data_send_tx, udp_data_send_rx) = mpsc::channel::<Vec<u8>>(send_chan_size);
        let quic_process_loop_task = tokio::spawn(link::quic_process_loop(
            peer_name.clone(),
            recv_info,
            conn,
            // 1. 处理收到的 UDP 数据
            udp_data_recv_rx,
            // 2. 生成对应的 QUIC 数据
            quic_data_recv_tx,
            // 3. 处理收到的 QUI 数据
            quic_data_send_rx,
            // 4. 生成对应的 UDP 数据
            udp_data_send_tx,
            max(
                self.buffer_config.max_udp_read_size,
                self.buffer_config.max_udp_write_size,
            ),
            self.buffer_config.max_stream_read_size,
            self.buffer_config.max_stream_send_size,
            self.timeout_config.idle_interval,
        ));

        // Application level data parse
        // 1. parse "link data" from QUIC Conn stream_recv(), get packets
        // 2. send packets to response tx
        let link_read_loop_task = tokio::spawn(link::link_read_loop(
            peer_name.clone(),
            quic_data_recv_rx,
            response_tx,
            self.buffer_config.max_stream_read_size,
        ));

        // Application level data send
        // 1. consume packets from request_rx, and send to QUIC Conn throught its producer
        let link_write_loop_task = tokio::spawn(link::link_write_loop(
            peer_name.clone(),
            request_rx,
            quic_data_send_tx,
        ));

        // Raw UDP data write loop
        // 1. get udp data from its consumer, and send to UDP socket
        let udp_write_loop_task = tokio::spawn(client::udp_write_loop(
            peer_name.clone(),
            socket,
            udp_data_send_rx,
        ));

        let rest = tokio::try_join!(
            udp_read_loop_task,
            quic_process_loop_task,
            link_read_loop_task,
            link_write_loop_task,
            udp_write_loop_task,
        );

        match rest {
            Ok(_) => Ok(()),
            Err(err) => Err(anyhow::anyhow!(
                "link connect task exit with error: {err:#}"
            )),
        }
    }

    pub async fn listen_at(
        self,
        server_addr: SocketAddr,
        seed: Key,
        accept_tx: mpsc::Sender<AcceptRest>,
    ) -> Result<()> {
        let socket = Arc::new(UdpSocket::bind(server_addr).await?);

        // Channelize all process for maximize multi-core CPU utilization
        //
        // UDP socket accept loop rs-routine
        // 1. UDP socket read, may block by peer / network / next chan
        // 2. accept new client, and launch new QUIC conn to process udp data
        // 3. time ticker to clear closed udp tx channel
        //
        // async IO resource cost small
        let mut chunk_buffer = vec![0u8; self.buffer_config.max_udp_read_size];
        let mut client_socket_data_tx_map =
            HashMap::<SocketAddr, mpsc::Sender<Vec<u8>>>::with_capacity(
                self.buffer_config.peer_conn_init_size,
            );
        // *10 避免过于频繁
        let mut ticker = tokio::time::interval(self.timeout_config.idle_interval * 10);
        let err = 'accept_loop: loop {
            select! {
                _ = ticker.tick() => {
                    // 清理已关闭的 channel
                    client_socket_data_tx_map.retain(|from, tx| {
                        if tx.is_closed() {
                            tracing::info!("link listen at task clear closed udp tx channel from {from:?}");
                            false
                        } else {
                            true
                        }
                    });
                }

                rest = socket.recv_from(&mut chunk_buffer) => match rest {
                    Ok((socket_read, from)) => {
                        #[cfg(debug_assertions)]
                        tracing::debug!("link listen at task server udp read loop recv {socket_read} bytes from peer {from:?}");

                        // new client ask for connect
                        // TODO: stateless retry
                        #[allow(clippy::map_entry)]
                        if !client_socket_data_tx_map.contains_key(&from) {
                            let cfg: quiche::Config = self.quiche_config.clone().into();
                            let (udp_data_recv_tx, udp_data_recv_rx) = mpsc::channel::<Vec<u8>>(self.channel_config.recv_chan_size);

                            // 接入到的第 1 个 udp 数据包, 再入 Q, 方便后续处理
                            let the_first_udp_data = chunk_buffer[..socket_read].to_vec();
                            if let Err(err) = udp_data_recv_tx.try_send(the_first_udp_data) {
                                tracing::error!("link listen at task send the_first_udp_data to udp_data_recv_tx error: {err:#}");
                                continue;
                            };

                            // new client init
                            let (request_tx, response_rx) = match server::accept_new_conn(
                                seed.clone(),
                                cfg,
                                server_addr,
                                from,
                                socket.clone(),
                                udp_data_recv_rx,
                                self.buffer_config,
                                self.channel_config,
                                self.timeout_config,
                            ).await {
                                Ok((request_tx, response_rx)) => (request_tx, response_rx),
                                Err(err) => {
                                    tracing::error!("link listen at task accept new conn error: {err:#}");
                                    continue;
                                }
                            };

                            // 清理已关闭的 channel, 避免依赖定时的清理, map 不断膨胀, 导致 OOM
                            client_socket_data_tx_map.retain(|from, tx| {
                                if tx.is_closed() {
                                    tracing::info!("link listen at task clear closed udp tx channel from {from:?}");
                                    false
                                } else {
                                    true
                                }
                            });

                            // 添加新的
                            client_socket_data_tx_map.insert(from, udp_data_recv_tx);

                            // accept new client
                            let tx = accept_tx.clone();
                            tokio::spawn(async move {
                                if let Err(err) = tx.send((request_tx, response_rx, from)).await {
                                    tracing::error!("link listen at task send accept rest error: {err:#}");
                                }
                            });
                        } else {
                            match client_socket_data_tx_map.get(&from) {
                                Some(tx) => {
                                    if tx.is_closed() {
                                        tracing::debug!("link listen at task udp data consumer channel closed, skip send udp data(from {from:?})");
                                        continue;
                                    };

                                    let data = chunk_buffer[..socket_read].to_vec();

                                    match tx.try_send(data) {
                                        Err(mpsc::error::TrySendError::Full(data)) => {
                                            let t = tx.clone();
                                            tokio::spawn(async move { t.send(data).await });
                                        },
                                        Err(err) => tracing::error!("link listen at task send udp data(from {from:?}) to next chan error: {err:#}"),
                                        Ok(()) => continue
                                    };
                                }
                                None => {
                                    tracing::error!("should not happen new udp data can't find its consumer from {from:?}");
                                    continue
                                }
                            }
                        }

                    }

                    Err(err) => break 'accept_loop anyhow::anyhow!("udp read loop udp socket read from peer get error: {err:#}"),
                }
            }
        };

        tracing::error!("link listen at task exit with err: {err:#}");
        Err(err)
    }
}
