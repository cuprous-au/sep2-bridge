use std::{error::Error, net::SocketAddr, path::PathBuf};

use clap::Parser;
use git_version::git_version;

/// A bridge service that translates IEEE 2030.5 (SEP2) messages to and from external
/// energy-system protocols and device interfaces.
#[derive(Parser, Debug)]
#[clap(author, about, long_about = None, version=git_version!())]
pub struct Args {
    /// A socket address for connecting to a sep2 server.
    #[clap(env, long, default_value = "127.0.0.1:8080")]
    server_addr: SocketAddr,

    /// A path to the certificate chain we should use when connecting to sep2.
    #[clap(env, long, default_value = "/tmp")]
    cert_path: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    env_logger::builder().format_timestamp_millis().init();

    log::info!("sep2-bridge started. Connecting to {}.", args.server_addr);

    Ok(())
}
