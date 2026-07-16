use crate::protos::{AuthInfo, AuthRequest, Heartbeat, SubscribeRequest};
use crate::quotation::{Brokers, Depths, Kline, OrderBookV2, Snapshot, TradePrice};
use deku::prelude::*;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

const DEFAULT_REQUEST_TIMEOUT: u8 = 5;

const REQUEST_HEADER_SIZE: usize = 8;

const RESPONSE_HEADER_SIZE: usize = 8;

const PUSH_HEADER_SIZE: usize = 5;

pub(crate) const MIN_PACKET_HEADER_SIZE: usize = 5;

pub fn request_id_trim(id: u32) -> u32 {
    // request id 24bit 3bytes
    id & 0x00ff_ffff
}

pub(crate) fn unix_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(id_type = "u8", bits = 2)]
pub enum Packet {
    #[deku(id = "0b00")]
    Request(Request),
    #[deku(id = "0b01")]
    Response(Response),
    #[deku(id = "0b10")]
    Push(Push),
}

impl Display for Packet {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Packet::Request(request) => write!(
                f,
                "Packet(Request{{ cmd:{:?} body_len:{} body:[..] }})",
                request.command, request.body_len
            ),
            Packet::Response(response) => write!(
                f,
                "Packet(Response{{ cmd:{:?} status:{:?} body_len:{} body:[..] }})",
                response.command, response.status, response.body_len
            ),
            Packet::Push(push) => write!(
                f,
                "Packet(Push{{ cmd:{:?} msg_type:{:?} body_len:{} body:[..] }})",
                push.command, push.msg_type, push.body_len
            ),
        }
    }
}

impl Packet {
    pub fn get_request_id(&self) -> u32 {
        match self {
            Packet::Request(v) => v.id,
            Packet::Response(v) => v.id,
            Packet::Push(_) => 0,
        }
    }

    pub fn total_bytes(&self) -> usize {
        match self {
            Packet::Request(r) => (r.body_len as usize) + REQUEST_HEADER_SIZE,
            Packet::Response(r) => (r.body_len as usize) + RESPONSE_HEADER_SIZE,
            Packet::Push(r) => (r.body_len as usize) + PUSH_HEADER_SIZE,
        }
    }
}

impl Packet {
    pub fn new_request(cmd: Command, id: u32, body: Vec<u8>) -> Self {
        Self::Request(Request {
            reserved: 0,
            command: cmd,
            id: request_id_trim(id),
            timeout: DEFAULT_REQUEST_TIMEOUT,
            body_len: body.len() as u32,
            body,
        })
    }

    pub fn new_response_from(req: &Request, status: ResponseStatus, body: Vec<u8>) -> Self {
        Self::Response(Response {
            reserved: 0,
            command: req.command,
            id: req.id,
            status,
            body_len: body.len() as u32,
            body,
        })
    }

    pub fn new_auth(id: u32, username: &str, password: &str, client_name: &str) -> Self {
        let auth = AuthRequest {
            auth_info: Some(AuthInfo {
                user_name: username.to_string(),
                password: password.to_string(),
                client_name: client_name.to_string(),
            }),
        }
        .encode_to_vec();

        Self::Request(Request {
            reserved: 0,
            command: Command::Auth,
            id: request_id_trim(id),
            timeout: DEFAULT_REQUEST_TIMEOUT,
            body_len: auth.len() as u32,
            body: auth,
        })
    }

    pub fn new_subscribe(cmd: Command, id: u32, sub_all: bool, span_idx: i64) -> Self {
        let sub = SubscribeRequest {
            all: sub_all,
            counter_ids: vec![],
            span_index: span_idx,
        }
        .encode_to_vec();

        Self::Request(Request {
            reserved: 0,
            command: cmd,
            id: request_id_trim(id),
            timeout: DEFAULT_REQUEST_TIMEOUT,
            body_len: sub.len() as u32,
            body: sub,
        })
    }

