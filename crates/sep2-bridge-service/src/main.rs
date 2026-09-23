use clap::Parser;
use git_version::git_version;
use sep2_client::{client::Client, device::SEDevice};
use sep2_common::{
    Pen,
    packages::types::{DeviceCategoryType, PINType},
};
use std::{
    collections::HashMap,
    fs,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};
use tokio::{
    signal::unix::{self, SignalKind},
    sync::mpsc,
    task::JoinSet,
};
use url::Url;

use sep2_bridge::{
    Error, Result, deactivated_broadcast, dispatch, metrics,
    modbus_connection::{self, Transport as ModbusTransport},
    scheduler, sep2_connection,
};

/// A bridge service that translates IEEE 2030.5 (SEP2) messages to and from external
/// energy-system protocols and device interfaces.
#[derive(Parser, Debug)]
#[clap(author, about, long_about = None, version=git_version!())]
pub struct Args {
    /// A path to the CA certificate we should use to validate the sep2 server's
    /// certificate.
    #[clap(env, long, value_parser = validate_file_exists, default_value = "/etc/sep2-bridge/ca.crt")]
    ca_path: PathBuf,

    /// A path to a directory of credentials, including the client certificate
    /// at `{credentials_directory}/client.crt` the client key at
    /// `{credentials_directory}/client.key` and optionally a PIN at
    /// `{credentials_path}/registration_pin`.
    #[clap(env, long, value_parser = validate_path_exists)]
    credentials_directory: PathBuf,

    /// The location of the dcap entrypoint URI.
    #[clap(env, long, default_value = "/dcap")]
    dcap_uri: String,

    /// An address for connecting to a sep2 server.
    #[clap(env, long, default_value = "127.0.0.1:8080")]
    server_addr: String,

    /// The maximum number of elements to query for in a list. If more elements
    /// are present, errors will be emitted and the service will continue as
    /// best it is able to but may not poll all items it should. Note that SEP2
    /// requires storing at least 24 DERControls so this list size should be
    /// set larger.
    #[clap(env, long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
    max_list_size: u32,

    /// The poll rate to use if the server does not specify a poll rate. It is
    /// useful to modify this during testing.
    #[clap(env, long, default_value_t = 900, value_parser = clap::value_parser!(u32).range(1..))]
    default_poll_rate: u32,

    /// The device id for the modbus connection to distinguish between other devices.
    #[clap(env, long, default_value_t = 1)]
    modbus_device_id: u8,

    /// The address to use for the modbus connection. A scheme prefix is
    /// required, one of unix://, tcp://, ...
    #[clap(env, long, value_parser = parse_modbus_socket)]
    modbus_socket: ModbusTransport,

    /// The PEN (Private Enterprise Number) used to make mRIDs unique.
    #[clap(env, long, default_value = "0", value_parser = parse_pen)]
    pen: Pen,

    /// The optional unix socket endpoint to regularly send metrics to. A form
    /// unix:///path/to/socket is required.
    #[clap(env, long, value_parser = parse_metrics_url)]
    metrics_url: Option<PathBuf>,

    /// How often to send metrics to the metrics endpoint.
    #[clap(env, long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    metrics_interval_sec: u64,

    /// A path to persist and restore scheduler state. Pass an empty string to disable persistence.
    #[clap(env, long, default_value = "", value_parser = parse_persistence_path)]
    // Note the full path std::option::Option is to prevent clap giving the Option special treatment.
    persistence_path: std::option::Option<PathBuf>,
}

fn validate_path_exists(input: &str) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(input);
    if !path.exists() {
        Err(format!("Cannot find path '{}'.", input))
    } else {
        Ok(path)
    }
}

fn validate_file_exists(input: &str) -> std::result::Result<PathBuf, String> {
    let path = validate_path_exists(input)?;
    if !path.is_file() {
        Err(format!("Path '{}' is not a file.", input))
    } else {
        Ok(path)
    }
}

