use prost::Message;

use crate::packets::{Command, MsgType, Packet};
use crate::{quotation, LinkError};

#[derive(Clone, Debug)]
pub enum Quotation {
    Trade(quotation::TradePrice),
    Price(quotation::Snapshot),
    Depth(quotation::Depths),
    Broker(quotation::Brokers),
    KLine(quotation::Kline),
    OrderBook(quotation::OrderBookV2),
}

pub async fn to_quotation(
    rx: std::sync::mpsc::Receiver<Vec<Packet>>,
    buf_size: usize,
) -> tokio::sync::broadcast::Receiver<Quotation> {
    let (async_tx, async_rx) = tokio::sync::broadcast::channel(buf_size);
    tokio::task::spawn_blocking(move || loop {
        let moved_async_tx = async_tx.clone();
        match rx.recv() {
            Ok(packets) => {
                tokio::spawn(async move {
                    for packet in packets {
                        match packet {
                            Packet::Request(_) => {
                                tracing::info!("link get request packet drop: {packet}");
                            }
                            Packet::Response(response) => {
                                tracing::info!("link get response packet: {response}");
                                if response.command == Command::StatusNotify {
                                    if let Ok(err_msg) = LinkError::decode(response.body.as_ref()) {
                                        // alarm
                                        tracing::error!(
                                            "link get status notify error: {}",
                                            err_msg.msg
                                        );
                                    };
                                }
                            }
                            Packet::Push(push) => {
                                if let Some(quotation) = parse_quotation(&push.msg_type, &push.body)
                                {
                                    if let Err(e) = moved_async_tx.send(quotation) {
                                        tracing::error!("link sync chan to async one err: {e:?}");
                                    }
                                }
                            }
                        }
                    }
                });
            }
            Err(e) => {
                tracing::error!("link sync chan recv err: {e:?}");
            }
        }
    });

    async_rx
}

pub fn parse_quotation(msg_type: &MsgType, data: &Vec<u8>) -> Option<Quotation> {
    match msg_type {
        MsgType::Unknown => None,
        MsgType::Snapshot => quotation::Snapshot::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse price err: {e:?}");
                None
            },
            |data| Some(Quotation::Price(data)),
        ),
        &MsgType::Trade => quotation::TradePrice::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse trade err: {e:?}");
                None
            },
            |data| Some(Quotation::Trade(data)),
        ),
        MsgType::Depths => quotation::Depths::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse depth err: {e:?}");
                None
            },
            |data| Some(Quotation::Depth(data)),
        ),
        MsgType::Brokers => quotation::Brokers::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse broker err: {e:?}");
                None
            },
            |data| Some(Quotation::Broker(data)),
        ),
        MsgType::MarketInfo => None,
        MsgType::OrderBookV2 => quotation::OrderBookV2::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse order book err: {e:?}");
                None
            },
            |data| Some(Quotation::OrderBook(data)),
        ),
        MsgType::Kline => quotation::Kline::decode(data.as_ref()).map_or_else(
            |e| {
                tracing::error!("link parse kline err: {e:?}");
                None
            },
            |data| Some(Quotation::KLine(data)),
        ),
    }
}

pub async fn asyncify(
    rx: std::sync::mpsc::Receiver<Vec<Packet>>,
    buf_size: usize,
) -> tokio::sync::broadcast::Receiver<Packet> {
    let (async_tx, async_rx) = tokio::sync::broadcast::channel(buf_size);
    tokio::task::spawn_blocking(move || {
        let async_tx = async_tx;
        loop {
            let moved_async_tx = async_tx.clone();
            match rx.recv() {
                Ok(packets) => {
                    tokio::task::spawn(async move {
                        for packet in packets {
                            if let Err(e) = moved_async_tx.send(packet) {
                                tracing::error!("link sync chan to async one err: {e:#?}");
                            }
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("link sync chan recv err: {e:?}");
                }
            }
        }
    });

    async_rx
}
