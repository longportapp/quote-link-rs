use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=protos/quote_base.proto");
    prost_build::compile_protos(&["control.proto", "base.proto"], &["protos/"])?;
    Ok(())
}