fn parse_modbus_socket(value: &str) -> std::result::Result<ModbusTransport, String> {
    match Url::parse(value) {
        Err(_) => Err(String::from("Unable to parse URL.")),
        Ok(url) => match url.scheme() {
            "unix" => {
                if url.username() != ""
                    || url.password().is_some()
                    || url.host_str().is_some()
                    || url.port().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    Err(String::from("Unexpected parts of URL present."))
                } else {
                    Ok(ModbusTransport::Unix(
                        url.to_file_path()
                            .map_err(|_| "Unable to extract file path from URL")?,
                    ))
                }
            }
            "tcp" => {
                if url.username() != ""
                    || url.password().is_some()
                    || !(url.path().is_empty() || url.path() == "/")
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    Err(String::from("Unexpected parts of URL present."))
                } else {
                    // Note: the url parsing for the tcp scheme leaves an IP as
                    // a domain in contrast to the http scheme.
                    let ip = match url.domain() {
                        None => Err(String::from("Missing host"))?,
                        Some(host) => host
                            .parse::<IpAddr>()
                            .map_err(|_| String::from("Host is not an IP address"))?,
                    };

                    Ok(ModbusTransport::Tcp(SocketAddr::new(
                        ip,
                        url.port().unwrap_or(502),
                    )))
                }
            }
            scheme => Err(format!("Unknown modbus socket scheme '{scheme}'")),
        },
    }
}

fn load_pin(credentials_path: PathBuf) -> Result<Option<PINType>> {
    let file_path = credentials_path.join("registration_pin");

    if !file_path.exists() {
        return Ok(None);
    }
    if !file_path.is_file() {
        return Err(Error::InvalidInput(format!(
            "'{}' is not a file",
            file_path.to_string_lossy()
        )));
    }

    let contents = fs::read_to_string(&file_path).map_err(|err| {
        Error::InvalidInput(format!(
            "Error reading from '{}': {}",
            file_path.to_string_lossy(),
            err
        ))
    })?;

    let number = contents
        .trim()
        .parse::<u32>()
        .map_err(|_| Error::InvalidInput(String::from("Could not parse PIN")))?;

    Some(PINType::new(number).ok_or(Error::InvalidInput(String::from(
        "PIN is a number but not of the right size",
    ))))
    .transpose()
}

fn parse_metrics_url(value: &str) -> std::result::Result<PathBuf, String> {
    match Url::parse(value) {
        Err(_) => Err(String::from("Unable to parse URL.")),
        Ok(url) => match url.scheme() {
            "unix" => {
                if url.username() != ""
                    || url.password().is_some()
                    || url.fragment().is_some()
                    || url.host_str().is_some()
                    || url.port().is_some()
                    || url.query().is_some()
                {
                    Err(String::from("Unexpected parts of URL present."))
                } else {
                    Ok(url
                        .to_file_path()
                        .map_err(|_| "Unable to extract file path from URL")?)
                }
            }
            scheme => Err(format!("Metrics endpoint scheme {scheme} not supported.")),
        },
    }
}

fn parse_pen(value: &str) -> std::result::Result<Pen, String> {
    value
        .parse::<u32>()
        .map_err(|_| "PEN is not a valid u32".into())
        .and_then(|val| {
            Pen::csipaus(val).ok_or("PEN is not the right size for CSIP-AUS format".into())
        })
}

fn parse_persistence_path(value: &str) -> std::result::Result<Option<PathBuf>, String> {
    if value.is_empty() {
        return Ok(None);
    }

    let path = PathBuf::from(value);
    if path.is_dir() {
        return Err(format!(
            "The path '{}' is a directory, expected a file instead.",
            path.display()
        ));
    }

    // We aren't going to create directories, so first ensure the parent exists.
    let parent = path
        .parent()
        .ok_or(format!("Path has no parent: '{}'", path.display()))?;
    // A relative filename without any path has the current directory as its
    // parent, which is represented by "".
    if !parent.is_empty() && !parent.exists() {
        return Err(format!(
            "The path '{}' should be a file inside a directory that already exists.",
            path.display()
        ));
    }
    Ok(Some(path))
}

