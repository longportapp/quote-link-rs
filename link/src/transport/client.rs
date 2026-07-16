use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::select;
use tokio::sync::mpsc;

pub(crate) async fn udp_read_loop(
    peer_name: String,
    socket_r: Arc<UdpSocket>,
    udp_data_recv_tx: mpsc::Sender<Vec<u8>>,
    buffer_size: usize,
) -> Result<()> {
    tracing::info!("client udp read loop will start");
    let mut chunk_buffer = vec![0u8; buffer_size];
    let rest = 'udp_read: loop {
        select! {
            rest = socket_r.recv(&mut chunk_buffer) => match rest {
                Ok(socket_read) => {
                    let start = Instant::now();

                    if let Err(err) = udp_data_recv_tx.send(chunk_buffer[..socket_read].to_vec()).await {
                        break 'udp_read anyhow::anyhow!("client udp read loop send to udp_data_recv_tx error: {err:#}");
                    }

                    let cost = start.elapsed();
                    metrics::process::cost_micro_sec(&peer_name, "udp_read_loop.once", cost);
                    #[cfg(debug_assertions)]
                    tracing::debug!("client udp read loop once peer={peer_name} cost={cost:?} socket_read={socket_read}");
                }
                Err(err) => break 'udp_read anyhow::anyhow!("client udp read loop udp socket read from peer {peer_name} get error: {err:#}"),
            },
        }
    };

    tracing::error!("client udp read loop exit with err: {rest:#}");
    Err(rest)
}

pub(crate) async fn quic_handshake(
    peer_name: String,
    recv_info: quiche::RecvInfo,
    socket: Arc<UdpSocket>,
    mut conn: quiche::Connection,
    udp_data_buffer_size: usize,
    handshake_timeout: Duration,
) -> Result<quiche::Connection> {
    let local_addr = recv_info.to;
    let mut chunk_buffer = vec![0u8; udp_data_buffer_size];
    loop {
        if conn.is_established() {
            return Ok(conn);
        }

        match conn.send(&mut chunk_buffer) {
            Ok((write, _)) => {
                #[cfg(debug_assertions)]
                tracing::debug!(
                    "quic handshake client({local_addr:?})->server:({peer_name}) conn send udp data len {write} data: {:?}",
                    &chunk_buffer[..write]
                );
                if let Err(err) = socket.send(&chunk_buffer[..write]).await {
                    return Err(anyhow::anyhow!(
                        "quic handshake client({local_addr:?})->server:({peer_name}) send udp_data_send_tx error: {err:#}"
                    ));
                }
            }
            Err(quiche::Error::Done) => {
                tracing::info!(
                    "quic handshake client({local_addr:?})->server:({peer_name}) conn no data wait to send, just wait recv {peer_name} data"
                );
            }
            Err(err) => {
                return Err(anyhow::anyhow!(
                    "quic handshake client({local_addr:?})->server:({peer_name}) conn send get err: {err:#}"
                ));
            }
        }
        let len = tokio::time::timeout(handshake_timeout, socket.recv(&mut chunk_buffer)).await??;
        #[cfg(debug_assertions)]
        tracing::debug!(
            "quic handshake client({local_addr:?})<-server:({peer_name}) conn recv udp data len {len} data: {:?}",
            &chunk_buffer[..len]
        );
        conn.recv(&mut chunk_buffer[..len], recv_info)?;
    }
}

pub(crate) async fn udp_write_loop(
    peer_name: String,
    socket_s: Arc<UdpSocket>,
    mut udp_data_send_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    tracing::info!("client udp write loop will start");
    let err = 'udp_write: loop {
        select! {
            rest = udp_data_send_rx.recv() => match rest {
                Some(b) => {
                    let start = Instant::now();

                    let total = b.len();
                    let mut sent = 0;
                    while sent < total {
                        match socket_s.send(&b[sent..total]).await {
                            Ok(v) => {
                                sent += v;
                            }
                            Err(err) => {
                                break 'udp_write anyhow::anyhow!("client udp write loop send udp data to peer {peer_name} error: {err:#}");
                            }
                        }
                    };

                    let cost = start.elapsed();
                    metrics::process::cost_micro_sec(&peer_name, "udp_write_loop.once", cost);
                    #[cfg(debug_assertions)]
                    tracing::debug!("client udp write loop once peer={peer_name} cost={cost:?} socket_write={total}");
                }
                None => break 'udp_write anyhow::anyhow!("client udp write loop udp_data_send_rx recv closed"),
            },
        }
    };
    tracing::error!("client udp write loop exit with err: {err:#}");
    Err(err)
}
