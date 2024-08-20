use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Context;
use deku::{DekuContainerRead, DekuContainerWrite, DekuError};
use mio::net::UdpSocket;
use prost::Message;
use quiche::Connection;
use ring::rand::SecureRandom;

use crate::config::{BufMode, ClientConfig, MAX_PACKET_SIZE};
use crate::packets::{
    Command, Packet, PacketHeader, Response, ResponseStatus, MIN_PACKET_HEADER_SIZE,
};
use crate::protos::LinkError;

const CLIENT: mio::Token = mio::Token(0);
const DEFAULT_STREAM_ID: u64 = 0;

const DEFAULT_EVENT_SIZE: usize = 1024;

const DEFAULT_IN_FLIGHT_SIZE: usize = 1024;

const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_millis(100);

const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);

const MAX_UDP_PACKET_SIZE: usize = (1 << 16) - 1;

const MAX_READ_FIRST_SIZE: usize = MAX_PACKET_SIZE - MAX_UDP_PACKET_SIZE;

const DEFAULT_INNER_ERR_CODE: u64 = 500;

const DEFAULT_OK_CODE: u64 = 0;

pub(crate) struct Transport {
    push_tx: Sender<Vec<Packet>>,

    client_config: ClientConfig,
    quiche_config: quiche::Config,
    wait_response: HashMap<u32, (i32, Sender<Response>)>,

    conn_id: AtomicI32,
}

impl Transport {
    pub fn new(
        client_config: ClientConfig,
        quiche_config: quiche::Config,
    ) -> (Self, Receiver<Vec<Packet>>) {
        let (push_tx, push_rx) = mpsc::channel();

        (
            Self {
                push_tx,
                client_config,
                quiche_config,
                wait_response: HashMap::with_capacity(DEFAULT_IN_FLIGHT_SIZE),
                conn_id: AtomicI32::new(0),
            },
            push_rx,
        )
    }
}

impl Transport {
    pub fn connect_with_retry(&'static mut self, auth_tx: Sender<Response>) {
        let peer_addrs = self
            .client_config
            .addr
            .to_socket_addrs()
            .expect("unable to resolve domin")
            .filter(|addr| addr.is_ipv4())
            .collect::<Vec<SocketAddr>>();
        tracing::info!("addr dns result: {:?}", peer_addrs);
        assert_ne!(0, peer_addrs.len());

        let peer_addr = peer_addrs[0];
        let bind_addr = match peer_addr {
            SocketAddr::V4(_) => "0.0.0.0:0",
            SocketAddr::V6(_) => "[::]:0",
        }
        .parse()
        .unwrap();

        _ = std::thread::Builder::new()
            .stack_size(MAX_PACKET_SIZE * 2) // ~ 32Mi
            .name(format!("eventLoop_{}", self.client_config.addr))
            .spawn(move || {
                let mut retry_time = 0;
                loop {
                    let mut socket = UdpSocket::bind(bind_addr).unwrap();

                    // Generate a random source connection ID for the connection.
                    let mut scid = [0; quiche::MAX_CONN_ID_LEN];
                    ring::rand::SystemRandom::new().fill(&mut scid[..]).unwrap();
                    let scid = quiche::ConnectionId::from_ref(&scid);

                    // Create a QUIC connection but not initiate handshake.
                    tracing::info!(
                        "new quic conn({:?}) to {:?} from {:?} scid: {:?} wait initiate handshake",
                        self.client_config.addr,
                        peer_addr,
                        socket.local_addr().unwrap(),
                        scid,
                    );
                    let mut conn = quiche::connect(
                        None,
                        &scid,
                        socket.local_addr().unwrap(),
                        peer_addr,
                        &mut self.quiche_config,
                    )
                    .unwrap();

                    // initiate handshake
                    // magic size from
                    // https://github.com/cloudflare/quiche/blob/master/apps/src/client.rs
                    let mut control_buf = [0; 1350];
                    self.quic_ctl_msg(&socket, &mut conn, &mut control_buf)
                        .unwrap();

                    // launch event loop
                    let mut reconnect_reason = String::new();
                    if let Err(e) = self.event_loop(&mut socket, &mut conn, auth_tx.clone()) {
                        reconnect_reason = format!(
                            "event loop finished and reconnect cause by err: {e} addr: {}",
                            self.client_config.addr
                        );
                        tracing::error!("{reconnect_reason}");
                    }

                    if self.retry_exceed_limit(&mut retry_time) {
                        tracing::error!(
                            "transport retry exceed limit cause by reconnect trigger addr: {}",
                            self.client_config.addr
                        );
                        metrics::receive::receive_status_count(
                            &self.client_config.addr,
                            "quote-link-rs-connect-lost",
                        );
                        self.push_status_notify(
                            DEFAULT_INNER_ERR_CODE,
                            format!(
                                "transport exit forever retry exceed limit: {} addr: {}",
                                self.client_config.retry_times, self.client_config.addr,
                            ),
                        );
                        break;
                    }

                    self.conn_id.fetch_add(1, Ordering::Relaxed);

                    // will reconnect and notify
                    metrics::receive::receive_status_count(
                        &self.client_config.addr,
                        "quote-link-rs-reconnect-occur",
                    );
                    self.push_status_notify(
                        DEFAULT_INNER_ERR_CODE,
                        format!("conn will reconnect trigger by {reconnect_reason}"),
                    );
                }
            });
    }

