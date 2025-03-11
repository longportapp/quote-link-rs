use tokio::select;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let client_config = link::config::get_from_filepath("tests/config.yaml").unwrap();
    let cli = link::client::Client::new(client_config);
    let push_rx = cli
        .connect(
            link::client::QuicheConfigBuilder::new()
                .build_in_recommend()
                .unwrap(),
        )
        .unwrap();

    let mut quotation_push_rx = link::convert::to_quotation(push_rx, 1024).await;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tokio::time::Duration::from_millis(500));
        loop {
            select! {
                t = ticker.tick() => {
                    tracing::debug!("async select ticker active at: {t:?}");
                }

                p = quotation_push_rx.recv() => {
                    if let Ok(quotation) = p {
                        tracing::info!("async select recv quotation: {quotation:?}");
                    }
                }
            }
        }
    });

    tokio::time::sleep(tokio::time::Duration::from_secs(10000)).await;
}
