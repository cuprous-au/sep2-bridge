use clap::Parser;
use git_version::git_version;
use sep2_client::{client::Client, device::SEDevice};
use sep2_common::packages::types::{DeviceCategoryType, PINType};
use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinSet};
use url::Url;

use sep2_bridge::{
    Error, Result, deactivated_broadcast, dispatch,
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
                    Ok(ModbusTransport::Unix(PathBuf::from(url.path())))
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

// Force a relatively quick tickrate for checking on polls. This time has to
// be shorter than any possible poll rate.
const POLL_TICKRATE: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<()> {
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

    // Start the SEP2 connection management task.
    let (sep2_conn_input_tx, sep2_conn_input_rx) = mpsc::channel(10);
    let (sep2_conn_output_tx, sep2_conn_output_rx) = deactivated_broadcast(10);
    join_set.spawn({
        let sep2_conn_input_tx = sep2_conn_input_tx.clone();
        async move {
            sep2_connection::task(
                sep2_conn_output_tx,
                sep2_conn_input_rx,
                sep2_conn_input_tx,
                client,
                sep2_connection::Sep2ConnectionArgs {
                    dcap_uri: args.dcap_uri,
                    max_list_size: args.max_list_size,
                    default_poll_rate: args.default_poll_rate,
                    device_to_register,
                    expected_pin,
                },
            )
            .await
        }
    });

    // Start the scheduler task.
    let (scheduler_input_tx, scheduler_input_rx) = mpsc::channel(10);
    let (scheduler_output_tx, scheduler_output_rx) = deactivated_broadcast(10);
    join_set.spawn(scheduler::task(
        scheduler_output_tx,
        scheduler_input_rx,
        scheduler_input_tx.clone(),
        lfdi,
    ));

    // Start the modbus task.
    let (modbus_input_tx, modbus_input_rx) = mpsc::channel(10);
    let (modbus_output_tx, modbus_output_rx) = deactivated_broadcast(10);
    join_set.spawn(modbus_connection::task(
        modbus_output_tx,
        modbus_input_rx,
        args.modbus_socket,
        args.modbus_device_id,
    ));

    // Dispatch sep2_conn events to the right places.
    join_set.spawn(dispatch::resource_update_dispatcher(
        sep2_conn_output_rx.activate_cloned(),
        scheduler_input_tx.clone(),
    ));

    // Dispatch scheduler events to the right places.
    join_set.spawn(dispatch::sep2_subscription_and_notification_dispatcher(
        scheduler_output_rx.activate_cloned(),
        sep2_conn_input_tx.clone(),
    ));
    join_set.spawn(dispatch::control_change_dispatcher(
        scheduler_output_rx.activate_cloned(),
        modbus_input_tx.clone(),
    ));

    // Dispatch modbus_conn events to the right places.
    join_set.spawn(dispatch::sep2_device_state_dispatcher(
        modbus_output_rx.activate_cloned(),
        sep2_conn_input_tx.clone(),
    ));

    // Wake up the tasks to begin their work.
    sep2_conn_input_tx
        .send(sep2_connection::Command::Wake)
        .await
        .map_err(|_| Error::ChannelClosed)?;
    scheduler_input_tx
        .send(scheduler::Command::NextSchedule)
        .await
        .map_err(|_| Error::ChannelClosed)?;

    // TODO: Better handling of errors that should abort the entire process.
    for result in join_set.join_all().await {
        result?;
    }

    // TODO: Ensure we clean up and persist state before we exit.

    Ok(())
}
