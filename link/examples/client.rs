fn main() {
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

    loop {
        match push_rx.recv() {
            Ok(packets) => {
                for packet in packets {
                    tracing::info!("recv from quic cli: {packet}");
                }
            }
            Err(e) => {
                tracing::error!("recv err: {e:?}");
            }
        }
    }
}
