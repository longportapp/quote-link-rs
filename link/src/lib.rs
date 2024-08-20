pub use convert::Quotation;
pub use protos::LinkError;

pub mod client;
pub mod config;
pub mod convert;
pub mod packets;
pub mod quotation {
    include!(concat!(env!("OUT_DIR"), "/lb.quote.base.rs"));
}

mod transport;

mod protos {
    include!(concat!(env!("OUT_DIR"), "/control.rs"));
}