// Force a relatively quick tickrate for checking on polls. This time has to
// be shorter than any possible poll rate.
const POLL_TICKRATE: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let args = Args::parse();

    env_logger::builder().format_timestamp_millis().init();

    log::info!("sep2-bridge started. Connecting to {}.", args.server_addr);

    let server_addr = if args.server_addr.starts_with("https://") {
        args.server_addr
    } else {
        format!("https://{}", args.server_addr)
    };
    let cert_path = validate_file_exists(
        &args
            .credentials_directory
            .join("client.crt")
            .to_string_lossy(),
    )
    .map_err(|_| Error::InvalidInput(String::from("Client certificate not present")))?;
    let key_path = validate_file_exists(
        &args
            .credentials_directory
            .join("client.key")
            .to_string_lossy(),
    )
    .map_err(|_| Error::InvalidInput(String::from("Client key not present")))?;
    let client = Client::new_https(
        &server_addr,
        &cert_path,
        &key_path,
        &args.ca_path,
        None,
        Some(POLL_TICKRATE),
    )
    .expect("Could not create client");

    let device_to_register = SEDevice::new_from_cert(&cert_path, DeviceCategoryType::all()).or(
        // Fatal error - we can't continue.
        Err(Error::InvalidInput(format!(
            "Device could not be loaded from certificate at {}",
            cert_path.display()
        ))),
    )?;

    let lfdi = device_to_register.lfdi;
    let sfdi = device_to_register.sfdi;
    log::debug!("Our device LFDI: {}, SFDI: {}", lfdi, sfdi);

    // If the user provided a credentials path then we expect a PIN to be present.
    let expected_pin = load_pin(args.credentials_directory)?;

    let mut join_set = JoinSet::new();
    let mut task_names = HashMap::new();

    // Start the metrics task if a metrics endpoint has been provided.
    if let Some(url) = args.metrics_url {
        let registry = metrics::initialise();
        let handle = join_set.spawn(metrics::task(
            registry,
            Duration::from_secs(args.metrics_interval_sec),
            url,
        ));
        task_names.insert(handle.id(), "metrics_push");
    }

    // Start the SEP2 connection management task.
    let (sep2_conn_input_tx, sep2_conn_input_rx) = mpsc::channel(10);
    let (sep2_conn_output_tx, sep2_conn_output_rx) = deactivated_broadcast(10);
    let handle = join_set.spawn({
        let sep2_conn_input_tx = sep2_conn_input_tx.clone();
        async move {
            sep2_connection::task(
                sep2_conn_output_tx,
                sep2_conn_input_rx,
                sep2_conn_input_tx,
                sep2_connection::Sep2ConnectionArgs {
                    client,
                    dcap_uri: args.dcap_uri,
                    max_list_size: args.max_list_size,
                    default_poll_rate: args.default_poll_rate,
                    device_to_register,
                    expected_pin,
                    pen: args.pen,
                },
            )
            .await
        }
    });
    task_names.insert(handle.id(), "sep2_connection");

    // Start the scheduler task.
    let (scheduler_input_tx, scheduler_input_rx) = mpsc::channel(10);
    let (scheduler_output_tx, scheduler_output_rx) = deactivated_broadcast(10);
    let handle = join_set.spawn(scheduler::task(
        scheduler_output_tx,
        scheduler_input_rx,
        scheduler_input_tx.clone(),
        lfdi,
        args.persistence_path,
    ));
    task_names.insert(handle.id(), "scheduler");

    // Start the modbus task.
    let (modbus_input_tx, modbus_input_rx) = mpsc::channel(10);
    let (modbus_output_tx, modbus_output_rx) = deactivated_broadcast(10);
    let handle = join_set.spawn(modbus_connection::task(
        modbus_output_tx,
        modbus_input_rx,
        args.modbus_socket,
        args.modbus_device_id,
    ));
    task_names.insert(handle.id(), "modbus_connection");

    // Dispatch sep2_conn events to the right places.
    let handle = join_set.spawn(dispatch::resource_update_dispatcher(
        sep2_conn_output_rx.activate_cloned(),
        scheduler_input_tx.clone(),
    ));
    task_names.insert(handle.id(), "resource_update_dispatcher");

    // Dispatch scheduler events to the right places.
    let handle = join_set.spawn(dispatch::sep2_subscription_and_notification_dispatcher(
        scheduler_output_rx.activate_cloned(),
        sep2_conn_input_tx.clone(),
    ));
    task_names.insert(handle.id(), "sep2_subscription_and_notification_dispatcher");
    let handle = join_set.spawn(dispatch::control_change_dispatcher(
        scheduler_output_rx.activate_cloned(),
        modbus_input_tx.clone(),
    ));
    task_names.insert(handle.id(), "control_change_dispatcher");

    // Dispatch modbus_conn events to the right places.
    let handle = join_set.spawn(dispatch::sep2_device_state_dispatcher(
        modbus_output_rx.activate_cloned(),
        sep2_conn_input_tx.clone(),
    ));
    task_names.insert(handle.id(), "sep2_device_state_dispatcher");

    // Install signal handlers.
    let handle = join_set.spawn(signals_handler());
    task_names.insert(handle.id(), "signals_handler");

    // No more tasks to be created. Remove mutability on task_names.
    let task_names = task_names;

    // Wake up the tasks to begin their work.
    sep2_conn_input_tx
        .send(sep2_connection::Command::Wake)
        .await
        .map_err(|_| Error::ChannelClosed)?;
    // The wake-up action for the scheduler is to emit all known resources.
    scheduler_input_tx
        .send(scheduler::Command::RefreshActiveResources)
        .await
        .map_err(|_| Error::ChannelClosed)?;

    // Await the first task to fail.
    // Note: if the main task itself panics, the tokio runtime will clean up all
    // tasks itself.
    let result = join_set
        .join_next_with_id()
        .await
        .expect("The join set should never be empty");

    let id = match &result {
        Ok((id, _)) => *id,
        Err(join_err) => join_err.id(),
    };
    let task_name = task_names.get(&id).unwrap_or(&"unknown");
    let exit_code = match result {
        Err(join_err) => {
            log::error!(
                "The task {task_name} failed with a tokio join error: {join_err}. Stopping the process."
            );
            ExitCode::FAILURE
        }
        // Special case: graceful exit when a signal is received.
        Ok((_, Err(Error::SignalReceived(signal)))) => {
            log::info!("Exiting because a {signal} signal was received.");
            ExitCode::SUCCESS
        }
        Ok((_, Err(err))) => {
            log::error!("The task {task_name} errored: {err}. Stopping the process.");
            ExitCode::FAILURE
        }
        Ok((_, Ok(_))) => {
            log::error!(
                "The task {task_name} finished without an explicit error, but no tasks are expected to finish. This is an unexpected state, stopping the process."
            );
            ExitCode::FAILURE
        }
    };

    // Abort and wait on all tasks. This is not strictly necessary (tokio will
    // abort all tasks when main ends) but it allows us to manage the clean up.
    log::info!("Aborting all tasks.");
    join_set.abort_all();
    while join_set.join_next().await.is_some() {}

    log::info!("Main task stopping.");

    Ok(exit_code)
}

async fn signals_handler() -> Result<()> {
    let sigint = async {
        let mut signal =
            unix::signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");
        signal.recv().await;
    };

    let terminate = async {
        let mut signal =
            unix::signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        signal.recv().await;
    };

    tokio::select! {
        _ = sigint => Err(Error::SignalReceived("SIGINT")),
        _ = terminate => Err(Error::SignalReceived("SIGTERM")),
    }
}
