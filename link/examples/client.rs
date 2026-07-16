use async_trait::async_trait;
use link::{
    client::{Client, ClientConnEvent, LinkConsumer},
    packets::{parse_quotation, Command, Push, Quotation, Request, Response},
    protos::{SubscribeRequest, SubscribeResponse},
};
use std::net::SocketAddr;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let client_config = link::config::read_from_filepath("tests/config.yaml").unwrap();

    // 1. new client
    let cli = Client::new(client_config);

    // 1.5 demo how to use "consumer" outof "link()"
    // Just show HowTo, do as you need
    let (tx, mut rx) = broadcast::channel::<Quotation>(1024);
    tokio::spawn(async move {
        let mut count = 0;
        loop {
            match rx.recv().await {
                Ok(_) => {
                    count += 1;
                    if count % 10000 == 0 {
                        tracing::info!("quotation count: {count}");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::info!("quotation recv chan lagged: {n}");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::error!("quotation recv chan closed");
                    break;
                }
            }
        }
    });

    // 2. link to peer
    match cli.link(move |link_cli| Consumer::new(link_cli, tx)).await {
        Ok(Ok(_)) => tracing::info!("link client link task exit with ok"),
        Ok(Err(err)) => tracing::error!("link client link task exit with err: {err:#}"),
        Err(err) => {
            tracing::error!("link client await link task get err: {err:#}");
            return;
        }
    };
}

#[derive(Clone)]
struct Consumer {
    link_cli: Client,
    quotation_tx: broadcast::Sender<Quotation>,
}

impl Consumer {
    fn new(link_cli: Client, quotation_tx: broadcast::Sender<Quotation>) -> Self {
        Self {
            link_cli,
            quotation_tx,
        }
    }
}

#[async_trait]
impl LinkConsumer for Consumer {
    async fn on_conn_event(&self, event: ClientConnEvent) {
        tracing::info!("on conn event: {event:?}");
    }

    async fn on_connected(&self, peer: SocketAddr) {
        tracing::info!("on connected peer: {peer}");

        // demo how to do C/S request
        // no need manually do sub request, here just for example.
        // (link client do three things:
        // 1. heartbeat and reconnect if connection is lost
        // 2. do auth request and process response
        // 3. do subscribe request, fire and forget
        let cli = self.link_cli.clone();
        tokio::spawn(async move {
            tracing::info!("will do manually request");
            match cli
                .send_request(
                    SubscribeRequest {
                        all: true,
                        counter_ids: vec![],
                        span_index: 0,
                    },
                    Command::CmdSubOrderBook,
                )
                .await
            {
                Ok(response) => {
                    let response: SubscribeResponse = response;
                    tracing::info!("manually request get response: {response:?}")
                }
                Err(err) => tracing::error!("manually request get err: {err:#}"),
            }
        });

        tracing::info!("on connected done");
    }

    async fn on_push(&self, push: Push) {
        tracing::info!("on push: {push:?}");

        // parse link data, do you self logic with packet's raw data
        let quotation = parse_quotation(&push.msg_type, &push.body);
        if let Some(quotation) = quotation {
            self.quotation_tx.send(quotation).unwrap();
        }

        // do echo send packet back to peer
        if let Err(err) = self.link_cli.send_push(push).await {
            tracing::error!("send push to peer get error: {err:#}");
        }
    }

    async fn on_response(&self, response: Response) {
        tracing::info!("on response: {response:?}");
    }

    async fn on_request(&self, request: Request) {
        tracing::info!("on request: {request:?}");
    }
}
