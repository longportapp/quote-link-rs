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

    let rt = Box::leak(Box::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("bridge-thread")
            .build()
            .unwrap(),
    ));

    let mut quotation_push_rx = link::convert::to_quotation(push_rx, 1024, rt).await;

    // 可以新起, 也可以通用, bridge rt 并无阻塞调用
    rt.spawn(async move {
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
