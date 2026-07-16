// Minimal demo: connect and consume raw `Packet`s (no quotation parsing) as an
// async broadcast stream. For parsed quotations, see examples/quotation_client.rs;
// for the fuller demo, see examples/client.rs.
use async_trait::async_trait;
use link::{
    client::{Client, ClientConnEvent, LinkConsumer},
    packets::{Packet, Push, Request, Response},
};
use std::net::SocketAddr;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let client_config = link::config::read_from_filepath("tests/config.yaml").unwrap();

    let (packet_tx, mut packet_rx) = broadcast::channel::<Packet>(1024);
    tokio::spawn(async move {
        loop {
            match packet_rx.recv().await {
                Ok(packet) => tracing::info!("recv packet: {packet}"),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::info!("packet recv chan lagged: {n}");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::error!("packet recv chan closed");
                    break;
                }
            }
        }
    });

    let cli = Client::new(client_config);
    match cli.link(move |_| Consumer { packet_tx }).await {
        Ok(Ok(_)) => tracing::info!("link client link task exit with ok"),
        Ok(Err(err)) => tracing::error!("link client link task exit with err: {err:#}"),
        Err(err) => tracing::error!("link client await link task get err: {err:#}"),
    };
}

struct Consumer {
    packet_tx: broadcast::Sender<Packet>,
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
        let _ = self.packet_tx.send(Packet::Push(push));
    }

    async fn on_response(&self, response: Response) {
        let _ = self.packet_tx.send(Packet::Response(response));
    }

    async fn on_request(&self, request: Request) {
        let _ = self.packet_tx.send(Packet::Request(request));
    }
}