    pub fn new_heartbeat(id: u32) -> Self {
        let raw_body = Heartbeat {
            timestamp: unix_micros(),
        }
        .encode_to_vec();

        Self::Request(Request {
            reserved: 0,
            command: Command::Heartbeat,
            id: request_id_trim(id),
            timeout: DEFAULT_REQUEST_TIMEOUT,
            body_len: raw_body.len() as u32,
            body: raw_body,
        })
    }

    pub fn new_heartbeat_response(req: Request) -> Self {
        Self::Response(Response {
            reserved: 0,
            command: Command::Heartbeat,
            id: req.id,
            status: ResponseStatus::Success,
            body_len: req.body_len,
            body: req.body,
        })
    }
}

/// Request 布局
/// 0               1               2               3               4
/// 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |typ|res|cmd_cod|           request_id                          |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///  time_out       |              body_len                         |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///                          body(mutable)
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(endian = "big")]
pub struct Request {
    #[deku(bits = 2)]
    pub reserved: u8,
    pub command: Command,
    #[deku(bits = 24)]
    pub id: u32, // cap request_id to 3 bytes
    pub timeout: u8, // server doesn't support yet?
    #[deku(bits = "24", update = "self.body.len()")]
    pub body_len: u32,
    #[deku(count = "body_len")]
    pub body: Vec<u8>,
}

impl Display for Request {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Request{{ cmd:{:?} id:{} body_len:{} body:[..] }}",
            self.command, self.id, self.body_len
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, DekuRead, DekuWrite, Serialize, Deserialize)]
#[deku(
    id_type = "u8",
    bits = 4,
    endian = "endian",
    ctx = "endian: deku::ctx::Endian"
)]
pub enum Command {
    // 认证
    Auth = 0,
    // 心跳
    Heartbeat = 1,
    // 断连
    Close = 2,
    // 重连
    Reconnect = 3,
    // Application cmd 从 4 起
    CmdSubAllType = 4,
    CmdUnSubAllType = 5,
    CmdSubOrderBook = 6,
    CmdUnSubOrderBook = 7,
    CmdSubTrades = 8,
    CmdSubBrokers = 9,
    CmdSubDepth = 10,
    CmdUnSubDepth = 11,
    CmdSubMarketInfo = 12,
    CmdUnSubBrokers = 13,
}

#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(endian = "big")]
pub struct Response {
    #[deku(bits = 2)]
    pub reserved: u8,
    pub command: Command,
    #[deku(bits = 24)]
    pub id: u32,
    pub status: ResponseStatus,
    #[deku(bits = "24", update = "self.body.len()")]
    pub body_len: u32,
    #[deku(count = "body_len")]
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(cmd: Command, id: u32, status: ResponseStatus, body: Vec<u8>) -> Self {
        Self {
            reserved: 0,
            command: cmd,
            id,
            status,
            body_len: body.len() as u32,
            body,
        }
    }
}

impl Display for Response {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Response{{ cmd:{:?} id:{} status:{:?} body_len:{} body: [..]}}",
            self.command, self.id, self.status, self.body_len
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(endian = "big")]
pub struct Push {
    #[deku(bits = 2)]
    pub reserved: u8,
    pub command: Command,
    pub msg_type: MsgType,
    #[deku(bits = 24, update = "self.body.len()")]
    pub body_len: u32,
    #[deku(count = "body_len")]
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, DekuRead, DekuWrite, Serialize, Deserialize)]
#[deku(id_type = "u8", endian = "endian", ctx = "endian: deku::ctx::Endian")]
// 该 type 同 proto 的 QuotationType
pub enum MsgType {
    Unknown = 0,
    Snapshot = 1,
    Trade = 2,
    Depths = 3,
    Brokers = 4,
    MarketInfo = 5,
    OrderBook = 6,
    Kline = 7,
}

#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(id_type = "u8", endian = "endian", ctx = "endian: deku::ctx::Endian")]
pub enum ResponseStatus {
    Success = 0,
    ServerTimeout = 1,
    ClientTimeout = 2,
    BadRequest = 3,
    BadResponse = 4,
    Unauthenticated = 5,
    PermissionDenied = 6,
    ServerInternalError = 7,
    ClientInternalError = 8,
}

