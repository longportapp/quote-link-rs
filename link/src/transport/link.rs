use crate::{
    client::DEFAULT_CLIENT_BI_STREAM_ID,
    packets::{Packet, PacketHeader, MIN_PACKET_HEADER_SIZE},
    simple_buf::SimpleBuf,
};
use anyhow::Result;
use deku::{DekuContainerRead, DekuContainerWrite, DekuError};
use std::time::Instant;
use std::{sync::Arc, time::Duration};
use tokio::{
    select,
    sync::{broadcast, mpsc},
};

macro_rules! conn_send_loop {
    ($conn:expr, $udp_data_buffer:expr, $udp_data_send_tx:expr) => {
        'conn_send: loop {
            let (write, _) = match $conn.send(&mut $udp_data_buffer) {
                Ok((write, send_info)) => (write, send_info),
                Err(quiche::Error::Done) => break 'conn_send,
                Err(err) => return Err(anyhow::anyhow!("udp write loop send get err: {err:#}")),
            };

            if let Err(err) = $udp_data_send_tx.send($udp_data_buffer[..write].to_vec()).await {
                return Err(anyhow::anyhow!("quic process loop conn send udp_data_send_tx error: {err:#}"));
            }
        }
    };

    ($conn:expr, $udp_data_buffer:expr, $udp_data_send_tx:expr, $timeout_signal_tx:expr) => {
        'conn_send: loop {
            let (write, _) = match $conn.send(&mut $udp_data_buffer) {
                Ok((write, send_info)) => (write, send_info),
                Err(quiche::Error::Done) => break 'conn_send,
                Err(err) => return Err(anyhow::anyhow!("udp write loop send get err: {err:#}")),
            };

            if let Some(d) = $conn.timeout() {
                if d.as_millis() < 1 && !d.is_zero() {
                    let tx = $timeout_signal_tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(d).await;
                        if let Err(err) = tx.send(()) {
                            tracing::error!("link process loop send signal error: timeout_signal_tx closed error: {err:#}");
                        }
                    });
                }
            }

            if let Err(err) = $udp_data_send_tx.send($udp_data_buffer[..write].to_vec()).await {
                return Err(anyhow::anyhow!("quic process loop conn send udp_data_send_tx error: {err:#}"));
            }
        }
    };
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub(crate) async fn quic_process_loop(
    /*  */ peer_name: String,
    recv_info: quiche::RecvInfo,
    mut conn: quiche::Connection,
    // 1. 处理收到的 UDP 数据
    mut udp_data_recv_rx: mpsc::Receiver<Vec<u8>>,
    // 2. 生成对应的 QUIC 数据
    quic_data_recv_tx: mpsc::Sender<Vec<u8>>,
    // 3. 处理收到的 QUIC 数据
    mut quic_data_send_rx: mpsc::Receiver<Vec<u8>>,
    // 4. 生成对应的 UDP 数据
    udp_data_send_tx: mpsc::Sender<Vec<u8>>,
    udp_data_buffer_size: usize,
    stream_read_buffer_size: usize,
    stream_send_buffer_size: usize,
    idle_interval: Duration,
) -> Result<()> {
    tracing::info!("quic process loop will start peer={peer_name}");
    // buffer 每次都会用尽
    let mut udp_data_buffer = vec![0u8; udp_data_buffer_size];

    // 处理 QUIC 协议相关数据流
    let mut stream_recv_buffer = vec![0u8; stream_read_buffer_size];
    let mut stream_send_buffer = SimpleBuf::new(stream_send_buffer_size);
    let (timeout_signal_tx, mut timeout_signal_rx) = broadcast::channel::<()>(1);
    // magic 200ms to avoid the last unsent data keep untriggered
    let mut ticker = tokio::time::interval(idle_interval);

    // 需要发送 QUIC 协议数据的四个场景:
    // 1. any time `recv()` is called
    // 2. on_timeout() is called
    // 3. stream_send() is called
    // 4. stream_recv() is called
    let err = 'quic_process: loop {
        select! {
            // 处理 timeout 事件
            rest = timeout_signal_rx.recv() => match rest {
                Ok(_) => {
                    conn.on_timeout();
                    conn_send_loop!(conn, udp_data_buffer, udp_data_send_tx);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    conn.on_timeout();
                    conn_send_loop!(conn, udp_data_buffer, udp_data_send_tx);
                },
                Err(broadcast::error::RecvError::Closed) => break 'quic_process anyhow::anyhow!("quic process loop timeout signal recv closed"),
            },

            // 从 UDP Socket 中接收的数据, 交给 quiche Connection 作协议解析
            // stream_recv() 得到的数据, 继续 push to "next chan"
            rest = udp_data_recv_rx.recv() => match rest {
                Some(mut b) => {
                    let start = Instant::now();

                    let mut sent = 0;
                    while sent < b.len() {
                        match conn.recv(&mut b[sent..], recv_info) {
                            Ok(read) => {
                                sent += read;
                            }
                            Err(err) => {
                                break 'quic_process anyhow::anyhow!("quic process loop conn recv data error: {err:#}");
                            }
                        }
                    };

                    conn_send_loop!(conn, udp_data_buffer, udp_data_send_tx, timeout_signal_tx);

                    let mut total_stream_recv = 0;
                    for stream_id in conn.readable() {
                        'stream_read: loop {
                            match conn.stream_recv(stream_id, &mut stream_recv_buffer) {
                                Ok((read, fin)) => {
                                    total_stream_recv += read;
                                    if let Err(err) = quic_data_recv_tx.send(stream_recv_buffer[..read].to_vec()).await {
                                        break 'quic_process anyhow::anyhow!("quic process loop conn stream recv data stream_recv={total_stream_recv} error: {err:#}");
                                    };

                                    #[cfg(debug_assertions)]
                                    tracing::debug!("quic process loop stream read bytes {read} fin {fin}");

                                    if fin {
                                        break 'quic_process anyhow::anyhow!("quic process loop conn stream recv fin flag");
                                    }
                                }
                                Err(quiche::Error::Done) => break 'stream_read,
                                Err(err) => break 'quic_process anyhow::anyhow!("quic process loop conn stream recv data error: {err:#}"),
                            }
                            conn_send_loop!(conn, udp_data_buffer, udp_data_send_tx, timeout_signal_tx);
                        }
                    }

                    let cost = start.elapsed();
                    metrics::process::cost_micro_sec(&peer_name, "quic_process_loop.read_once", cost);
                    #[cfg(debug_assertions)]
                    tracing::debug!("quic process loop conn read once peer={peer_name} cost={cost:?} recv={sent} stream_recv={total_stream_recv}");
                }
                None => break 'quic_process anyhow::anyhow!("quic process loop udp_data_recv_signal_rx recv closed"),
            },

            // 处理上轮未发完的数据
            _ = ticker.tick() => {
                let start = Instant::now();

                let mut total_stream_send = 0;
                'stream_send: while !stream_send_buffer.is_empty() {
                    match conn.stream_send(DEFAULT_CLIENT_BI_STREAM_ID, stream_send_buffer.occupied_slice(), false) {
                        Ok(sent) => {
                            total_stream_send += sent;
                            if let Err(err) = stream_send_buffer.free(sent) {
                                break 'quic_process anyhow::anyhow!("quic process loop stream send buffer free failed: {err:#}");
                            };

                            #[cfg(debug_assertions)]
                            tracing::debug!("quic process loop stream send bytes {sent} cause last buffer have data unsend, ticker");
                        }
                        Err(quiche::Error::Done) => break 'stream_send,
                        Err(err) => break 'quic_process anyhow::anyhow!("quic process loop conn stream send data stream_send={total_stream_send} error: {err:#}"),
                    }

                    conn_send_loop!(conn, udp_data_buffer, udp_data_send_tx, timeout_signal_tx);
                }

                let cost = start.elapsed();
                metrics::process::cost_micro_sec(&peer_name, "quic_process_loop.send_buffer_once", cost);
                #[cfg(debug_assertions)]
                tracing::debug!("quic process loop send buffer once peer={peer_name} cost={cost:?} stream_send={total_stream_send}");


                if let Some(d) = conn.timeout() {
                    // 只在 is zero 时, 才立即处理, 其情况有 timeout chan 处理超时信号
                    if d.is_zero() {

                        #[cfg(debug_assertions)]
                        tracing::info!("quic process loop conn will do on_timeout {d:?} immediately for peer {peer_name}");

                        conn.on_timeout();
                        conn_send_loop!(conn, udp_data_buffer, udp_data_send_tx);
                    }
                }

                if conn.is_timed_out() {
                    break 'quic_process anyhow::anyhow!("quic process loop conn is timed out");
                }

                if conn.is_closed() {
                    break 'quic_process anyhow::anyhow!("quic process loop conn is closed");
                }
            },

            rest = quic_data_send_rx.recv() => match rest {
                Some(b) => {
                    let start = Instant::now();

                    let mut total_stream_send = 0;
                    // 先发上轮未发完
                    'stream_send: while !stream_send_buffer.is_empty() {
                        match conn.stream_send(DEFAULT_CLIENT_BI_STREAM_ID, stream_send_buffer.occupied_slice(), false) {
                            Ok(sent) => {
                                total_stream_send += sent;
                                if let Err(err) = stream_send_buffer.free(sent) {
                                    break 'quic_process anyhow::anyhow!("quic process loop stream send buffer free failed: {err:#}");
                                };

                                #[cfg(debug_assertions)]
                                tracing::debug!("quic process loop stream send bytes {sent} cause last buffer have data unsend");
                            }
                            Err(quiche::Error::Done) => break 'stream_send,
                            Err(err) => break 'quic_process anyhow::anyhow!("quic process loop conn stream send data stream_send={total_stream_send} error: {err:#}"),
                        }

                        conn_send_loop!(conn, udp_data_buffer, udp_data_send_tx, timeout_signal_tx);
                    }

                    stream_send_buffer.reuse_if();

                    if stream_send_buffer.len() < b.len() {
                        // FIXME: buffer 空间不足时, 看最大空间是否足, 仍不足, 为避免发一半无法发送, 丢掉
                        if stream_send_buffer.max_len() < b.len() {
                            tracing::error!("quic process loop stream send buffer max len {} still less than b len {} drop this data", stream_send_buffer.max_len(), b.len());
                            continue
                        } else {
                            stream_send_buffer.reuse();
                        }
                    }

                    // 再发新收到的数据
                    let mut sent = 0;
                    'stream_send: while sent < b.len() {
                        match conn.stream_send(DEFAULT_CLIENT_BI_STREAM_ID, &b[sent..], false) {
                            Ok(v) => {
                                total_stream_send += v;
                                sent+=v
                            },
                            // 有可能受对端限制, 无法完全发送
                            Err(quiche::Error::Done) => {
                                #[cfg(debug_assertions)]
                                tracing::debug!("quic process loop stream send bytes {sent} send get Done peer may consume slowly");

                                break 'stream_send
                            },
                            Err(err) => break 'quic_process anyhow::anyhow!("quic process loop conn stream send data error: {err:#}"),
                        }
                        conn_send_loop!(conn, udp_data_buffer, udp_data_send_tx, timeout_signal_tx);
                    };

                    // 发不完的, 攒起来
                    if sent != b.len() {
                        #[cfg(debug_assertions)]
                        tracing::debug!("quic process loop buffer unsend bytes {} send next loop", b.len() - sent);

                        if stream_send_buffer.len() < b.len() {
                            if stream_send_buffer.max_len() < b.len() {
                                break 'quic_process anyhow::anyhow!("link read loop packet buffer peer={peer_name} max len {} still less than b len {} drop this data", stream_send_buffer.max_len(), b.len());
                            } else {
                                stream_send_buffer.reuse();
                            }
                        }

                        if let Err(err) = stream_send_buffer.push_slice(&b[sent..]) {
                            break 'quic_process anyhow::anyhow!("quic process loop stream send buffer push slice failed: {err:#}");
                        }
                    }

                    let cost = start.elapsed();
                    metrics::process::cost_micro_sec(&peer_name, "quic_process_loop.send_once", cost);
                    #[cfg(debug_assertions)]
                    tracing::debug!("quic process loop send once peer={peer_name} cost={cost:?} stream_send={total_stream_send}");
                }
                None => break 'quic_process anyhow::anyhow!("quic process loop quic_data_send_rx recv closed"),
            },
        }
    };

    tracing::error!("quic process loop exit with err: {err:#}");
    Err(err)
}

