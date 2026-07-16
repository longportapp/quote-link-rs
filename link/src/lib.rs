pub use protos::LinkError;

pub mod client;
pub mod config;
pub mod packet_utils;
pub mod packets;
pub mod server;
pub mod quotation {
    include!(concat!(env!("OUT_DIR"), "/lb.quote.base.rs"));
}

// inner use
mod simple_buf;
mod tls_utils;
mod transport;
mod utils;

pub mod protos {
    include!(concat!(env!("OUT_DIR"), "/control.rs"));
}

pub use packets::{parse_quotation, Quotation};
