# quote-link-rs
行情实时数据 quic 连接接入 sdk

## 如何运行
### 命令行
 * cargo build
 * cd ./link && cargo run --example quotation_record
### systemctl
 * ln -s ./link/.service /etc/systemd/system/quotation_record.service
 * 29 21 * * 1-5 systemctl start quotation_record
 * 5 5 * * 1-6 systemctl stop quotation_record