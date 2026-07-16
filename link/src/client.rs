use crate::config::{self, LinkClientConfig};
use crate::packets::{Command, Packet, Push, Request, Response, ResponseStatus};
use crate::protos::{Heartbeat, LinkError};
use crate::transport::Transport;
use crate::utils::{addr_resolve, random_cid};
use anyhow::{Context, Result};
use async_trait::async_trait;
use prost::Message;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::select;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

// BI means bidirectional stream
// 最低位 0x01 为 0 表示 client stream
// 最低位 0x01 为 1 表示 server stream
// 第二低位 0x02 为 0 表示双向 stream
// 第二低位 0x02 为 1 表示单向 stream
pub(crate) const DEFAULT_CLIENT_BI_STREAM_ID: u64 = 0b00;
#[allow(dead_code)]
pub(crate) const DEFAULT_SERVER_BI_STREAM_ID: u64 = 0b01;

#[async_trait]
pub trait LinkConsumer {
    // 连接相关
    async fn on_conn_event(&self, event: ClientConnEvent);
    async fn on_connected(&self, peer: SocketAddr);

    // 处理接收的数据
    async fn on_push(&self, push: Push);
    async fn on_response(&self, response: Response);
    async fn on_request(&self, request: Request);
}

#[derive(Debug, Clone)]
pub struct Client {
    conf: config::LinkClientConfig,
    request_id_gen: Arc<AtomicU32>,
    request_tx: Option<RequestSender>,
    signal_tx: Option<mpsc::Sender<ClientSignal>>,
}

impl Client {
    pub fn new(conf: LinkClientConfig) -> Self {
        Client {
            conf,
            request_id_gen: Arc::new(AtomicU32::new(1)),
            request_tx: None,
            signal_tx: None,
        }
    }
}

impl Client {
    pub async fn send_request<Req, Resp>(&self, req: Req, cmd: Command) -> Result<Resp>
    where
        Req: Message + Default,
        Resp: TryFrom<Response>,
        Resp::Error: std::fmt::Display,
    {
        if cmd == Command::Heartbeat || cmd == Command::Auth {
            anyhow::bail!("not support send request manually for cmd: {cmd:?}");
        }

        match &self.request_tx {
            Some(tx) => {
                let req_id = self.request_id_gen.fetch_add(1, Ordering::Relaxed);
                let packet = Packet::new_request(cmd, req_id, req.encode_to_vec());
                let (response_tx, response_rx) = oneshot::channel();

                // do request
                _ = tokio::time::timeout(
                    self.conf.timeout_config.request,
                    tx.send((Arc::new(packet), Some(response_tx))),
                )
                .await
                .context("send request timeout when send")?;
                tracing::info!("send request id: {req_id} to peer success");

                // wait response
                match tokio::time::timeout(self.conf.timeout_config.response, response_rx).await? {
                    Ok(resp) => {
                        if resp.status != ResponseStatus::Success {
                            if let Ok(link_err) = LinkError::decode(resp.body.as_ref()) {
                                return Err(anyhow::anyhow!(
                                    "response is link error: {link_err:?}"
                                ));
                            }
                        }

                        Ok(Resp::try_from(resp).map_err(|err| {
                            anyhow::anyhow!("convert response to response type failed: {err:#}")
                        })?)
                    }
                    Err(err) => Err(anyhow::anyhow!("wait response timeout: {err:#}")),
                }
            }
            None => Err(anyhow::anyhow!("request tx is not init, call link first")),
        }
    }

    pub async fn send_push(&self, push: Push) -> Result<()> {
        match &self.request_tx {
            Some(tx) => {
                let packet = Packet::Push(push);
                Ok(tx.send((Arc::new(packet), None)).await?)
            }
            None => Err(anyhow::anyhow!("request tx is not init, call link first")),
        }
    }

    pub async fn send_response(&self, response: Response) -> Result<()> {
        match &self.request_tx {
            Some(tx) => {
                let packet = Packet::Response(response);
                Ok(tx.send((Arc::new(packet), None)).await?)
            }
            None => Err(anyhow::anyhow!("request tx is not init, call link first")),
        }
    }

