use async_trait::async_trait;
use chrono::Utc;
use link::{
    packets::{Command, MsgType, Packet, Push, Request, Response, ResponseStatus},
    protos::{AuthInfo, AuthResponse, LinkError, SubscribeRequest, SubscribeResponse},
    quotation::TradePrice,
    server::{ConnHandler, NewClient, SendToPeer, Server, ServerSignal},
};
use prost::Message;
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let server_config = link::config::read_from_filepath("tests/config.yaml").unwrap();

    // 1. new server
    let mut server = Server::new(server_config);

    // 1.5 Demo how to extend ConnHandler function
    let conns = Arc::new(RwLock::new(HashMap::new()));

    // 2. serve linker (with conn handler factory)
    let server_task = server
        .serve_linker(move |(data_tx, peer_addr)| new_handler(conns.clone(), (data_tx, peer_addr)));

    // 3. interact with peer or backgroud server process
    let server = Arc::new(server);

    // server signal
    let s_cloned = server.clone();
    tokio::spawn(async move {
        // Terminate the server after 120 seconds
        tracing::info!("Terminate the server after 120 seconds");
        tokio::time::sleep(Duration::from_secs(120)).await;
        s_cloned.send_signal(ServerSignal::Shutdown).await.unwrap();
    });

    // send data to peer
    let s_cloned = server.clone();
    tokio::spawn(async move {
        let trade = TradePrice {
            src_id: 999,
            symbol: "ST/US/AAPL".to_string(),
            timestamp: 10086,
            price: 10000,
            amount: 100,
            r#type: "I".to_string(),
            side: 1,
            sequence: 1,
            total_amount: 100,
            total_balance: 1000,
            sale_condition: 1,
            open: 10000,
            high: 10000,
            low: 10000,
            open_interest: 10000,
            exchange_id: 1,
            exchange_seq: 1,
            tag: 1,
            nano_timestamp: Utc::now().timestamp_micros() * 1000,
        };
        let raw_data = trade.encode_to_vec();
        let packet = Arc::new(Packet::Push(Push {
            reserved: 0,
            command: Command::CmdSubAllType,
            msg_type: MsgType::Trade,
            body_len: raw_data.len() as u32,
            body: raw_data,
        }));
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Err(err) = s_cloned.send_packet(packet.clone()).await {
                tracing::error!("send packet error: {err:#}");
            };
        }
    });

    // 4. Until server exit
    match server_task.await {
        Ok(Ok(_)) => {
            tracing::info!("server task exit");
        }
        Ok(Err(err)) => {
            tracing::error!("server task exit with error: {err:#}");
        }
        Err(err) => {
            tracing::error!("server task exit with error: {err:#}");
        }
    }
}

fn new_handler(
    conns: Arc<RwLock<HashMap<String, i32>>>,
    (send_to_tx, peer_addr): NewClient,
) -> Handler {
    tracing::info!("new client from {peer_addr}");
    Handler { conns, send_to_tx }
}

struct Handler {
    conns: Arc<RwLock<HashMap<String, i32>>>,
    send_to_tx: SendToPeer,
}

#[async_trait]
impl ConnHandler for Handler {
    // 连接相关
    async fn on_auth(&self, _peer: SocketAddr, request_id: u32, auth_info: AuthInfo) -> bool {
        tracing::info!("on auth: {auth_info:?}");

        // 连接管理 - 示例
        let mut conns = self.conns.write().await;
        let num = conns.entry(auth_info.user_name.clone()).or_insert(0);
        *num += 1;
        tracing::info!(
            "after authed username: {} total conns: {}",
            auth_info.user_name,
            *num
        );

        let auth_response = AuthResponse {
            auth_info: Some(auth_info),
        };
        let raw_data = auth_response.encode_to_vec();
        let response = Response {
            reserved: 0,
            command: Command::Auth,
            id: request_id,
            status: ResponseStatus::Success,
            body_len: raw_data.len() as u32,
            body: raw_data,
        };

        if let Err(err) = self
            .send_to_tx
            .send(Arc::new(Packet::Response(response)))
            .await
        {
            tracing::error!("send auth response to client get error: {err:#}",);
        }
        true
    }

    async fn on_disconnect(&self, peer: SocketAddr, auth_info: AuthInfo) {
        tracing::info!("on disconnect: disconnect with peer {peer} auth_info: {auth_info:?}");
    }

    async fn on_push(&self, push: Push) {
        tracing::info!("on push: {push:?}");
    }

    async fn on_response(&self, response: Response) {
        tracing::info!("on response: {response:?}");
    }

    async fn on_request(&self, request: Request) {
        tracing::info!("on request request: {request:?}");
        match request.command {
            Command::CmdSubAllType | Command::CmdSubTrades | Command::CmdSubOrderBook => {
                let subscribe_request = SubscribeRequest::decode(request.body.as_ref()).unwrap();
                tracing::info!("subscribe request: {subscribe_request:?}");

                let response = SubscribeResponse {
                    success: vec![],
                    failed: vec![],
                }
                .encode_to_vec();
                if let Err(err) = self
                    .send_to_tx
                    .send(Arc::new(Packet::Response(Response {
                        reserved: 0,
                        command: request.command,
                        id: request.id,
                        status: ResponseStatus::Success,
                        body_len: response.len() as u32,
                        body: response,
                    })))
                    .await
                {
                    tracing::error!("send subscribe response to client get error: {err:#}",);
                };
            }
            _ => {
                let link_err = LinkError {
                    code: 400,
                    msg: "unsupported command".to_string(),
                };
                let raw_data = link_err.encode_to_vec();
                if let Err(err) = self
                    .send_to_tx
                    .send(Arc::new(Packet::Response(Response {
                        reserved: 0,
                        command: request.command,
                        id: request.id,
                        status: ResponseStatus::BadRequest,
                        body_len: raw_data.len() as u32,
                        body: raw_data,
                    })))
                    .await
                {
                    tracing::error!(
                        "send unsupported command response to client get error: {err:#}",
                    );
                };
                tracing::info!("unsupported command: {request}");
            }
        }
    }

    async fn on_send(&self, packet: Arc<Packet>) {
        if let Err(err) = self.send_to_tx.send(packet).await {
            tracing::error!("send packet to client get error: {err:#}");
            return;
        };
    }
}
