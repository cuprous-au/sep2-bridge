use async_broadcast::Sender as BroadcastSender;
use derive_more::Display;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use sunspec::{
    client::{AsyncClient, AsyncDevice, Config},
    models::{
        model1::Model1,
        model701::{self, Alrm, ConnSt, Model701},
        model702::{CtrlModes, Model702},
        model703::{self, Model703},
        model713::Model713,
    },
};
use tokio::{
    net::UnixStream,
    sync::{Mutex, mpsc::Receiver as MpscReceiver},
    time::{self, Instant},
};
use tokio_modbus::client::{self, Client, Context};

#[derive(Clone, Debug, Display)]
pub enum Error {
    ConnectionFailed,
    #[display("Modbus server doesn't provide required model {_0}")]
    MissingModel(u16),
    CommunicationTimeout,
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug)]
pub enum Event {
    CapabilitiesPolled(Capabilities),
    StatePolled(Status, Settings),
}

#[derive(Clone, Debug)]
pub enum Command {
    UpdateParameters(Parameters),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameters {
    pub es: Option<model703::Es>,
    pub esvhi: Option<u16>,
    // TODO: Add the remaining parameters required by AS5438
}

// We ensure the loop wakes regularly to make progress on what it needs to do,
// e.g. reestablish a connection or perform a poll.
const LOOP_TIMER_PERIOD: Duration = Duration::from_secs(1);

// Limit all communication to a timeout to ensure we never block the task which
// would cause the input queue to fill up.
// Note: the timeout is enforced at the top-level of the loop. That is, it is a
// timeout on a sequence of queries as a coarse granuality rather than a
// fine-grained timeout for each individual read/write.
const COMM_TIMEOUT: Duration = Duration::from_secs(5);

// The minimum time between polls. This sets the maximum poll rate.
const MIN_POLL_PERIOD: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub enum Transport {
    Tcp(SocketAddr),
    //Rtu
    Unix(PathBuf),
}
type TokioModbusContext = Arc<Mutex<Context>>;

pub async fn task(
    output_ch: BroadcastSender<Event>,
    mut input_ch: MpscReceiver<Command>,
    socket: Transport,
    device_id: u8,
) -> crate::Result<()> {
    let mut device_opt: Option<AsyncDevice<TokioModbusContext>> = None;
    let mut parameters = None;
    let mut last_sent_parameters = None;
    let mut last_poll_time = Instant::now();

    loop {
        match time::timeout(LOOP_TIMER_PERIOD, input_ch.recv()).await {
            Err(_) => {
                // Timeout: wake up and see if there's anything that needs doing.
            }
            Ok(None) => break,
            Ok(Some(command)) => match command {
                Command::UpdateParameters(new_parameters) => {
                    log::trace!("Received updated parameters");
                    parameters = Some(new_parameters);
                }
            },
        }

        // Ensure the device is connected
        if device_opt.is_none() {
            match time::timeout(COMM_TIMEOUT, establish_connection(&socket, device_id)).await {
                Err(_) => {
                    drop_connection(device_opt.take(), Error::CommunicationTimeout).await;
                }
                Ok(Err(err)) => {
                    drop_connection(device_opt.take(), err).await;
                }
                Ok(Ok((new_device, capabilities))) => {
                    device_opt = Some(new_device);
                    log::trace!("Broadcasting CapabilitiesPolled");
                    output_ch
                        .broadcast(Event::CapabilitiesPolled(capabilities))
                        .await
                        .map_err(|_| crate::Error::ChannelClosed)?;
                    // As the device may have been restarted, we reset our last
                    // sent parameters to indicate we don't know what the device
                    // is set to and prompt this task to send the parameters again.
                    last_sent_parameters = None;
                }
            }
        }

        // The jobs we need to do on every loop.
        if let Some(device) = &device_opt
            && parameters != last_sent_parameters
            && let Some(parameters) = &parameters
        {
            match time::timeout(COMM_TIMEOUT, send_new_parameters(device, parameters)).await {
                Err(_) => {
                    drop_connection(device_opt.take(), Error::CommunicationTimeout).await;
                }
                Ok(Err(err)) => {
                    drop_connection(device_opt.take(), err).await;
                }
                Ok(Ok(_)) => {
                    log::trace!("Sent new parameters");
                    last_sent_parameters = Some(parameters.clone());
                }
            }
        }

        // Poll the device again if it is time.
        if (Instant::now().duration_since(last_poll_time) > MIN_POLL_PERIOD)
            && let Some(device) = &device_opt
        {
            log::trace!("Polling device state");
            match time::timeout(COMM_TIMEOUT, poll_device_state(device)).await {
                Err(_) => {
                    drop_connection(device_opt.take(), Error::CommunicationTimeout).await;
                }
                Ok(Err(err)) => {
                    drop_connection(device_opt.take(), err).await;
                }
                Ok(Ok((status, settings))) => {
                    output_ch
                        .broadcast(Event::StatePolled(status, settings))
                        .await
                        .map_err(|_| crate::Error::ChannelClosed)?;
                }
            };
            last_poll_time = Instant::now();
        }
    }

    log::info!("Input channel closed, stopping modbus connection loop");

    Ok(())
}

/// Given a modbus socket target, attempt to connect and probe the device
/// capabilities. On success, returns the modbus device and a capabilities
/// structure.
async fn establish_connection(
    socket: &Transport,
    device_id: u8,
) -> Result<(AsyncDevice<TokioModbusContext>, Capabilities)> {
    let context = match socket {
        Transport::Unix(path) => {
            let stream = UnixStream::connect(path).await.map_err(|err| {
                log::warn!(
                    "Unable to connect to modbus socket at {}: {}",
                    path.display(),
                    err
                );
                Error::ConnectionFailed
            })?;
            client::tcp::attach(stream)
        }
        Transport::Tcp(addr) => client::tcp::connect(*addr).await.map_err(|err| {
            log::warn!("Unable to connect to modbus socket at {}: {}", addr, err);
            Error::ConnectionFailed
        })?, // TODO: RTU over serial
             // TODO: RTU over TCP
    };

    let config = Config::default();
    let client = AsyncClient::new(context, config);
    let device = client.device(device_id).await.map_err(comm_err)?;

    let capabilities = capabilities_query(&device).await?;

    Ok((device, capabilities))
}

/////
// Communication failure handling.
//
fn comm_err<T: std::error::Error>(err: T) -> Error {
    log::debug!("Communications error: {err}");
    Error::ConnectionFailed
}

/// Drop the modbus connection associated with a device. This function
/// deliberately takes the device by value and not reference, with the caller
/// recommended to move the device into this function for it to be dropped, e.g.
/// `drop_connection(device_opt.take(), err)`.
async fn drop_connection(device_opt: Option<AsyncDevice<TokioModbusContext>>, err: Error) {
    log::warn!("Modbus connection issue, dropping connection and retrying: {err}");
    // Gracefully drop the connection just in case.
    if let Some(device) = device_opt {
        let _ = device.client.lock().await.disconnect().await;
    }
}

/////
// Specific queries to different sunspec models.
//

#[derive(Clone, Debug)]
pub struct Capabilities {
    pub w_max_rtg: Option<u16>,
    pub w_ovr_ext_rtg: Option<u16>,
    pub w_ovr_ext_rtg_pf: Option<u16>,
    pub w_und_ext_rtg: Option<u16>,
    pub w_und_ext_rtg_pf: Option<u16>,
    pub va_max_rtg: Option<u16>,
    pub var_max_inj_rtg: Option<u16>,
    pub var_max_abs_rtg: Option<u16>,
    pub w_cha_rte_max_rtg: Option<u16>,
    pub va_cha_rte_max_rtg: Option<u16>,
    pub v_nom_rtg: Option<u16>,
    pub v_max_rtg: Option<u16>,
    pub v_min_rtg: Option<u16>,
    pub ctrl_modes: Option<CtrlModes>,
    pub react_suscept_rtg: Option<u16>,
}

impl From<Model702> for Capabilities {
    fn from(m702: Model702) -> Self {
        Capabilities {
            w_max_rtg: m702.w_max_rtg,
            w_ovr_ext_rtg: m702.w_ovr_ext_rtg,
            w_ovr_ext_rtg_pf: m702.w_ovr_ext_rtg_pf,
            w_und_ext_rtg: m702.w_und_ext_rtg,
            w_und_ext_rtg_pf: m702.w_und_ext_rtg_pf,
            va_max_rtg: m702.va_max_rtg,
            var_max_inj_rtg: m702.var_max_inj_rtg,
            var_max_abs_rtg: m702.var_max_abs_rtg,
            w_cha_rte_max_rtg: m702.w_cha_rte_max_rtg,
            va_cha_rte_max_rtg: m702.va_cha_rte_max_rtg,
            v_nom_rtg: m702.v_nom_rtg,
            v_max_rtg: m702.v_max_rtg,
            v_min_rtg: m702.v_min_rtg,
            ctrl_modes: m702.ctrl_modes,
            react_suscept_rtg: m702.react_suscept_rtg,
        }
    }
}
async fn capabilities_query(device: &AsyncDevice<TokioModbusContext>) -> Result<Capabilities> {
    log::trace!("Query capabilities");

    // Technically the information in model 1 is not needed for the SEP2
    // communication for a DER. However, it is a good sanity check on the
    // modbus connection.
    let m1: Model1 = device.read_model().await.map_err(comm_err)?;

    log::debug!("Model 1 data: {:?}", m1);

    log::debug!(
        "Supported models (and known to us): {}",
        device
            .models
            .supported_model_ids()
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    for required_model in [701, 702, 703, 713] {
        if !device
            .models
            .supported_model_ids()
            .contains(&required_model)
        {
            return Err(Error::MissingModel(required_model));
        }
    }

    let m702: Model702 = device.read_model().await.map_err(comm_err)?;

    Ok(Capabilities::from(m702))
}

#[derive(Clone, Debug)]
pub struct Status {
    pub st: Option<model701::St>,
    pub conn_st: Option<ConnSt>,
    pub alrm: Option<Alrm>,
    pub soc: Option<u16>,
}

impl Status {
    fn from(m701: Model701, m713: Model713) -> Self {
        Status {
            st: m701.st,
            conn_st: m701.conn_st,
            alrm: m701.alrm,
            soc: m713.soc,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub esv_hi: Option<u16>,
    // TODO: Add the remaining settings required by AS5438
}

impl Settings {
    fn from(m703: Model703) -> Self {
        Settings {
            esv_hi: m703.esv_hi,
        }
    }
}

async fn poll_device_state(device: &AsyncDevice<TokioModbusContext>) -> Result<(Status, Settings)> {
    let m701: Model701 = device.read_model().await.map_err(comm_err)?;
    let m703: Model703 = device.read_model().await.map_err(comm_err)?;
    let m713: Model713 = device.read_model().await.map_err(comm_err)?;

    Ok((Status::from(m701, m713), Settings::from(m703)))
}

/////
// Writing values

async fn send_new_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    // TODO: Understand the SEP2 and sunspec meaning of None. Is it:
    // a) value is not provided and should not be communicated, or
    // b) value is null and should be set to null on the other side.
    // Currently this function is going with the interpretation of a) in both directions.
    //
    // Note: if b) ends up being the interpretation, then it might be better to
    // implement a write_model for optimal communication rather than sending
    // each point individually.
    if parameters.es.is_some() {
        device
            .write_point(Model703::ES, parameters.es)
            .await
            .map_err(comm_err)?;
    }
    if parameters.esvhi.is_some() {
        device
            .write_point(Model703::ESV_HI, parameters.esvhi)
            .await
            .map_err(comm_err)?;
    }

    Ok(())
}
