use std::io;

fn main() -> io::Result<()> {
    prost_build::Config::new().compile_protos(&["proto/vpn.proto"], &["proto"])?;
    Ok(())
}
