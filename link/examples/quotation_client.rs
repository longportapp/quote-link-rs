// Minimal demo: connect and consume parsed `Quotation`s only.
// For the fuller demo (manual request, raw packet callbacks, echo), see examples/client.rs.
use async_trait::async_trait;
use link::{
    client::{Client, ClientConnEvent, LinkConsumer},
    packets::{parse_quotation, Push, Quotation, Request, Response},
};
use std::net::SocketAddr;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let client_config = link::config::read_from_filepath("tests/config.yaml").unwrap();

    let (quotation_tx, mut quotation_rx) = broadcast::channel::<Quotation>(1024);
    tokio::spawn(async move {
        loop {
            match quotation_rx.recv().await {
                Ok(quotation) => tracing::info!("recv quotation: {quotation:?}"),
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

    let cli = Client::new(client_config);
    match cli.link(move |_| Consumer { quotation_tx }).await {
        Ok(Ok(_)) => tracing::info!("link client link task exit with ok"),
        Ok(Err(err)) => tracing::error!("link client link task exit with err: {err:#}"),
        Err(err) => tracing::error!("link client await link task get err: {err:#}"),
    };
}

struct Consumer {
    quotation_tx: broadcast::Sender<Quotation>,
}

#[async_trait]
impl LinkConsumer for Consumer {
    async fn on_conn_event(&self, event: ClientConnEvent) {
        tracing::info!("on conn event: {event:?}");
    }

    async fn on_connected(&self, peer: SocketAddr) {
        tracing::info!("on connected peer: {peer}");
    }

    async fn on_push(&self, push: Push) {
        if let Some(quotation) = parse_quotation(&push.msg_type, &push.body) {
            let _ = self.quotation_tx.send(quotation);
        }
    }

    async fn on_response(&self, response: Response) {
        tracing::info!("on response: {response:?}");
    }

    async fn on_request(&self, request: Request) {
        tracing::info!("on request: {request:?}");
    }
}
