use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=protos/base.proto");
    println!("cargo:rerun-if-changed=protos/control.proto");
    prost_build::compile_protos(&["control.proto", "base.proto"], &["protos/"])?;
    Ok(())
}
