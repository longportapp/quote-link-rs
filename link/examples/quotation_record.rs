use std::fs::OpenOptions;
use std::fs::File;
use std::io::Write;
use link::convert::Quotation;
use tokio::select;


fn generate_filename(file_type: &str, timestamp: i64, freq: i64) -> String {
    let days = timestamp / (24 * 3600);
    let start_second = (timestamp % (24 * 3600)) / (freq * 60) * (freq * 60);
    let end_second = start_second + (freq * 60);
    format!("{}_{}_{}_{}.txt", file_type, days, start_second, end_second)
}


fn open_file(trade_type: &str, timestamp: i64, freq: i64) -> (String, File) {
    let filename = generate_filename(trade_type, timestamp, freq);
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&filename)
        .unwrap();
    (filename, file)
}


#[tokio::main]
async fn main() {
    // init record files
    let freq = 1;
    let mut global_timestamp = 0;
    let (mut filename_price, mut file_price) = open_file("Price", global_timestamp, freq);
    let (mut filename_trade, mut file_trade) = open_file("Trade", global_timestamp, freq);
    let (mut filename_depth, mut file_depth) = open_file("Depth", global_timestamp, freq);
    let (mut filename_others, mut file_others) = open_file("Others", global_timestamp, freq);
    // let mut filename_price = generate_filename("Price", 0, freq);
    // let mut file_price = OpenOptions::new()
    //     .append(true)
    //     .create(true)
    //     .open(filename_price.as_str())
    //     .unwrap();
    // let mut filename_trade = generate_filename("Trade", 0, freq);
    // let mut file_trade = OpenOptions::new()
    //     .append(true)
    //     .create(true)
    //     .open(filename_trade.as_str())
    //     .unwrap();
    // let mut filename_others = generate_filename("Others", 0, freq);
    // let mut file_others = OpenOptions::new()
    //     .append(true)
    //     .create(true)
    //     .open(filename_others.as_str())
    //     .unwrap();

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
                        // tracing::info!("async select recv quotation: {quotation:?}");       
                        match quotation {
                            Quotation::Trade(trade_price) => {
                                // 根据时间戳生成日志文件名称
                                global_timestamp = trade_price.timestamp; // 更新global_timestamp用于其他quotation的切片
                                let next_filename_trade = generate_filename("Trade", trade_price.timestamp, freq);
                                // 判断文件名称是否一致
                                if filename_trade == next_filename_trade {
                                } else {
                                    file_trade = OpenOptions::new()
                                        .append(true)
                                        .create(true)
                                        .open(next_filename_trade.as_str())
                                        .unwrap();
                                    filename_trade = next_filename_trade;
                                }
                                // 写入文件
                                if let Err(e) = writeln!(file_trade, "{:?}", trade_price) {
                                    eprintln!("Failed to write to file: {}", e);
                                }
                            }
                            Quotation::Price(snapshot) => {
                                // 根据时间戳生成日志文件名称
                                let next_filename_price = generate_filename("Price", snapshot.timestamp, freq);
                                // 判断文件名称是否一致
                                if filename_price == next_filename_price {
                                } else {
                                    file_price = OpenOptions::new()
                                        .append(true)
                                        .create(true)
                                        .open(next_filename_price.as_str())
                                        .unwrap();
                                        filename_price = next_filename_price;
                                }
                                // 写入文件
                                if let Err(e) = writeln!(file_price, "{:?}", snapshot) {
                                    eprintln!("Failed to write to file: {}", e);
                                }
                            }
                            Quotation::Depth(depth) => {
                                // 根据时间戳生成日志文件名称
                                let next_filename_depth = generate_filename("Depth", global_timestamp, freq);
                                // 判断文件名称是否一致
                                if filename_depth == next_filename_depth {
                                } else {
                                    file_depth = OpenOptions::new()
                                        .append(true)
                                        .create(true)
                                        .open(next_filename_depth.as_str())
                                        .unwrap();
                                        filename_depth = next_filename_depth;
                                }
                                // 写入文件
                                if let Err(e) = writeln!(file_depth, "{:?}", depth) {
                                    eprintln!("Failed to write to file: {}", e);
                                }
                            }
                            _ => {
                                // 根据时间戳生成日志文件名称
                                let next_filename_others = generate_filename("Others", 0, freq);
                                // 判断文件名称是否一致
                                if filename_others == next_filename_others {
                                } else {
                                    file_others = OpenOptions::new()
                                        .append(true)
                                        .create(true)
                                        .open(next_filename_others.as_str())
                                        .unwrap();
                                        filename_others = next_filename_others;
                                }
                                // 写入文件
                                if let Err(e) = writeln!(file_others, "{:?}", quotation) {
                                    eprintln!("Failed to write to file: {}", e);
                                }
                            }
                        }
                    }

                }
            }
        }
    });

    tokio::time::sleep(tokio::time::Duration::from_secs(10000)).await;
}
