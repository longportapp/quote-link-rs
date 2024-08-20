use metrics::register_and_server_at;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    tokio::join!(
        async {
            register_and_server_at("", "", vec![]).await;
        },
        async {
            metrics::receive::receive_status_count("quote-link-rs", "example_server");
            let resp = reqwest::get("http://localhost:9102/metrics").await.unwrap();
            println!(
                "metrics response: {}",
                resp.text().await.unwrap_or_default()
            );
        }
    );
}