pub(crate) async fn link_read_loop(
    peer_name: String,
    mut quic_data_recv_rx: mpsc::Receiver<Vec<u8>>,
    response_tx: mpsc::Sender<Packet>,
    stream_read_buffer_size: usize,
) -> Result<()> {
    tracing::info!("link process loop will start");
    let mut packet_buffer = SimpleBuf::new(stream_read_buffer_size);
    let mut need_size = MIN_PACKET_HEADER_SIZE;

    let rest = 'link_read: loop {
        select! {
            rest = quic_data_recv_rx.recv() => match rest {
                Some(b) => {
                    let start = Instant::now();
                    let stream_recv = b.len();

                    if packet_buffer.len() < b.len() {
                        if packet_buffer.max_len() < b.len() {
                            break 'link_read anyhow::anyhow!("link read loop packet buffer peer={peer_name} stream_recv={stream_recv} max len {} still less than b len {} drop this data", packet_buffer.max_len(), b.len());
                        } else {
                            packet_buffer.reuse();
                        }
                    }

                    if let Err(err) = packet_buffer.push_slice(&b) {
                        break 'link_read anyhow::anyhow!("link read loop packet buffer push slice failed: {err:#}");
                    }

                    // 解析数据包
                    let mut total_consumed = 0;
                    if packet_buffer.len() >= need_size {
                        match parse_packet_buf(&response_tx, packet_buffer.occupied_slice()).await {
                            Ok((c, n)) => {
                                total_consumed += c;
                                // 出现即解析异常, 后续无法继续解析, 直接退出
                                if let Err(err) = packet_buffer.free(c) {
                                    break 'link_read anyhow::anyhow!("link read loop parse packet get consumed bytes consumed={total_consumed} let advance failed: {err:#}");
                                }
                                need_size = n;
                            }
                            Err(err) => {
                                break 'link_read anyhow::anyhow!("link read loop parse packet buf failed: {err:#}");
                            }
                        }
                    }
                    packet_buffer.reuse_if();

                    let cost = start.elapsed();
                    metrics::process::cost_micro_sec(&peer_name, "link_read_loop.once", cost);
                    #[cfg(debug_assertions)]
                    tracing::debug!("link read loop once peer={peer_name} cost={cost:?} stream_recv={stream_recv} consumed={total_consumed}");
                }
                None => break 'link_read anyhow::anyhow!("link read loop quic_data_recv_rx recv closed"),
            },
        }
    };

    tracing::error!("link read loop exit with err: {rest:#}");
    Err(rest)
}

