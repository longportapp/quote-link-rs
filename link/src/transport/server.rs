use crate::{
    config::{BufferConfig, ChannelConfig, TimeoutConfig},
    packets::Packet,
    transport::link,
    utils::sign_cid,
};
use anyhow::Result;
use ring::hmac::Key;
use std::time::Instant;
use std::{cmp::max, net::SocketAddr, sync::Arc};
use tokio::{net::UdpSocket, select, sync::mpsc};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn accept_new_conn(
    seed: Key,
    mut cfg: quiche::Config,
    server_addr: SocketAddr,
    client_addr: SocketAddr,
    socket: Arc<UdpSocket>,
    mut udp_data_recv_rx: mpsc::Receiver<Vec<u8>>,
    buffer_config: BufferConfig,
    channel_config: ChannelConfig,
    timeout_config: TimeoutConfig,
) -> Result<(mpsc::Sender<Arc<Packet>>, mpsc::Receiver<Packet>)> {
    // new conncetion resposne / request txs
    let (response_tx, response_rx) = mpsc::channel::<Packet>(channel_config.recv_chan_size);
    let (request_tx, request_rx) = mpsc::channel::<Arc<Packet>>(channel_config.send_chan_size);

    tokio::spawn(async move {
        let mut chunk_buffer = vec![0u8; buffer_config.max_udp_read_size];
        let mut conn: Option<quiche::Connection> = None;
        let start = Instant::now();
        loop {
            if let Some(c) = &conn {
                if c.is_established() {
                    break;
                }
                tracing::info!("conn is not established, wait for udp data from C:{client_addr}");
            };

            select! {
                rest = udp_data_recv_rx.recv() => match rest {
                    Some(mut data) => {
                        #[cfg(debug_assertions)]
                        tracing::debug!("link server quic handshake recv udp data from C:{client_addr} len {} data: {:?}", data.len(), &data);

                        let hdr = match quiche::Header::from_slice(&mut data, quiche::MAX_CONN_ID_LEN) {
                            Ok(hdr) => hdr,
                            Err(err) => {
                                tracing::error!("quic handshake server recv udp data error: {err:#}");
                                return
                            }
                        };

                        #[cfg(debug_assertions)]
                        tracing::debug!("link server quic handshake recv udp data from C:{client_addr} hdr: {:?}", hdr);

                        if conn.is_none() {
                            // use hdr.dcid because https://github.com/cloudflare/quiche/blob/master/apps/src/bin/quiche-server.rs#L261
                            // learn more QUIC RFC: https://datatracker.ietf.org/doc/rfc9000/
                            let scid = sign_cid(&seed, &hdr.dcid);
                            tracing::info!("link server quic handshake header: {hdr:?}, server's scid: {scid:?}");
                            conn = match quiche::accept(&scid, None, server_addr, client_addr, &mut cfg) {
                                Ok(conn) => Some(conn),
                                Err(err) => {
                                    tracing::error!("quic handshake server accept conn error: {err:#}");
                                    return
                                }
                            };
                        };

                        let c = conn.as_mut().unwrap();

                        // 创建 server 端 QUIC Connection
                        let recv_info = quiche::RecvInfo {
                            from: client_addr,
                            to: server_addr,
                        };

                        let mut read = 0;
                        while read < data.len() {
                            match c.recv(&mut data[read..], recv_info) {
                                Ok(r) => {
                                    read += r;
                                }
                                Err(quiche::Error::UnknownVersion) => {
                                    // k8s 环境中, Pod restart 后, client 继续发送 "old QUIC packet" but server 已经重启
                                    // FIXME: 如果需要做 seamless reconnect, 这里需要精细化处理
                                    tracing::error!("quic handshake server conn recv data error: UnknownVersion");
                                    return
                                }
                                Err(err) => {
                                    tracing::error!("quic handshake server conn recv data error: {err:#}");
                                    return
                                }
                            }
                        }

                        // 写响应
                        loop {
                            let (write, info) = match c.send(&mut chunk_buffer) {
                                Ok(v) => v,
                                Err(quiche::Error::Done) => break,
                                Err(err) => {
                                    tracing::error!("quic handshake server conn send data error: {err:#}");
                                    return
                                }
                            };

                            let mut sent = 0;
                            while sent < write {
                                match socket.send_to(&chunk_buffer[sent..write], info.to).await {
                                    Ok(v) => sent += v,
                                    Err(err) => {
                                        tracing::error!("quic handshake server send udp data error: {err:#}");
                                        return
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        tracing::error!("link server quic handshake udp_data_recv_rx recv closed");
                        return
                    }
                }
            }
        }

        tracing::info!(
            "link server quic handshake ok S:{server_addr} <-> C:{client_addr} ok cost {:?}",
            start.elapsed()
        );

        let peer_name = format!("client:{}", client_addr);

        // process new client
        let (quic_data_recv_tx, quic_data_recv_rx) =
            mpsc::channel::<Vec<u8>>(channel_config.recv_chan_size);
        let (quic_data_send_tx, quic_data_send_rx) =
            mpsc::channel::<Vec<u8>>(channel_config.send_chan_size);
        let (udp_data_send_tx, udp_data_send_rx) =
            mpsc::channel::<Vec<u8>>(channel_config.send_chan_size);

        // QUIC conn related process, such as
        // 1. consume udp data, fill with QUIC Conn recv()
        // 2. stream_recv() from QUIC Conn, and pub those data to its producer
        // 3. consume "link data" (wait to send to QUIC Conn), and call stream_send()
        // 4. send() to generate udp data, and send to throught its producer
        let recv_info = quiche::RecvInfo {
            from: client_addr,
            to: server_addr,
        };
        let conn = conn.unwrap();
        tokio::spawn(link::quic_process_loop(
            peer_name.clone(),
            recv_info,
            conn,
            udp_data_recv_rx,
            quic_data_recv_tx,
            quic_data_send_rx,
            udp_data_send_tx,
            max(
                buffer_config.max_udp_read_size,
                buffer_config.max_udp_write_size,
            ),
            buffer_config.max_stream_read_size,
            buffer_config.max_stream_send_size,
            timeout_config.idle_interval,
        ));

        // Application level data parse
        // 1. parse "link data" from QUIC Conn stream_recv(), get packets
        // 2. send packets to response tx
        tokio::spawn(link::link_read_loop(
            peer_name.clone(),
            quic_data_recv_rx,
            response_tx,
            buffer_config.max_stream_read_size,
        ));

        // Application level data send
        // 1. consume packets from request_rx, and send to QUIC Conn throught its producer
        tokio::spawn(link::link_write_loop(
            peer_name.clone(),
            request_rx,
            quic_data_send_tx,
        ));

        // Raw UDP data write loop
        // 1. get udp data from its consumer, and send to UDP socket
        tokio::spawn(udp_write_loop(
            peer_name.clone(),
            client_addr,
            socket.clone(),
            udp_data_send_rx,
        ));
    });

    Ok((request_tx, response_rx))
}

pub(crate) async fn udp_write_loop(
    peer_name: String,
    peer: SocketAddr,
    scoket_s: Arc<UdpSocket>,
    mut udp_data_send_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    tracing::info!("server udp write loop will start");
    let err = 'udp_write: loop {
        select! {
            rest = udp_data_send_rx.recv() => match rest {
                Some(b) => {
                    let start = Instant::now();

                    let total = b.len();
                    let mut sent = 0;
                    while sent < total {
                        match scoket_s.send_to(&b[sent..total], peer).await {
                            Ok(v) => {
                                #[cfg(debug_assertions)]
                                tracing::debug!("server udp write loop send {v} bytes to peer");
                                sent += v
                            }
                            Err(err) => break 'udp_write anyhow::anyhow!("server udp write loop send udp data to peer {peer_name} error: {err:#}"),
                        }
                    };

                    let cost = start.elapsed();
                    metrics::process::cost_micro_sec(&peer_name, "udp_write_loop.once", cost);
                    #[cfg(debug_assertions)]
                    tracing::debug!("server udp write loop send once peer({peer_name}) cost={cost:?} socket_write={total}");
                }
                None => break 'udp_write anyhow::anyhow!("server udp write loop udp_data_send_rx recv closed"),
            },
        }
    };
    tracing::error!("server udp write loop exit with err: {err:#}");
    Err(err)
}

#[allow(dead_code)]
const TOKEN_PREFIX: &[u8] = b"link-rs";
#[allow(dead_code)]
const TOKEN_PREFIX_LEN: usize = TOKEN_PREFIX.len();

// Generate a stateless retry token.
//
// The token includes the static string `"quiche"` followed by the IP address
// of the client and by the original destination connection ID generated by the
// client.
//
// Note that this function is only an example and doesn't do any cryptographic
// authenticate of the token. *It should not be used in production system*.
//
// Ref: https://github.com/cloudflare/quiche/blob/master/quiche/examples/server.rs#L398
#[allow(dead_code)]
fn mint_token(hdr: &quiche::Header, src: &SocketAddr) -> Vec<u8> {
    let mut token = Vec::new();

    token.extend_from_slice(TOKEN_PREFIX);

    let addr = match src.ip() {
        std::net::IpAddr::V4(a) => a.octets().to_vec(),
        std::net::IpAddr::V6(a) => a.octets().to_vec(),
    };

    token.extend_from_slice(&addr);
    token.extend_from_slice(&hdr.dcid);

    token
}

// Validates a stateless retry token.
//
// This checks that the ticket includes the `"quiche"` static string, and that
// the client IP address matches the address stored in the ticket.
//
// Note that this function is only an example and doesn't do any cryptographic
// authenticate of the token. *It should not be used in production system*.
// Ref: https://github.com/cloudflare/quiche/blob/master/quiche/examples/server.rs#L421
#[allow(dead_code)]
fn validate_token<'a>(src: &SocketAddr, token: &'a [u8]) -> Option<quiche::ConnectionId<'a>> {
    if token.len() < TOKEN_PREFIX_LEN {
        return None;
    }

    if &token[..TOKEN_PREFIX_LEN] != TOKEN_PREFIX {
        return None;
    }

    let token = &token[TOKEN_PREFIX_LEN..];

    let addr = match src.ip() {
        std::net::IpAddr::V4(a) => a.octets().to_vec(),
        std::net::IpAddr::V6(a) => a.octets().to_vec(),
    };

    if token.len() < addr.len() || &token[..addr.len()] != addr.as_slice() {
        return None;
    }

    Some(quiche::ConnectionId::from_ref(&token[addr.len()..]))
}
