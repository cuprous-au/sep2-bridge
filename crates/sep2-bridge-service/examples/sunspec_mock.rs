#[path = "../tests/modbus_server_mock/mod.rs"]
mod modbus_server_mock;

use std::net::SocketAddr;

use clap::Parser;
use modbus_server_mock::SunSpecMock;

/// Runs the SunSpec modbus mock used by the integration tests as a standalone server.
#[derive(Parser)]
struct Args {
    /// Address to listen on.
    #[clap(long, default_value = "127.0.0.1:5020")]
    addr: SocketAddr,

    /// Only expose these SunSpec models, comma separated. Defaults to all of them.
    #[clap(long, value_delimiter = ',')]
    models: Option<Vec<u32>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut mock = SunSpecMock::new(args.models.as_deref()).await?;
    mock.addr = Some(args.addr);
    mock.start().await?;
    tokio::signal::ctrl_c().await?;
    mock.stop().await;
    Ok(())
}