    pub fn retry_exceed_limit(&self, retry_times: &mut i32) -> bool {
        if self.client_config.retry_times == 0 {
            return false;
        };

        *retry_times += 1;
        if *retry_times > self.client_config.retry_times {
            return true;
        };

        return false;
    }

    pub fn quic_ctl_msg(
        &self,
        socket: &UdpSocket,
        conn: &mut Connection,
        control_buf: &mut [u8],
    ) -> anyhow::Result<()> {
        'write: loop {
            let (write, send_info) = match conn.send(control_buf) {
                Ok(v) => {
                    tracing::debug!("quic ctl conn send ok {}", v.0);
                    v
                }
                Err(quiche::Error::Done) => {
                    break 'write;
                }
                Err(e) => {
                    tracing::error!("quic ctl conn send get unknown err: {e:#}");
                    return Err(anyhow::anyhow!("quic ctl conn send get unknown err: {e:#}"));
                }
            };

            match socket.send_to(&control_buf[..write], send_info.to) {
                Ok(v) => {
                    tracing::debug!("quic ctl socket send_to ok {v}");
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        tracing::debug!("quic ctl socket send_to would block break ctl loop");
                        break 'write;
                    }

                    tracing::error!("quic ctl socket send_to get unknown err: {e:#}");
                    return Err(anyhow::anyhow!(
                        "quic ctl socket send_to get unknown err: {e:#}"
                    ));
                }
            }
        }

        Ok(())
    }

    pub fn curr_conn_id(&self) -> i32 {
        self.conn_id.load(Ordering::Relaxed)
    }

    pub fn auth(
        &mut self,
        tx: Sender<Response>,
        buf: &mut [u8],
        socket: &UdpSocket,
        conn: &mut Connection,
    ) -> anyhow::Result<()> {
        let req = Packet::new_auth(
            &self.client_config.username,
            &self.client_config.password,
            &self.client_config.client_name,
        );

        let id = req.get_request_id();
        if let Err(e) = self.write_all(req, buf, socket, conn) {
            tracing::error!("link client send auth request failed re-send txs also: {e:#}");
            let err_msg = LinkError {
                code: 0,
                msg: "link client send auth request failed".to_string(),
            }
            .encode_to_vec();
            _ = tx.clone().send(Response {
                reserved: 0,
                command: Command::Auth,
                id,
                status: ResponseStatus::BadRequest,
                body_len: err_msg.len() as u32,
                body: err_msg,
            });
            return Err(e);
        }

        self.wait_response.insert(id, (self.curr_conn_id(), tx));
        Ok(())
    }

    pub fn subscribe(
        &self,
        buf: &mut [u8],
        socket: &UdpSocket,
        conn: &mut Connection,
    ) -> anyhow::Result<()> {
        if self.client_config.subscribe_cmds.is_empty() {
            tracing::error!("client config sub cmds is empty!");
        }

        for cmd in &self.client_config.subscribe_cmds {
            self.write_all(Packet::new_subscribe(cmd.clone()), buf, socket, conn)?
        }

        Ok(())
    }

    pub fn heartbeat_if(
        &self,
        last_ping_at: &mut Instant,
        buf: &mut [u8],
        socket: &UdpSocket,
        conn: &mut Connection,
    ) {
        if !conn.is_established() {
            return;
        }
        if last_ping_at.elapsed() < DEFAULT_HEARTBEAT_INTERVAL {
            return;
        }

        tracing::debug!("event poll tx will send heartbeat");
        if let Err(e) = self.write_all(Packet::new_heartbeat(), buf, socket, conn) {
            tracing::error!("heartbeat if write all err: {e:#}");
            return;
        }

        *last_ping_at = Instant::now();
    }

    pub fn push_status_notify(&self, code: u64, msg: String) {
        let err_msg = LinkError { code, msg }.encode_to_vec();
        _ = self.push_tx.send(vec![Packet::Response(Response {
            reserved: 0,
            command: Command::StatusNotify,
            id: 0,
            status: ResponseStatus::Success,
            body_len: err_msg.len() as u32,
            body: err_msg,
        })]);
    }

    pub fn event_loop(
        &mut self,
        socket: &mut UdpSocket,
        conn: &mut Connection,
        auth_tx: Sender<Response>,
    ) -> anyhow::Result<()> {
        let mut poll = mio::Poll::new().unwrap();
        let mut events = mio::Events::with_capacity(DEFAULT_EVENT_SIZE);

        poll.registry()
            .register(socket, CLIENT, mio::Interest::READABLE)
            .unwrap();

        let mut send_auth_cmd = false;

        let mut read_buf = [0; MAX_UDP_PACKET_SIZE];
        let mut write_buf = [0; MAX_UDP_PACKET_SIZE];
        // let mut packet_buf = [0; 1024]; // for test in main thread
        let mut packet_buf = [0; MAX_PACKET_SIZE]; // max ~ 16Mi
        let mut packet_size: usize = 0;

        let mut last_ping_at = Instant::now();
        let mut last_pong_at = Instant::now();
        'poll: loop {
            if let Err(e) = poll.poll(&mut events, Some(DEFAULT_POLL_TIMEOUT)) {
                tracing::error!("event poll get err: {e:#}");
                // TOTO: how to reconnect
                break 'poll;
            };

            // 维护 quic 协议连接本身
            self.quic_ctl_msg(socket, conn, &mut write_buf)?;

            // 业务消息处理
            if conn.is_established() {
                self.heartbeat_if(&mut last_ping_at, &mut write_buf, socket, conn);

                if !send_auth_cmd {
                    self.auth(auth_tx.clone(), &mut write_buf, socket, conn)
                        .context("conn auth cmd get err")?;
                    send_auth_cmd = true;
                }
            }

            if self.is_timeout(&mut last_ping_at, &mut last_pong_at) {
                conn.on_timeout();
                return Err(anyhow::anyhow!(
                    "event loop timeout ping: {:?} pong: {:?}",
                    last_ping_at.elapsed(),
                    last_pong_at.elapsed(),
                ));
            }

            if !events.is_empty() {
                self.read_loop(
                    socket,
                    conn,
                    &mut read_buf,
                    &mut write_buf,
                    &mut packet_buf,
                    &mut packet_size,
                    &mut last_ping_at,
                    &mut last_pong_at,
                )?;

                tracing::debug!("event loop read done");
            }
        }

        Ok(())
    }

    fn is_timeout(&self, last_ping_at: &mut Instant, last_pong_at: &mut Instant) -> bool {
        if last_ping_at.elapsed() > self.client_config.timeout {
            return true;
        }
        if last_pong_at.elapsed() > self.client_config.timeout {
            return true;
        }

        false
    }

    fn manual_keep_heartbeat(
        &self,
        last_ping_at: &mut Instant,
        last_pong_at: &mut Instant,
        buf: &mut [u8],
        socket: &UdpSocket,
        conn: &mut Connection,
    ) {
        self.heartbeat_if(last_ping_at, buf, socket, conn);

        // 避免读饥饿
        if last_pong_at.elapsed() > 2 * DEFAULT_HEARTBEAT_INTERVAL {
            tracing::debug!("manual keep heartbeat refresh last pong at");
            *last_pong_at = Instant::now();
        }
    }

    fn read_loop(
        &mut self,
        socket: &UdpSocket,
        conn: &mut Connection,
        read_buf: &mut [u8],
        write_buf: &mut [u8],
        packet_buf: &mut [u8],
        packet_size: &mut usize,
        last_ping_at: &mut Instant,
        last_pong_at: &mut Instant,
    ) -> anyhow::Result<()> {
        let mut total_recv_size = 0;
        'read_quic: loop {
            let (len, from) = match socket.recv_from(read_buf) {
                Ok(v) => v,

                Err(e) => {
                    // There are no more UDP packets to read, so end the read loop.
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        tracing::debug!("recv() would block");
                        break 'read_quic;
                    }

                    return Err(anyhow::anyhow!("socket recv from get err: {e:?}"));
                }
            };

            total_recv_size += len;
            tracing::debug!("read loop socket recv_from {len} bytes {from}");

            // Process potentially coalesced packets.
            let recv_info = quiche::RecvInfo {
                to: socket.local_addr().unwrap(),
                from,
            };
            match conn.recv(&mut read_buf[..len], recv_info) {
                Ok(v) => {
                    tracing::debug!("feed to conn {len}");
                    if v != len {
                        tracing::error!("conn quic recv feed data not match socket recv");
                        return Err(anyhow::anyhow!("quic conn recv return len not match"));
                    }
                }
                Err(e) => {
                    tracing::error!("recv failed: {:?}", e);
                    return Err(anyhow::anyhow!("quic conn recv get err: {e:?}"));
                }
            };

            // TODO: 读优先, 先尽可能的读, 提升对端网络吞吐
            match self.client_config.buf_mode {
                BufMode::ParseFirst => {
                    self.application_read(
                        socket,
                        conn,
                        read_buf,
                        write_buf,
                        packet_buf,
                        packet_size,
                        last_ping_at,
                        last_pong_at,
                    )?;
                }
                BufMode::ReadFirst => {
                    // 主动刷新 ping pong 避免要读的数据太多, 影响了检活
                    self.manual_keep_heartbeat(last_ping_at, last_pong_at, write_buf, socket, conn);

                    // 即将超过 1 个最大 packet size 强制开始解析
                    if total_recv_size >= MAX_READ_FIRST_SIZE {
                        tracing::debug!("read loop socket total recv_from {total_recv_size} bytes execute MAX_PACKET_SIZE {MAX_READ_FIRST_SIZE}");
                        break;
                    }
                }
            }
        }

        // 读取优先
        if self.client_config.buf_mode == BufMode::ReadFirst {
            self.application_read(
                socket,
                conn,
                read_buf,
                write_buf,
                packet_buf,
                packet_size,
                last_ping_at,
                last_pong_at,
            )?;
        }

        Ok(())
    }

    fn application_read(
        &mut self,
        socket: &UdpSocket,
        conn: &mut Connection,
        read_buf: &mut [u8],
        write_buf: &mut [u8],
        packet_buf: &mut [u8],
        packet_size: &mut usize,
        last_ping_at: &mut Instant,
        last_pong_at: &mut Instant,
    ) -> anyhow::Result<()> {
        // Application level
        // Process all readable streams.
        for stream_id in conn.readable() {
            'parse: loop {
                let (read, fin) = match conn.stream_recv(stream_id, read_buf) {
                    // no data to read
                    Err(quiche::Error::Done) => {
                        break 'parse;
                    }
                    Err(e) => {
                        tracing::error!("stream recv err: {e:#}");
                        break 'parse;
                    }
                    Ok(v) => {
                        tracing::debug!(
                            "stream ({stream_id}) received {} bytes, fin: {}",
                            v.0,
                            v.1
                        );
                        v
                    }
                };

                // Reading data from a stream may trigger queueing of control messages (e. g. MAX_STREAM_DATA).
                // send() should be called after reading.
                self.quic_ctl_msg(socket, conn, write_buf)?;

                self.consume_buf_process_packet(
                    last_ping_at,
                    last_pong_at,
                    &mut read_buf[..read],
                    packet_size,
                    packet_buf,
                    write_buf,
                    socket,
                    conn,
                )?;
                if fin {
                    break 'parse;
                }
            }
        }

        Ok(())
    }

    fn write_all(
        &self,
        packet: Packet,
        buf: &mut [u8],
        socket: &UdpSocket,
        conn: &mut Connection,
    ) -> anyhow::Result<()> {
        tracing::debug!("write all got packet {packet}");
        let b = packet.to_bytes().context("write all packet to byte err")?;
        if b.len() > MAX_PACKET_SIZE {
            // FIXME: drop large packet, as client do no scene
            tracing::error!(
                "write all send body bigger than max packet size: {} drop",
                b.len()
            );
            return Err(anyhow::anyhow!(
                "write all send body bigger than max packet size"
            ));
        }

        tracing::debug!("write all will write {packet}");
        let v = conn
            .stream_send(DEFAULT_STREAM_ID, &b, false)
            .context("write all stream send write to peer err")?;
        // TODO: stream send may not send all
        if v != b.len() {
            tracing::error!(
                "write all stream send success size {v} not equal to data size {}",
                b.len()
            );
            return Err(anyhow::anyhow!(
                "write all stream send success size not equal to data size"
            ));
        };

        'write: loop {
            let (write, send_info) = match conn.send(buf) {
                Ok(v) => v,
                Err(quiche::Error::Done) => {
                    break 'write;
                }
                Err(e) => {
                    // An error occurred, handle it.
                    tracing::error!("write all conn send err: {e:#}");
                    return Err(anyhow::anyhow!("write all conn send err: {e:#}"));
                }
            };

            match socket.send_to(&buf[..write], send_info.to) {
                Ok(v) => {
                    tracing::debug!("write all socket send to ok len {v}");
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        tracing::error!("write all skip cause by socket send would block");
                    } else {
                        tracing::error!("write all skip cause by socket send err: {e:#}");
                    }
                    break 'write;
                }
            }
        }

        Ok(())
    }

    fn consume_buf_process_packet(
        &mut self,
        last_ping_at: &mut Instant,
        last_pong_at: &mut Instant,
        data_buf: &mut [u8],
        packet_size: &mut usize,
        packet_buf: &mut [u8],
        write_buf: &mut [u8],
        socket: &UdpSocket,
        conn: &mut Connection,
    ) -> anyhow::Result<()> {
        let mut consumed_data = false;
        while !consumed_data {
            // 避免 packet_buf + data_buf 溢出
            if !consumed_data
                && *packet_size + data_buf.len() < packet_buf.len()
                && data_buf.len() != 0
            {
                consumed_data = true;
                packet_buf[*packet_size..(*packet_size + data_buf.len())].copy_from_slice(data_buf);

                // 增加 packet size 计数
                tracing::debug!("consume data buf done, data buf size: {} packet size: {packet_size} after consumed packet size: {}", data_buf.len(), *packet_size+data_buf.len());
                *packet_size += data_buf.len();
            };

            let packets = self.parse_packet_buf(packet_size, packet_buf)?;
            if packets.is_empty() {
                return Ok(());
            }

            // 处理 packet
            self.process_packet(last_ping_at, last_pong_at, packets, write_buf, socket, conn)?;
        }
        Ok(())
    }

    fn parse_packet_buf(
        &mut self,
        packet_size: &mut usize,
        packet_buf: &mut [u8],
    ) -> anyhow::Result<Vec<Packet>> {
        tracing::debug!("just enter parse packet buf: packet size: {}", *packet_size);

        // 保护性逻辑, 避免后续 consumed_bytes 消耗空数组, uszie -= 溢出
        if *packet_size <= MIN_PACKET_HEADER_SIZE {
            return Ok(vec![]);
        }

        let mut parsed_packets = Vec::new();
        let mut total_consumed_bytes: usize = 0;
        let mut wait_parse_packet_buf = &packet_buf[..*packet_size];
        let mut wait_parse_bit_begin = 0;
        while *packet_size > MIN_PACKET_HEADER_SIZE {
            // 至少够一个完整的 packet header
            match PacketHeader::from_bytes((wait_parse_packet_buf, wait_parse_bit_begin)) {
                Ok(((_, _), header)) => {
                    let need_size = header.need_bytes();
                    tracing::debug!("header consume need size: {need_size} packet size: {}, wait parse packet buf size: {} wait parse bit begin: {}", *packet_size, wait_parse_packet_buf.len(), wait_parse_bit_begin);
                    if *packet_size < need_size {
                        break;
                    }
                }
                Err(DekuError::Incomplete(_)) => break,
                Err(e) => {
                    tracing::error!("parse packet header deku from bytes err: {e:#}");
                    metrics::receive::receive_status_count(
                        &self.client_config.addr,
                        "quote-link-rs-parse-byte-header-err",
                    );
                    return Err(anyhow::anyhow!(
                        "parse packet header can not parse anymore: {e}"
                    ));
                }
            }

            match Packet::from_bytes((wait_parse_packet_buf, wait_parse_bit_begin)) {
                Ok(((rest, i), packet)) => {
                    let consumed_bytes = packet.total_bytes();
                    tracing::debug!("data consume packet success {packet} buf size:{consumed_bytes} before: packet size: {}, wait parse packet buf size: {} wait parse bit begin: {}", *packet_size, wait_parse_packet_buf.len(), wait_parse_bit_begin);
                    parsed_packets.push(packet);
                    *packet_size -= consumed_bytes;
                    total_consumed_bytes += consumed_bytes;
                    wait_parse_packet_buf = rest;
                    wait_parse_bit_begin = i;
                }
                Err(DekuError::Incomplete(_)) => break,
                Err(e) => {
                    tracing::error!("parse packet deku from bytes err: {e:#}");
                    metrics::receive::receive_status_count(
                        &self.client_config.addr,
                        "quote-link-rs-parse-byte-err",
                    );
                    return Err(anyhow::anyhow!("parse packet can not parse anymore: {e}"));
                }
            };
        }

        // 将剩余拷贝到前面来
        // 栗子:
        // packet_size: 10
        // packet_buf: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
        // consumed_bytes: 6
        // packet_size: 4
        // idx(0) <-X-> idx(6)
        // ...
        // idx(3) <-X-> idx(9)
        // idx = 4 end
        // packet_buf: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
        if total_consumed_bytes != 0 {
            let mut idx = 0;
            while idx < *packet_size {
                packet_buf[idx] = packet_buf[idx + total_consumed_bytes];
                idx += 1;
            }
            tracing::debug!("after copy the rest to front: rest packet size: {} total consumed bytes: {total_consumed_bytes}", *packet_size);
        }
        Ok(parsed_packets)
    }

    fn process_packet(
        &mut self,
        last_ping_at: &mut Instant,
        last_pong_at: &mut Instant,
        packets: Vec<Packet>,
        write_buf: &mut [u8],
        socket: &UdpSocket,
        conn: &mut Connection,
    ) -> anyhow::Result<()> {
        // 主动刷新 ping pong 避免要解析的数据太多, 影响了检活
        self.manual_keep_heartbeat(last_ping_at, last_pong_at, write_buf, socket, conn);

        let mut wait_push = Vec::new();
        for packet in packets {
            tracing::debug!("process new packet: {packet}");
            match packet {
                Packet::Request(request) => {
                    tracing::error!("read loop parse packet get unreachable type");
                    if request.command == Command::Close {
                        self.push_status_notify(
                            DEFAULT_INNER_ERR_CODE,
                            "read loop parse packet get closed `request`".to_string(),
                        );
                    };

                    return Err(anyhow::anyhow!("read loop parse packet get closed request"));
                }
                Packet::Response(response) => {
                    if response.command == Command::Heartbeat {
                        *last_pong_at = Instant::now();
                        metrics::receive::receive_status_count(
                            &self.client_config.addr,
                            "quote-link-rs-heartbeat-ok",
                        );
                    };
                    if response.command == Command::Auth
                        && response.status == ResponseStatus::Success
                    {
                        match self.subscribe(write_buf, socket, conn) {
                            Ok(_) => {
                                self.push_status_notify(
                                    DEFAULT_OK_CODE,
                                    format!(
                                        "connect success will send sub cmd addr: {}",
                                        self.client_config.addr
                                    ),
                                );
                            }
                            Err(e) => {
                                self.push_status_notify(
                                    DEFAULT_OK_CODE,
                                    format!(
                                        "connect failed with err: {e:#} addr: {}",
                                        self.client_config.addr
                                    ),
                                );
                            }
                        }
                    }
                    if response.status != ResponseStatus::Success {
                        let payload = LinkError::decode(response.body.as_ref());
                        tracing::error!( "read loop parse packet get not success response: {response:?}, payload: {payload:?}" );
                        self.push_status_notify(
                            DEFAULT_INNER_ERR_CODE,
                            format!(
                                "got != ok response: {payload:?} addr: {}",
                                self.client_config.addr
                            ),
                        );
                    }

                    let id = response.id;
                    if let Some((_, tx)) = self.wait_response.get(&id) {
                        tracing::debug!("will send response back to channel: {id}");
                        if let Err(e) = tx.send(response.clone()) {
                            tracing::error!(
                            "parse packet get enough data but send response{response:?} get err: {e:#}"
                        );
                        };
                        self.wait_response.remove(&id);
                    }
                }
                Packet::Push(push) => {
                    metrics::receive::receive_count(
                        &self.client_config.addr,
                        &format!("quote-link-rs-msg-{:?}", push.msg_type),
                    );
                    wait_push.push(Packet::Push(push));
                }
            };
        }

        if !wait_push.is_empty() {
            if let Err(e) = self.push_tx.send(wait_push) {
                tracing::error!("parse packet get enough data but send push out get err: {e:#}");
            };
        }

        Ok(())
    }
}