pub(crate) async fn link_write_loop(
    peer_name: String,
    mut request_rx: mpsc::Receiver<Arc<Packet>>,
    quic_data_send_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    tracing::info!("link write loop will start");
    let err = 'link_write: loop {
        select! {
            b = request_rx.recv() => match b {
                Some(packet) => {
                    let start = Instant::now();
                    #[cfg(debug_assertions)]
                    tracing::debug!("link write loop will send packet: {packet:?}");

                    let mut stream_send = 0;
                    match packet.to_bytes() {
                        Ok(data) => {
                            stream_send += data.len();
                            if let Err(err) = quic_data_send_tx.send(data).await {
                                break 'link_write anyhow::anyhow!("link write loop quic_data_send_tx send data stream_send={stream_send} error: {err:#}");
                            }
                        }
                        Err(err) => {
                            break 'link_write anyhow::anyhow!("link write loop packet to peer {peer_name} failed: {err:#}");
                        }
                    }

                    let cost = start.elapsed();
                    metrics::process::cost_micro_sec(&peer_name, "link_write_loop.once", cost);
                    #[cfg(debug_assertions)]
                    tracing::debug!("link write loop once peer={peer_name} cost={cost:?} gen_stream_send={stream_send}");
                }
                None => break 'link_write anyhow::anyhow!("link write loop request rx is closed"),
            }
        }
    };

    tracing::error!("link write loop exit with err: {err:#}");
    Err(err)
}

