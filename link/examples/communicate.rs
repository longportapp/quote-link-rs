use std::sync::mpsc;
use std::thread::sleep;

use tokio::select;
use tokio::time::Duration;

// 结论:
// 1. 在 tokio 中调用 std::sync::mpsc 中阻塞 recv 会阻塞整个事件循环
// 2. 运行在不同事件循环中的 tokio::sync::mpsc 可以正常被调度 (传递消息)
fn main() {
    tracing_subscriber::fmt::init();

    // 仅当前线程拉起, 测试与其它线程的 channel 通信, 是否会阻塞事件循环
    let rt1 = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // 拉起 1s 的 ticker
    rt1.spawn(async {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            select! {
                now = ticker.tick() => {
                    tracing::info!("runtime 1s ticker received at: {now:?}");
                },
            }
        }
    });

    // 过 3s 再拉
    rt1.block_on(async {
        tokio::time::sleep(Duration::from_secs(3)).await;
    });

    // infinite buffer
    let (tx, rx) = mpsc::channel();

    // 先 sleep 3s 再发送消息出来
    let t1 = std::thread::Builder::new().name(String::from("quic-thread"));
    let job = t1
        .spawn(move || {
            tracing::info!("quic thread running, will sleep");
            sleep(std::time::Duration::from_secs(3));
            tracing::info!("quic thread running, sleep done");
            let handle = std::thread::current();
            tx.send(format!("Hello from thread {}", handle.name().unwrap()))
                .unwrap();
            tracing::info!("quic thread running, send done");
        })
        .unwrap();

    // 在2个不同 thread 中运行的事件循环调度下的通信
    let (tx2, mut rx2) = tokio::sync::mpsc::channel(10);

    // 同事件循环中, 等待阻塞 channel
    rt1.spawn(async move {
        let thread_name = std::thread::current().name().unwrap().to_string();
        let greet = rx.recv().unwrap();
        tracing::info!("runtime loop 1 in thread({thread_name}) rx received with: {greet}");

        tx2.send(format!("Hello from runtime loop 1, its thread name: {thread_name}")).await.unwrap();
    });

    // 在单独的线程中，再拉起个 runtime
    let t2 = std::thread::Builder::new().name(String::from("quic-bridge"));
    _ = t2.spawn(move || {
        let rt2 = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt2.block_on(async  {
            let thread_name = std::thread::current().name().unwrap().to_string();
            let greet = rx2.recv().await.unwrap();
            tracing::info!("runtime thread({thread_name}) rx2 received with: {greet}");
        })
    });

    _ = job.join().unwrap();
    rt1.block_on(async {
        tokio::time::sleep(Duration::from_secs(10)).await;
    })
}