#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(id_type = "u8", bits = 2)]
pub(crate) enum PacketHeader {
    #[deku(id = "0b00")]
    Request(RequestHeader),
    #[deku(id = "0b01")]
    Response(ResponseHeader),
    #[deku(id = "0b10")]
    Push(PushHeader),
}

impl PacketHeader {
    pub fn need_bytes(&self) -> usize {
        match self {
            PacketHeader::Request(r) => (r.body_len as usize) + REQUEST_HEADER_SIZE,
            PacketHeader::Response(r) => (r.body_len as usize) + RESPONSE_HEADER_SIZE,
            PacketHeader::Push(r) => (r.body_len as usize) + PUSH_HEADER_SIZE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(endian = "big")]
pub(crate) struct RequestHeader {
    #[deku(bits = 2)]
    pub reserved: u8,
    pub command: Command,
    #[deku(bits = 24)]
    pub id: u32, // cap request_id to 3 bytes
    pub timeout: u8, // server doesn't support yet?
    #[deku(bits = "24")]
    pub body_len: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(endian = "big")]
pub(crate) struct ResponseHeader {
    #[deku(bits = 2)]
    pub reserved: u8,
    pub command: Command,
    #[deku(bits = 24)]
    pub id: u32,
    pub status: ResponseStatus,
    #[deku(bits = "24")]
    pub body_len: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, DekuRead, DekuWrite)]
#[deku(endian = "big")]
pub(crate) struct PushHeader {
    #[deku(bits = 2)]
    pub reserved: u8,
    pub command: Command,
    pub msg_type: MsgType,
    #[deku(bits = "24")]
    pub body_len: u32,
}

#[derive(Clone, Debug)]
pub enum Quotation {
    Trade(TradePrice),
    Price(Snapshot),
    Depth(Depths),
    Broker(Brokers),
    KLine(Kline),
    OrderBook(OrderBookV2),
}

impl Quotation {
    pub fn msg_type(&self) -> MsgType {
        match self {
            Quotation::Trade(_) => MsgType::Trade,
            Quotation::Price(_) => MsgType::Snapshot,
            Quotation::Depth(_) => MsgType::Depths,
            Quotation::Broker(_) => MsgType::Brokers,
            Quotation::KLine(_) => MsgType::Kline,
            Quotation::OrderBook(_) => MsgType::OrderBook,
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        match self {
            Quotation::Trade(data) => data.encode_to_vec(),
            Quotation::Price(data) => data.encode_to_vec(),
            Quotation::Depth(data) => data.encode_to_vec(),
            Quotation::Broker(data) => data.encode_to_vec(),
            Quotation::KLine(data) => data.encode_to_vec(),
            Quotation::OrderBook(data) => data.encode_to_vec(),
        }
    }
}

pub fn parse_quotation(msg_type: &MsgType, data: &Vec<u8>) -> Option<Quotation> {
    match msg_type {
        MsgType::Unknown => None,
        MsgType::Snapshot => Snapshot::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse price err: {e:?}");
                None
            },
            |data| Some(Quotation::Price(data)),
        ),
        &MsgType::Trade => TradePrice::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse trade err: {e:?}");
                None
            },
            |data| Some(Quotation::Trade(data)),
        ),
        MsgType::Depths => Depths::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse depth err: {e:?}");
                None
            },
            |data| Some(Quotation::Depth(data)),
        ),
        MsgType::Brokers => Brokers::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse broker err: {e:?}");
                None
            },
            |data| Some(Quotation::Broker(data)),
        ),
        MsgType::MarketInfo => None,
        MsgType::OrderBook => OrderBookV2::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse order book err: {e:?}");
                None
            },
            |data| Some(Quotation::OrderBook(data)),
        ),
        MsgType::Kline => Kline::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse kline err: {e:?}");
                None
            },
            |data| Some(Quotation::KLine(data)),
        ),
    }
}
