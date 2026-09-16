use git_version::git_version;
use prometheus_client::{encoding::text::encode, metrics::counter::Counter, registry::Registry};
use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

use crate::Result;

/// Provides all metrics available to modules.
pub struct Metrics {
    pub modbus_connection_drops: Counter,
    pub sep2_heartbeat_failures: Counter,
}

// Global value for all users to obtain through the metrics() function.
static METRICS: OnceLock<Metrics> = OnceLock::new();

/// Obtains the metrics structure if available. For metrics to be available, the
/// `initialise()` function has to have been called first.
pub fn metrics() -> Option<&'static Metrics> {
    METRICS.get()
}

/// Initialises the prometheus metrics, returning a registry which the caller
/// can use to respond to pulls or create pushes. This function must be called
/// before inserting metrics.
pub fn initialise() -> Registry {
    let metrics = Metrics {
        modbus_connection_drops: Counter::default(),
        sep2_heartbeat_failures: Counter::default(),
    };

    let mut registry = Registry::with_prefix("sep2_bridge");

    registry.register(
        "modbus_connection_drops",
        "The number of times an active modbus connection was dropped",
        metrics.modbus_connection_drops.clone(),
    );
    registry.register(
        "sep2_heartbeat_failures",
        "The number of failures encountered when polling the DeviceCapability endpoint of the SEP2 connection",
        metrics.sep2_heartbeat_failures.clone(),
    );

    METRICS
        .set(metrics)
        .unwrap_or_else(|_| panic!("Initialise has been called more than once"));

    log::debug!("Initialised metrics registry");

    registry
}

/// The metrics task that will take a registry and repeatedly push the metrics to
/// the given endpoint.
pub async fn task(registry: Registry, interval: Duration, unix_path: PathBuf) -> Result<()> {
    loop {
        let buffer = {
            let mut buffer = String::new();
            match encode(&mut buffer, &registry) {
                Err(err) => {
                    log::error!("Problem encoding registry to push metrics: {err}");
                    None
                }
                Ok(()) => Some(buffer),
            }
        };

        if let Some(buffer) = buffer {
            log::debug!("Sending metrics to {}", unix_path.display());
            let timeout = Duration::from_secs(5);
            match tokio::time::timeout(timeout, push_metrics(&unix_path, &buffer)).await {
                Err(_) => log::error!("Timed out sending metrics to {}", unix_path.display()),
                Ok(Err(err)) => log::error!("Problem writing metrics: {err}"),
                Ok(Ok(_)) => {}
            }
        }

        tokio::time::sleep(interval).await;
    }
}

async fn push_metrics(unix_path: &Path, body: &str) -> io::Result<()> {
    let header = format!(
        "POST /metrics/job/sep2_bridge HTTP/1.1\r\n\
Host: localhost\r\n\
User-Agent: sep2-bridge/{}\r\n\
Accept: */*\r\n\
Content-Length: {}\r\n\
Content-Type: application/openmetrics-text; version=1.0.0; charset=utf-8\r\n\
Connection: close\r\n\
\r\n",
        git_version!(),
        body.len()
    );

    let mut stream = UnixStream::connect(unix_path).await?;
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;

    // Check the response for the status code. If it is not a 2xx then report it
    // as an unknown error.
    let response = String::from_utf8_lossy(&response);
    let status_line = response.lines().next().unwrap_or_default();
    let status_code = status_line.split(' ').nth(1);
    if let Some(status_code) = status_code {
        if !status_code.starts_with("2") {
            log::warn!("Received unexpected response for metrics push: {status_code}");
        }
    } else {
        log::warn!("No valid HTTP response given for metrics push.");
    }

    Ok(())
}