    pub fn link<T: LinkConsumer + Send + Sync + 'static, F: FnOnce(Self) -> T>(
        mut self,
        factory: F,
    ) -> JoinHandle<Result<()>> {
        let (master, backups) = addr_resolve(&self.conf.addr, &self.conf.backup_config)
            .expect("link peer dns resolve failed");
        tracing::info!("link peer dns result master addr: {master} backup addrs: {backups:?}");

        // init channels inside for easy usage
        let (signal_tx, mut signal_rx) = mpsc::channel(self.conf.channel_config.signal_chan_size);
        self.signal_tx = Some(signal_tx.clone());
        let (request_tx, mut request_rx) = mpsc::channel(self.conf.channel_config.send_chan_size);
        self.request_tx = Some(request_tx.clone());

        // prepare
        let conf = self.conf.clone();
        let request_id_gen = self.request_id_gen.clone();
        let requests = Arc::new(Mutex::new(
            HashMap::<u32, oneshot::Sender<Response>>::with_capacity(
                conf.channel_config.signal_chan_size,
            ),
        ));
        let consumer = Arc::new(factory(self));

        // on_xxx callback execute sequentially, this rs-routine may block by use fn on_xxx
        // so spawn separate rs-routine
        let consumer_cloned = consumer.clone();
        let master_cloned = master.clone();
        let requests_cloned = requests.clone();
        let (packet_process_tx, mut packet_process_rx) =
            mpsc::channel::<Packet>(conf.channel_config.process_chan_size);
        tokio::spawn(async move {
            loop {
                select! {
                    rest = packet_process_rx.recv() => match rest {
                        Some(packet) => {
                            match packet {
                                Packet::Request(request) => {
                                    consumer_cloned.on_request(request).await;
                                }
                                Packet::Response(response) => {
                                    let response_tx = {
                                        let mut requests = requests_cloned.lock().await;
                                        requests.remove(&response.id)
                                    };
                                    match response_tx {
                                        Some(response_tx) => {
                                            tokio::spawn(async move {
                                                let _ = response_tx.send(response);
                                            });
                                        },
                                        None => {
                                            match response.command {
                                                Command::Auth if response.status == ResponseStatus::Success => {
                                                    consumer_cloned.on_connected(master_cloned.socket_addr).await;
                                                },
                                                _ => {
                                                    consumer_cloned.on_response(response).await;
                                                }
                                            }
                                        }
                                    }
                                }
                                Packet::Push(push) => {
                                    metrics::receive::receive_count(&master_cloned.raw_addr, &format!("on_push_{:?}", push.msg_type));
                                    consumer_cloned.on_push(push).await;
                                }
                            }
                        }
                        None => {
                            tracing::error!("link client consumer {master_cloned:?} will no more packet recv");
                            break
                        }
                    }
                }
            }
        });

        // main link loop
        // 1. process heartbeat
        // 2. process auth request
        // 3. process reconnect
        // 4. broadcast packet to separate user callback rs-routine
        let link_task = tokio::spawn(async move {
            let mut retry_times: u32 = 0;
            let max_retry_times = conf.healthy_config.retry_times;
            let retry_interval = conf.healthy_config.retry_interval;
            let err = 'conn: loop {
                let tran: Transport = conf.clone().into();

                // gen random scid
                let Ok(scid) = random_cid() else {
                    break anyhow::anyhow!("link peer connect random cid failed");
                };

                // connnection result handler
                let auth_packet = Packet::new_auth(
                    request_id_gen.fetch_add(1, Ordering::Relaxed),
                    &conf.username,
                    &conf.password,
                    &conf.client_name,
                );
                let (inner_request_tx, inner_request_rx) =
                    mpsc::channel::<Arc<Packet>>(conf.channel_config.send_chan_size);
                let (inner_response_tx, mut inner_response_rx) =
                    mpsc::channel::<Packet>(conf.channel_config.recv_chan_size);
                let mut conn_task = tokio::spawn(tran.connect(
                    master.raw_addr.to_string(),
                    master.socket_addr,
                    scid.clone(),
                    auth_packet,
                    inner_request_tx.clone(),
                    inner_request_rx,
                    inner_response_tx,
                ));

                // process response loop
                let mut is_authed = false;
                let conn_created_at = Instant::now();
                let mut last_pong_at = Instant::now();
                let mut ping_ticker = tokio::time::interval(conf.healthy_config.heartbeat_interval);
                let err = 'data: loop {
                    select! {
                        // Ctrl + C interrupt signal
                        _ = tokio::signal::ctrl_c() => {
                            tracing::info!("link client recv ctrl-c signal will exit");
                            let _ = &conn_task.abort();
                            return Ok(());
                        },

                        // process transport result
                        rest = &mut conn_task => match rest {
                            Ok(Ok(_)) => {
                                tracing::info!("link peer transport connect task completed and link client will exit");
                                return Ok(());
                            }
                            Ok(Err(err)) => break 'data anyhow::anyhow!("link peer transport connect task get error: {err:#}"),
                            Err(err) => break 'data anyhow::anyhow!("link peer await transport connect task get error: {err:#}"),
                        },

                        // process heartbeat ticker
                        _ = ping_ticker.tick() => {
                            match is_authed {
                                false => {
                                    if conn_created_at.elapsed() > conf.healthy_config.auth_timeout {
                                        break 'data anyhow::anyhow!("link peer connect wait auth done timeout {master}");
                                    }
                                },
                                true => {
                                    if last_pong_at.elapsed() > conf.healthy_config.heartbeat_timeout {
                                        break 'data anyhow::anyhow!("link peer connect heartbeat timeout {master}");
                                    }
                                }
                            }

                            match inner_request_tx.send(Arc::new(Packet::new_heartbeat(request_id_gen.fetch_add(1, Ordering::Relaxed)))).await {
                                Ok(_) => continue,
                                Err(e) => {
                                    break 'data anyhow::anyhow!("link client send heartbeat to peer get error: {e:?}");
                                }
                            };
                        },

                        // process client exit signal
                        Some(signal) = signal_rx.recv() => {
                            match signal {
                                ClientSignal::Exit(msg) => {
                                    tracing::info!("link peer recv exit signal {msg} connection with {master} will exit");
                                    let _ = &conn_task.abort();

                                    // avoid block main loop
                                    let event = ClientConnEvent::DisconnectedForever(format!("link peer recv exit signal {msg} connection with {master} will exit"));
                                    if let Err(err) = tokio::time::timeout(conf.timeout_config.response, consumer.on_conn_event(event)).await {
                                        tracing::error!("link client on_conn_event get error: {err:#}");
                                    };
                                }
                            }
                            return Ok(());
                        },

                        // request transmit to inner channel (cause by transport may re-new when reconnect)
                        Some((packet, response_tx)) = request_rx.recv() => {
                            #[cfg(debug_assertions)]
                            tracing::debug!("link client send packets to peer: packet: {:?}", packet);

                            match inner_request_tx.send(packet.clone()).await {
                                Ok(_) => {
                                    // save the response tx for send response back
                                    if let Err(err) = tokio::time::timeout(conf.timeout_config.response, async {
                                        if let (Packet::Request(request), Some(response_tx)) = (packet.as_ref(), response_tx) {
                                            let mut requests = requests.lock().await;
                                            requests.insert(request.id, response_tx);
                                        };
                                    }).await {
                                        tracing::error!("link client save response tx to requests get error: {err:#}");
                                    };
                                }
                                Err(e) => {
                                    tracing::error!("link client send packets to peer get error: {e:?}");
                                }
                            };
                        },

                        // process response
                        rest = inner_response_rx.recv() => match rest {
                            Some(packet) => {
                                #[cfg(debug_assertions)]
                                tracing::debug!("link client recv packet from peer: {:?}", packet);

                                // avoid heartbeat packet be blocked when data is too much, cause heartbeat timeout
                                last_pong_at = Instant::now();

                                match packet {
                                    Packet::Response(response) if response.command == Command::Heartbeat => {
                                        // record link heartbeat cost
                                        let now = chrono::Utc::now().timestamp_micros();
                                        match Heartbeat::decode(response.body.as_ref()) {
                                            Ok(heartbeat) => {
                                                let cost = std::time::Duration::from_micros((now - heartbeat.timestamp) as u64);
                                                tracing::debug!("link client with {master} RTT cost: {cost:?} request id: {}", response.id);
                                                metrics::process::cost_micro_sec(&master.raw_addr, &format!("ping-pong-RTT-{}", conf.client_name), cost);
                                            },
                                            Err(err) => {
                                                // should not happen
                                                break 'data anyhow::anyhow!("link client decode heartbeat response get error: {err:#}");
                                            }
                                        }
                                    }

                                    Packet::Response(response) if response.command == Command::Auth => {
                                        if response.status != ResponseStatus::Success {
                                            let msg = LinkError::decode(response.body.as_ref()).unwrap_or(LinkError { code: 0, msg: String::from("unknown error") });
                                            break 'data anyhow::anyhow!("link client auth failed master: {master} msg: {msg:?} will exit");
                                        }

                                        is_authed = true;
                                        if let Err(err) = packet_process_tx.send(Packet::Response(response)).await {
                                            break 'data anyhow::anyhow!("link client send auth response to broadcast process channel get error: {err:#}");
                                        }

                                        tracing::info!("link client will send subscribe request to peer {master:?}");
                                        let sub_packets = conf.subscribes.iter().map(|sub| {
                                            tracing::info!("link client will send subscribe {sub:?} request to peer: {master:?}");
                                            Packet::new_subscribe(sub.cmd, request_id_gen.fetch_add(1, Ordering::Relaxed), sub.sub_all, sub.span_index)
                                        }).collect::<Vec<_>>();
                                        for packet in sub_packets {
                                            match inner_request_tx.send(Arc::new(packet)).await {
                                                Ok(_) => continue,
                                                Err(err) => {
                                                    break 'data anyhow::anyhow!("link client send subscribe request to peer get error: {err:#}");
                                                }
                                            };
                                        };

                                        tracing::info!("link client send subscribe request to peer success {master:?}");
                                    }

                                    _ => {
                                        match packet_process_tx.try_send(packet) {
                                            Ok(_) => continue,
                                            Err(TrySendError::Full(packet)) => {
                                                #[cfg(debug_assertions)]
                                                tracing::debug!("link client consumer {master:?} send to process chan get full error lost packet: {packet:?}");

                                                metrics::process::exception_count(&master.raw_addr, "client_broadcast_rx", "consume_slowly");
                                            },
                                            Err(TrySendError::Closed(_)) => {
                                                break 'data anyhow::anyhow!("link client consumer {master:?} send to process chan get closed error");
                                            }
                                        }
                                    }
                                };
                            },
                            None => break 'data anyhow::anyhow!("link peer connect inner response recv chan closed")
                        },
                    }
                };

                // release background rs-routine task
                let _ = &conn_task.abort();

                // reset request_id_gen
                request_id_gen.store(1, Ordering::Relaxed);

                retry_times += 1;
                if max_retry_times > 0 && retry_times >= max_retry_times {
                    break 'conn anyhow::anyhow!("{err:#} retry times: {retry_times} exceed max retry times: {max_retry_times} will exit the loop");
                }

                metrics::process::exception_count(&master.raw_addr, "link_connection", "reconnect");
                tracing::error!(
                    "link peer loop will reconnect(after {retry_interval:?}) trigger by err: {err:#}"
                );
                let event = ClientConnEvent::Error(format!("Connect to {master} failed: {err:#}"));

                // call consumer must with a timer to avoid block main loop
                if let Err(err) = tokio::time::timeout(
                    conf.timeout_config.response,
                    consumer.on_conn_event(event),
                )
                .await
                {
                    tracing::error!("link client on_conn_event timeout: {err:#}");
                };

                // reconnect interval
                tokio::time::sleep(retry_interval).await;
            };

            let event = ClientConnEvent::DisconnectedForever(format!(
                "Connect to {master} failed err: {err:#}"
            ));
            consumer.on_conn_event(event).await;
            tracing::error!("link peer connect task exit with error: {err:#}");
            Err(err)
        });

        link_task
    }
}

#[derive(Debug, Clone)]
pub enum ClientSignal {
    Exit(String),
}

#[derive(Debug, Clone)]
pub enum ClientConnEvent {
    Error(String),
    DisconnectedForever(String),
}

type RequestSender = mpsc::Sender<(Arc<Packet>, Option<oneshot::Sender<Response>>)>;
