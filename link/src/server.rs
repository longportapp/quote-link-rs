use crate::{
    config::LinkServerConfig,
    packets::{Command, MsgType, Packet, Push, Request, Response},
    protos::{AuthInfo, AuthRequest},
    transport::Transport,
    utils::{addr_resolve, SYSTEM_RANDOM},
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use prost::Message;
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};
use tokio::{
    select,
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

pub type ClientName = String;
pub type ServerName = String;
pub type SendToPeer = mpsc::Sender<Arc<Packet>>;

#[async_trait]
pub trait ConnHandler {
    // 连接相关
    async fn on_auth(&self, peer: SocketAddr, request_id: u32, auth_info: AuthInfo) -> bool;
    async fn on_disconnect(&self, peer: SocketAddr, auth_info: AuthInfo);

    // 数据接入
    async fn on_push(&self, push: Push);
    async fn on_response(&self, response: Response);
    async fn on_request(&self, request: Request);

    // 数据发送
    async fn on_send(&self, packet: Arc<Packet>);
}

pub struct Server {
    pub conf: LinkServerConfig,
    send_tx: Option<mpsc::Sender<Arc<Packet>>>,
    signal_tx: Option<mpsc::Sender<ServerSignal>>,
}

impl Server {
    pub fn new(conf: LinkServerConfig) -> Self {
        Self {
            conf,
            send_tx: None,
            signal_tx: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ServerSignal {
    CloseClient(ClientName),
    Shutdown,
}

pub type NewClient = (SendToPeer, SocketAddr);

pub(crate) type AcceptRest = (SendToPeer, mpsc::Receiver<Packet>, SocketAddr);

impl Server {
    // 注意: 此 sender 与 serve linker 中 factory 返回的 sender 不是同一个
    // 前者为广播数据, linker 可能由于消费不及, 数据丢掉
    // 后者直达发送给 peer, block until be sent
    pub fn get_sender(&self) -> Result<mpsc::Sender<Arc<Packet>>> {
        self.send_tx.clone().ok_or(anyhow::anyhow!(
            "send tx is not init, call serve_linker first"
        ))
    }

    pub async fn send_quotation<T: prost::Message>(
        &self,
        quotation: T,
        cmd: Command,
        msg_type: MsgType,
    ) -> Result<()> {
        let raw_data = quotation.encode_to_vec();
        self.send_packet(Arc::new(Packet::Push(Push {
            reserved: 0,
            command: cmd,
            msg_type,
            body_len: raw_data.len() as u32,
            body: raw_data,
        })))
        .await
    }

    pub async fn send_packet(&self, packet: Arc<Packet>) -> Result<()> {
        match &self.send_tx {
            Some(tx) => tx
                .send(packet)
                .await
                .context("send packet to server get error"),
            None => Err(anyhow::anyhow!(
                "send tx is not init, call serve_linker first"
            )),
        }
    }

    pub async fn send_signal(&self, signal: ServerSignal) -> Result<()> {
        match &self.signal_tx {
            Some(tx) => tx
                .send(signal)
                .await
                .context("send signal to server get error"),
            None => Err(anyhow::anyhow!(
                "signal tx is not init, call serve_linker first"
            )),
        }
    }

    pub fn serve_linker<T, F>(&mut self, mut factory: F) -> JoinHandle<Result<()>>
    where
        T: ConnHandler + Send + Sync + 'static,
        F: FnMut(NewClient) -> T,
        F: Send + Sync + 'static,
    {
        // DNS
        let (listen_addr, _) =
            addr_resolve(&self.conf.listen_addr, &None).expect("link peer dns resolve failed");

        // gen random seed
        let seed = ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &*SYSTEM_RANDOM)
            .map_err(|err| anyhow::anyhow!("generate seed from hmac key failed: {err:#}"))
            .expect("generate seed from hmac key failed");

        // init channels
        //
        // send chan
        let (send_tx, mut send_rx) =
            mpsc::channel::<Arc<Packet>>(self.conf.channel_config.send_chan_size);
        self.send_tx = Some(send_tx);

        // signal chan
        let (signal_tx, mut signal_rx) =
            mpsc::channel::<ServerSignal>(self.conf.channel_config.signal_chan_size);
        self.signal_tx = Some(signal_tx);

        // signal chan
        let conf = self.conf.clone();

        // launch server
        let server_task = tokio::spawn(async move {
            // new conn chan
            let (accept_tx, mut accept_rx) =
                mpsc::channel::<AcceptRest>(conf.channel_config.signal_chan_size);

            // launch server
            let tran: Transport = conf.clone().into();
            let addr = listen_addr.socket_addr;
            let mut server_task = tokio::spawn(tran.listen_at(addr, seed, accept_tx));

            // conn signal chan
            let (new_client_tx, mut new_client_rx) =
                mpsc::channel::<ClientSignal>(conf.channel_config.signal_chan_size);

            // 1. conn manager signal process
            // 2. new conn signal process
            // 3. broadcast "will send" data to every client
            let mut conn_mgmt_task: JoinHandle<Result<()>> = tokio::spawn(async move {
                // metric interval
                let mut interval = tokio::time::interval(conf.healthy_config.heartbeat_timeout);

                // clients map
                let mut clients: HashMap<ClientName, (ClientBroadcastSender, ClientExitSignal)> =
                    HashMap::with_capacity(conf.buffer_config.peer_conn_init_size);

                loop {
                    select! {
                        // process server signal
                        Some(rest) = signal_rx.recv() => match rest {
                            ServerSignal::CloseClient(client_name) => {
                                match clients.remove(&client_name) {
                                    Some((_, tx)) => {
                                        if let Err(err) = tx.send(()).await {
                                            tracing::error!("serve linker send close signal to client {client_name} get error: {err:#}");
                                            continue;
                                        };

                                        tracing::info!("server send close signal to client {client_name} ok");
                                    },
                                    None => {
                                        tracing::error!("serve linker will close client {client_name} but client not found");
                                        continue;
                                    }
                                };
                            },
                            ServerSignal::Shutdown => {
                                tracing::info!("serve linker recv shutdown signal server will shutdown");
                                return Ok(())
                            },
                        },

                        // process new conn signal
                        Some(rest) = new_client_rx.recv() => match rest {
                            ClientSignal::New(client_name, (broadcast_tx, exit_signal_tx)) => {
                                tracing::info!("serve linker client signal: recv add client signal server will add client: {client_name}");
                                clients.insert(client_name, (broadcast_tx, exit_signal_tx));
                            },
                            ClientSignal::Exit(client_name) => {
                                tracing::error!("serve linker client signal: recv exit client signal server will exit client: {client_name}");
                                clients.remove(&client_name);
                            },
                        },

                        // process metric interval
                        _ = interval.tick() => {
                            metrics::general::msg_gauge("quic-clients", clients.len() as i64);
                        },

                        // send to peer
                        Some(packet) = send_rx.recv() => {
                            for (broadcast_tx, _) in clients.values() {
                                if let Err(err) = broadcast_tx.send(packet.clone()) {
                                    tracing::error!("linker server send data to client get error: {err:#}");
                                }
                            }
                        },
                    }
                }
            });

            // main listen at process loop
            let err = 'conn: loop {
                select! {
                    // Ctrl + C interrupt signal
                    _ = tokio::signal::ctrl_c() => {
                        tracing::info!("serve linker recv ctrl-c signal server will exit");
                        return Ok(())
                    },

                    // server task result
                    rest = &mut server_task => match rest{
                        Ok(Ok(_)) => {
                            tracing::info!("serve linker completed successfully and main listen at loop will exit");
                            return Ok(())
                        }
                        Ok(Err(err)) => {
                            tracing::error!("serve linker completed with error: {err:#}");
                            break 'conn anyhow::anyhow!("serve linker completed with error: {err:#}");
                        }
                        Err(e) => {
                            break 'conn anyhow::anyhow!("wait serve linker result get error: {e:#}");
                        }
                    },

                    // conn management task result
                    rest = &mut conn_mgmt_task => match rest {
                        Ok(Ok(_)) => {
                            tracing::info!("serve linker conn management task completed successfully and main listen at loop will exit");
                            return Ok(())
                        }
                        Ok(Err(err)) => {
                            tracing::error!("serve linker conn management task completed with error: {err:#}");
                            break 'conn anyhow::anyhow!("serve linker conn management task completed with error: {err:#}");
                        }
                        Err(e) => {
                            break 'conn anyhow::anyhow!("wait serve linker conn management task result get error: {e:#}");
                        }
                    },

                    // new conn accepted
                    Some((send_tx, mut recv_rx, peer_addr)) = accept_rx.recv() => {
                        let conn_handler = Arc::new((factory)((send_tx.clone(), peer_addr)));

                        // init channels
                        let (exit_tx, mut exit_rx) = mpsc::channel::<()>(conf.channel_config.signal_chan_size);
                        // TODO: server 收到的 packet 默认比较少, 未做成 broadcash chennl, 如果吞吐过慢, 有可能导致过多 rs-routine
                        let (recved_packet_tx, mut recved_packet_rx) = mpsc::channel::<Packet>(conf.channel_config.process_chan_size);
                        let (on_send_broadcast_tx, mut on_send_broadcast_rx) = broadcast::channel::<Arc<Packet>>(conf.channel_config.process_chan_size);
                        let client_signal_tx_cloned = new_client_tx.clone();

                        // on_xxx callback execute sequentially, this rs-routine may block by use fn on_xxx
                        // so spawn separate rs-routine
                        let conn_handler_cloned = conn_handler.clone();
                        let mut client_name_cloned = format!("waitAuth@{}", peer_addr);
                        tokio::spawn(async move {
                            let err = loop {
                                select! {
                                    // recv packet broadcast channel
                                    rest = recved_packet_rx.recv() => match rest {
                                        Some(packet) => {
                                            let handler = conn_handler_cloned.clone();
                                            match packet {
                                                Packet::Request(request) => {
                                                    if request.command == Command::Auth {
                                                        let Ok(auth_request) = AuthRequest::decode(request.body.as_ref()) else {
                                                            break anyhow::anyhow!("serve linker auth request decode error(should not happen) client={client_name_cloned}");
                                                        };
                                                        let client_name = auth_request.auth_info.map(|info| info.client_name).unwrap_or("lackAuthInfo".to_string());
                                                        client_name_cloned = format!("{}@{}", client_name, peer_addr);
                                                    } else {
                                                        tokio::spawn(async move {
                                                            handler.on_request(request).await;
                                                        });
                                                    }

                                                },
                                                Packet::Response(response) => {
                                                    tokio::spawn(async move {
                                                        handler.on_response(response).await;
                                                    });
                                                },
                                                Packet::Push(push) => {
                                                    tokio::spawn(async move {
                                                        handler.on_push(push).await;
                                                    });
                                                },
                                            }
                                        },
                                        None => break anyhow::anyhow!("serve linker recv packet from client {client_name_cloned} get none(stopped)"),
                                    },

                                    // push data broadcast channel
                                    rest = on_send_broadcast_rx.recv() => match rest {
                                        Ok(packet) => {
                                            if let Packet::Push(push) = packet.as_ref() {
                                                metrics::send::send_count(&client_name_cloned, &format!("on_send_{:?}", push.msg_type));
                                            };

                                            // 顺序执行发送, 保证 clients 数据有序性
                                            conn_handler_cloned.on_send(packet).await;
                                        },
                                        Err(broadcast::error::RecvError::Lagged(_)) => {
                                            metrics::send::send_count(&client_name_cloned, "consume_slowly_on_send");
                                        },
                                        Err(broadcast::error::RecvError::Closed) => {
                                            break anyhow::anyhow!("serve linker client inner_broadcast_rx recv closed client_name: {client_name_cloned}");
                                        },
                                    },
                                }
                            };

                            tracing::error!("serve linker client process callback loop exit with err: {err:#}");
                        });

                        // client main process loop
                        tokio::spawn(async move {
                            let mut is_auth_ok = false;
                            let mut client_name = format!("waitAuth@{}", peer_addr);
                            let mut last_ping_at = Instant::now();
                            let mut heartbeat_interval = tokio::time::interval(conf.healthy_config.heartbeat_interval);
                            let mut auth_info = AuthInfo::default();
                            let err = loop {
                                select! {
                                    // check heartbeat
                                    _ = heartbeat_interval.tick() => {
                                        match is_auth_ok {
                                            true => {
                                                if last_ping_at.elapsed() > conf.healthy_config.heartbeat_timeout {
                                                    break anyhow::anyhow!("serve linker client heartbeat timeout client={client_name}");
                                                }
                                            }
                                            false => {
                                                if last_ping_at.elapsed() > conf.healthy_config.auth_timeout {
                                                    break anyhow::anyhow!("serve linker client auth timeout client={client_name}");
                                                }
                                            }
                                        }
                                    }

                                    // process exit signal
                                    Some(_) = exit_rx.recv() => {
                                        break anyhow::anyhow!("serve linker client exit signal will close it client={client_name}");
                                    }

                                    // process recv Packet
                                    rest = recv_rx.recv() => match rest {
                                        Some(packet) => {
                                            // 有数据收到, 即刷新 ping at
                                            last_ping_at = Instant::now();

                                            #[cfg(debug_assertions)]
                                            tracing::debug!("serve linker recv packet from peer: {:?}", packet);

                                            match packet {
                                                Packet::Request(request) if request.command == Command::Heartbeat => {
                                                    tracing::debug!("serve linker client recv ping request id: {} client={client_name}", request.id);
                                                    let response = Packet::new_heartbeat_response(request);
                                                    if let Err(err) = send_tx.send(Arc::new(response)).await {
                                                        tracing::error!("serve linker client send heartbeat response to client get error: {err:#}");
                                                    };
                                                }

                                                Packet::Request(request) if request.command == Command::Auth => {
                                                    let Ok(auth_req) = AuthRequest::decode(request.body.as_ref()) else {
                                                        break anyhow::anyhow!("serve linker client auth request decode error(should not happen) client={client_name}");
                                                    };

                                                    auth_info = auth_req.auth_info.clone().unwrap_or_default();
                                                    client_name = format!("{}@{}", auth_info.client_name, peer_addr);
                                                    let ok = conn_handler.on_auth(peer_addr, request.id, auth_info.clone()).await;
                                                    if !ok {
                                                        tracing::error!("serve linker client auth failed will close it client={client_name}");
                                                        continue;
                                                    }

                                                    // auth ok
                                                    is_auth_ok = true;
                                                    tracing::info!("serve linker client auth ok client={client_name}");
                                                    if let Err(err) = client_signal_tx_cloned.send(ClientSignal::New(client_name.clone(), (on_send_broadcast_tx.clone(), exit_tx.clone()))).await {
                                                        tracing::error!("serve linker send signal to peer C:{client_name} get error: {err:#}");
                                                    };

                                                    if let Err(err) = recved_packet_tx.send(Packet::Request(request)).await {
                                                        tracing::error!("serve linker send auth request to client get error: {err:#}");
                                                    }
                                                }

                                                _ => {
                                                    // avoid block main loop
                                                    match recved_packet_tx.try_send(packet) {
                                                        Err(mpsc::error::TrySendError::Full(data)) => {
                                                            let tx = recved_packet_tx.clone();
                                                            tokio::spawn(async move { tx.send(data).await });
                                                        }
                                                        Err(err) => tracing::error!("serve linker send packet to client get error: {err:#}"),
                                                        _ => continue
                                                    }
                                                }
                                            };
                                        },
                                        None => break anyhow::anyhow!("serve linker client response_rx recv closed C:{client_name}"),
                                    },
                                }
                            };

                            // 只有 auth 成功的, 才会调用 on_disconnect
                            if is_auth_ok {
                                conn_handler.on_disconnect(peer_addr, auth_info).await;
                            }

                            if let Err(err) = client_signal_tx_cloned.send(ClientSignal::Exit(client_name.clone())).await {
                                tracing::error!("serve linker send signal to client {client_name} get error: {err:#}");
                            };

                            tracing::error!("serve linker client process main loop exit with err: {err:#}");
                        });
                    },
                }
            };

            tracing::error!("serve linker listen at task exit with err: {err:#}");
            Err(err)
        });

        server_task
    }
}

type ClientExitSignal = mpsc::Sender<()>;
// 避免冗余复制
type ClientBroadcastSender = broadcast::Sender<Arc<Packet>>;

enum ClientSignal {
    New(ClientName, (ClientBroadcastSender, ClientExitSignal)),
    Exit(ClientName),
}