pub(crate) async fn parse_packet_buf(
    response_tx: &mpsc::Sender<Packet>,
    mut packet_buffer: &[u8],
) -> Result<(usize, usize)> {
    let mut consumed_bytes = 0;
    let mut occupied_bytes = packet_buffer.len();
    let mut need_size = MIN_PACKET_HEADER_SIZE;
    // not actually effect
    let mut cursor = 0;
    loop {
        // 先做头解析, 避免 body size 过大, 且不充足, 降低效率
        match PacketHeader::from_bytes((packet_buffer, cursor)) {
            Ok(((_rest, _cursor), header)) => {
                need_size = header.need_bytes();
                // 检查是否有足够的数据来解析完整的包
                if occupied_bytes < need_size {
                    return Ok((consumed_bytes, need_size));
                }
            }
            Err(DekuError::Incomplete(_)) => {
                #[cfg(debug_assertions)]
                tracing::debug!(
                    "parse_packet_buf data is incomplete even only header need size: {need_size}"
                );
                return Ok((consumed_bytes, need_size));
            }
            Err(err) => {
                return Err(anyhow::anyhow!("parse packet header failed: {err:#}"));
            }
        }

        // 解析完整 Packet
        match Packet::from_bytes((packet_buffer, cursor)) {
            Ok(((rest, i), packet)) => {
                let packet_size = packet.total_bytes();
                occupied_bytes -= packet_size;
                consumed_bytes += packet_size;
                packet_buffer = rest;
                cursor = i;

                #[cfg(debug_assertions)]
                tracing::debug!(
                    "packet consume need size: {consumed_bytes} packet: {:?}",
                    packet
                );

                // 重置 need size
                need_size = MIN_PACKET_HEADER_SIZE;
                if let Err(err) = response_tx.send(packet).await {
                    // 异常检出, consumer is dead
                    return Err(anyhow::anyhow!(
                        "link read loop send packets to response tx failed: {err:#}"
                    ));
                }
            }
            Err(DekuError::Incomplete(_)) => {
                #[cfg(debug_assertions)]
                tracing::debug!("parse_packet_buf packet is incomplete need size: {need_size}");

                return Ok((consumed_bytes, MIN_PACKET_HEADER_SIZE));
            }
            Err(err) => {
                return Err(anyhow::anyhow!("parse packet failed: {err:#}"));
            }
        }
    }
}
