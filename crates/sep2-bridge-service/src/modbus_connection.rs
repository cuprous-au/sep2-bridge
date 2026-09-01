use async_broadcast::Sender as BroadcastSender;
use derive_more::Display;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use sunspec::{
    FixedSize, Group, Model, Point, Value,
    client::{AsyncClient, AsyncDevice, AsyncModbusClient, Config},
    models::{
        model1::Model1,
        model701::{self, Alrm, ConnSt, Model701},
        model702::{CtrlModes, Model702},
        model703::{self, Model703},
        model704::{self, Model704},
        model711::{self, Model711},
        model713::Model713,
    },
};
use tokio::{
    net::UnixStream,
    sync::{Mutex, mpsc::Receiver as MpscReceiver},
    time::{self, Instant},
};
use tokio_modbus::client::{self, Client, Context};

use crate::{ScaledValue, ScaledValueInner};

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
    // Model1 doesn't support Clone, so wrap it with an Arc to pass around a reference.
    DeviceConnected(Arc<Model1>),
    CapabilitiesPolled(Capabilities),
    StatePolled(Option<Status>, Option<Settings>, Option<Metering>),
}

#[derive(Clone, Debug)]
pub enum Command {
    UpdateParameters(Parameters),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parameters {
    // AS5438 - Table E.9, Section E.4.7
    pub droop_ctl: Option<Model711Ctl>,

    // AS5438 - Table E.10, Section E.4.8
    pub es: Option<model703::Es>,
    pub esv_hi: Option<ScaledValue<u16>>,
    pub esv_lo: Option<ScaledValue<u16>>,
    pub es_hz_hi: Option<ScaledValue<u32>>,
    pub es_hz_lo: Option<ScaledValue<u32>>,
    pub es_dly_tms: Option<u32>,
    pub es_rnd_tms: Option<u32>,
    pub es_rmp_tms: Option<u32>,

    // AS5438 - Table 11, Section E.4.9
    pub w_max_lim_pct_ena: Option<model704::WMaxLimPctEna>,
    pub w_max_lim_pct: Option<ScaledValue<u16>>,

    // AS5438 - Table 12, Section E.4.10
    pub w_set_ena: Option<model704::WSetEna>,
    pub w_set_pct: Option<ScaledValue<i16>>,
    // TODO: Add the remaining parameters required by AS5438
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model711Ctl {
    pub db_of: ScaledValue<u32>,
    pub db_uf: ScaledValue<u32>,
    pub k_of: ScaledValue<u16>,
    pub k_uf: ScaledValue<u16>,
    pub rsp_tms: ScaledValue<u32>,
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
                Ok(Ok((new_device, model1, capabilities))) => {
                    device_opt = Some(new_device);
                    output_ch
                        .broadcast(Event::DeviceConnected(Arc::new(model1)))
                        .await
                        .map_err(|_| crate::Error::ChannelClosed)?;
                    if let Some(capabilities) = capabilities {
                        log::trace!("Broadcasting CapabilitiesPolled");
                        output_ch
                            .broadcast(Event::CapabilitiesPolled(capabilities))
                            .await
                            .map_err(|_| crate::Error::ChannelClosed)?;
                    }
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
                    log::trace!("Sent new parameters: {:?}", parameters);
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
                Ok(Ok((status, settings, metering))) => {
                    output_ch
                        .broadcast(Event::StatePolled(status, settings, metering))
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
/// capabilities. On success, returns the modbus device, the device info from
/// model 1, and an optional capabilities structure if the probe was successful.
async fn establish_connection(
    socket: &Transport,
    device_id: u8,
) -> Result<(
    AsyncDevice<TokioModbusContext>,
    Model1,
    Option<Capabilities>,
)> {
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

    for required_model in [701, 702, 703] {
        if !device
            .models
            .supported_model_ids()
            .contains(&required_model)
        {
            // Don't error but do warn if it is missing.
            log::warn!(
                "Model {required_model} is missing from device. This will impact communication with the SEP2 server."
            );
        }
    }

    let capabilities = capabilities_query(&device).await?;

    Ok((device, m1, capabilities))
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

/// The read_model method doesn't check if the model is supported. This function
/// wraps read_model with a check and returns None if the model isn't supported
/// by the device.
async fn read_model_safe<M: Model>(device: &AsyncDevice<TokioModbusContext>) -> Result<Option<M>> {
    if !device.models.supported_model_ids().contains(&M::ID) {
        return Ok(None);
    }

    Ok(Some(device.read_model().await.map_err(comm_err)?))
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
async fn capabilities_query(
    device: &AsyncDevice<TokioModbusContext>,
) -> Result<Option<Capabilities>> {
    log::trace!("Query capabilities");

    read_model_safe::<Model702>(device)
        .await
        .map(|opt| opt.map(Capabilities::from))
}

#[derive(Clone, Debug)]
pub struct Status {
    pub st: Option<model701::St>,
    pub conn_st: Option<ConnSt>,
    pub alrm: Option<Alrm>,
    pub soc: Option<ScaledValue<u16>>,
}

impl Status {
    fn from(m701: &Option<Model701>, m713: &Option<Model713>) -> Option<Self> {
        if m701.is_none() && m713.is_none() {
            return None;
        }

        let m701 = m701.as_ref();
        let m713 = m713.as_ref();
        Some(Status {
            st: m701.and_then(|m701| m701.st),
            conn_st: m701.and_then(|m701| m701.conn_st),
            alrm: m701.and_then(|m701| m701.alrm),
            // Extract SoC with scale factor
            soc: m713.and_then(|m713| {
                m713.soc
                    .map(|soc| ScaledValue::new(soc, m713.pct_sf.unwrap_or_default()))
            }),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub esv_hi: Option<ScaledValue<u16>>,
    // TODO: Add the remaining settings required by AS5438
}

impl Settings {
    fn from(m703: &Option<Model703>) -> Option<Self> {
        m703.as_ref().map(|m703| Settings {
            // Extract the voltage with its scale factor.
            esv_hi: m703
                .esv_hi
                .map(|esv_hi| ScaledValue::new(esv_hi, m703.v_sf.unwrap_or_default())),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct Metering {
    pub w: Option<i16>,
    pub wl1: Option<i16>,
    pub wl2: Option<i16>,
    pub wl3: Option<i16>,
    pub w_sf: Option<i16>,
    pub var: Option<i16>,
    pub var_sf: Option<i16>,
    pub voltages: Vec<VoltageWithReference>,
    pub v_sf: Option<i16>,
    pub hz: Option<u32>,
    pub hz_sf: Option<i16>,
}

#[derive(Clone, Debug)]
pub struct VoltageWithReference(pub u16, pub PhaseReference);

#[derive(Copy, Clone, Debug, Display)]
pub enum PhaseReference {
    LLV,
    LNV,
    VL1L2,
    VL1,
    VL2L3,
    VL2,
    VL3L1,
    VL3,
}

impl Metering {
    fn from(m701: &Option<Model701>) -> Option<Self> {
        match m701.as_ref() {
            None => None,
            Some(m701) => {
                // Collecting up all voltages into a vector rather than hardcoding them.
                let voltages = [
                    m701.llv
                        .map(|v| VoltageWithReference(v, PhaseReference::LLV)),
                    m701.lnv
                        .map(|v| VoltageWithReference(v, PhaseReference::LNV)),
                    m701.vl1l2
                        .map(|v| VoltageWithReference(v, PhaseReference::VL1L2)),
                    m701.vl1
                        .map(|v| VoltageWithReference(v, PhaseReference::VL1)),
                    m701.vl2l3
                        .map(|v| VoltageWithReference(v, PhaseReference::VL2L3)),
                    m701.vl2
                        .map(|v| VoltageWithReference(v, PhaseReference::VL2)),
                    m701.vl3l1
                        .map(|v| VoltageWithReference(v, PhaseReference::VL3L1)),
                    m701.vl3
                        .map(|v| VoltageWithReference(v, PhaseReference::VL3)),
                ]
                .into_iter()
                .flatten()
                .collect();
                Some(Metering {
                    w: m701.w,
                    wl1: m701.wl1,
                    wl2: m701.wl2,
                    wl3: m701.wl3,
                    w_sf: m701.w_sf,
                    var: m701.var,
                    var_sf: m701.var_sf,
                    voltages,
                    v_sf: m701.v_sf,
                    hz: m701.hz,
                    hz_sf: m701.hz_sf,
                })
            }
        }
    }
}

async fn poll_device_state(
    device: &AsyncDevice<TokioModbusContext>,
) -> Result<(Option<Status>, Option<Settings>, Option<Metering>)> {
    // Always poll model 1. This acts as a health check regardless of what other
    // models the device supports. For example a device might only expose models
    // to control it but not provide any feedback. Likely this is not compliant
    // but it is simple to support and also useful in development.
    let _m1: Model1 = device.read_model().await.map_err(comm_err)?;
    let m701 = read_model_safe::<Model701>(device).await?;
    let m703 = read_model_safe::<Model703>(device).await?;
    let m713 = read_model_safe::<Model713>(device).await?;

    Ok((
        Status::from(&m701, &m713),
        Settings::from(&m703),
        Metering::from(&m701),
    ))
}

/////
// Writing values

async fn send_new_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    // This is an ugly utility function to make the rest of the parameters less ugly.
    // The main point is that we only send the value if it is Some.
    async fn write_if_some<T: FixedSize, M: Model>(
        device: &AsyncDevice<TokioModbusContext>,
        p: Point<M, Option<T>>,
        value: Option<T>,
    ) -> Result<()> {
        match value {
            None => Ok(()),
            Some(value) => device.write_point(p, Some(value)).await.map_err(comm_err),
        }
    }
    // As above but with a ScaledValue and a target scale factor.
    async fn write_rescaled_if_some<T: FixedSize + ScaledValueInner, M: Model>(
        device: &AsyncDevice<TokioModbusContext>,
        p: Point<M, Option<T>>,
        value: Option<ScaledValue<T>>,
        scale_factor: i16,
    ) -> Result<()> {
        write_if_some(
            device,
            p,
            value.map(|inner| inner.rescale(scale_factor).value),
        )
        .await
    }

    // AS5438 - Table E.9, Section E.4.7
    if device.models.supported_model_ids().contains(&711)
        && let Some(droop_ctl) = parameters.droop_ctl.as_ref()
    {
        // As per the modbus spec, the first control is readonly and represents
        // the current state. Make sure the device allows at least one other
        // control before continuing.
        let n_ctl = device.read_point(Model711::N_CTL).await.map_err(comm_err)?;
        if n_ctl >= 2 {
            // We write into the second Ctl group.
            let offset = Model711::addr(&device.models).addr + Model711::LEN + model711::Ctl::LEN;

            let db_sf = device.read_point(Model711::DB_SF).await.map_err(comm_err)?;
            let k_sf = device.read_point(Model711::K_SF).await.map_err(comm_err)?;
            let rsp_tms_sf = device
                .read_point(Model711::RSP_TMS_SF)
                .await
                .map_err(comm_err)?;
            // And assign manually

            // FIXME: It would be nice to write all of these registers in one
            // call. However, we can't write the read_only register itself
            // so it's not as trivial as encoding the entire struct.
            write_offset_point(
                device,
                offset,
                model711::Ctl::DB_OF,
                droop_ctl.db_of.rescale(db_sf).value,
            )
            .await?;
            write_offset_point(
                device,
                offset,
                model711::Ctl::DB_UF,
                droop_ctl.db_uf.rescale(db_sf).value,
            )
            .await?;
            write_offset_point(
                device,
                offset,
                model711::Ctl::K_OF,
                droop_ctl.k_of.rescale(k_sf).value,
            )
            .await?;
            write_offset_point(
                device,
                offset,
                model711::Ctl::K_UF,
                droop_ctl.k_uf.rescale(k_sf).value,
            )
            .await?;
            write_offset_point(
                device,
                offset,
                model711::Ctl::RSP_TMS,
                droop_ctl.rsp_tms.rescale(rsp_tms_sf).value,
            )
            .await?;
        }
    }

    // AS5438 - Table E.10, Section E.4.8
    if device.models.supported_model_ids().contains(&703) {
        write_if_some(device, Model703::ES, parameters.es).await?;
        if parameters.esv_hi.is_some() || parameters.esv_lo.is_some() {
            let v_sf = device
                .read_point(Model703::V_SF)
                .await
                .map_err(comm_err)?
                .unwrap_or_default();
            write_rescaled_if_some(device, Model703::ESV_HI, parameters.esv_hi, v_sf).await?;
            write_rescaled_if_some(device, Model703::ESV_LO, parameters.esv_lo, v_sf).await?;
        }
        if parameters.es_hz_hi.is_some() || parameters.es_hz_lo.is_some() {
            let hz_sf = device
                .read_point(Model703::HZ_SF)
                .await
                .map_err(comm_err)?
                .unwrap_or_default();
            write_rescaled_if_some(device, Model703::ES_HZ_HI, parameters.es_hz_hi, hz_sf).await?;
            write_rescaled_if_some(device, Model703::ES_HZ_LO, parameters.es_hz_lo, hz_sf).await?;
        }
        write_if_some(device, Model703::ES_DLY_TMS, parameters.es_dly_tms).await?;
        write_if_some(device, Model703::ES_RND_TMS, parameters.es_rnd_tms).await?;
        write_if_some(device, Model703::ES_RMP_TMS, parameters.es_rmp_tms).await?;
    }

    if device.models.supported_model_ids().contains(&704) {
        // AS5438 - Table E.11, Section E.4.9
        write_if_some(
            device,
            Model704::W_MAX_LIM_PCT_ENA,
            parameters.w_max_lim_pct_ena,
        )
        .await?;
        if parameters.w_max_lim_pct.is_some() {
            let pct_sf = device
                .read_point(Model704::W_MAX_LIM_PCT_SF)
                .await
                .map_err(comm_err)?
                .unwrap_or_default();
            write_rescaled_if_some(
                device,
                Model704::W_MAX_LIM_PCT,
                parameters.w_max_lim_pct,
                pct_sf,
            )
            .await?;
        }

        // AS5438 - Table E.12, Section E.4.10
        write_if_some(device, Model704::W_SET_ENA, parameters.w_set_ena).await?;
        if parameters.w_set_pct.is_some() {
            let pct_sf = device
                .read_point(Model704::W_SET_PCT_SF)
                .await
                .map_err(comm_err)?
                .unwrap_or_default();
            write_rescaled_if_some(device, Model704::W_SET_PCT, parameters.w_set_pct, pct_sf)
                .await?;
        }
    }

    Ok(())
}

/// A convenience tool for writing points that are part of repeating groups.
/// Requires the absolute register address for the start of the group and a
/// point within the group.
async fn write_offset_point<G: Group, T: Value>(
    device: &AsyncDevice<TokioModbusContext>,
    offset: u16,
    p: Point<G, T>,
    value: T,
) -> Result<()> {
    let addr = offset + p.offset;
    let words = value.encode();
    device
        .client
        .write_registers(device.slave_id, addr, &words)
        .await
        .map_err(comm_err)
}
