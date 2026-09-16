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
        model705::{self, Model705},
        model706::{self, Model706},
        model707::{self, Model707},
        model708::{self, Model708},
        model709::{self, Model709},
        model710::{self, Model710},
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

use crate::{ScaledValue, ScaledValueInner, metrics};

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
    // AS5438 - Table E.3, Section E.4.1
    pub pfw_inj_ena: Option<model704::PfwInjEna>,
    pub pfw_inj_pf: Option<ScaledValue<u16>>,
    pub pfw_inj_ext: Option<model704::PfwInjExt>,
    pub pfw_abs_ena: Option<model704::PfwAbsEna>,
    pub pfw_abs_pf: Option<ScaledValue<u16>>,
    pub pfw_abs_ext: Option<model704::PfwAbsExt>,

    // AS5438 - Table E.4, Section E.4.2
    pub vref: Option<ScaledValue<u16>>,
    pub vref_auto_ena: Option<model705::CrvVRefAutoEna>,
    pub vref_auto_tms: Option<u16>,
    pub der_volt_var: Option<Curve<u16, i16>>,
    pub der_volt_var_tms: Option<ScaledValue<u32>>,
    pub der_volt_var_dept_ref: Option<model705::CrvDeptRef>,

    // AS5438 - Table E.6, Section E.4.4
    pub der_volt_watt: Option<Curve<u16, i16>>,
    pub der_volt_watt_tms: Option<ScaledValue<u32>>,
    pub der_volt_watt_dept_ref: Option<model706::CrvDeptRef>,

    // AS5438 - Table E.5, Section E.4.3
    pub var_set_ena: Option<model704::VarSetEna>,
    pub var_set_mod: Option<model704::VarSetMod>,
    pub var_set_pct: Option<ScaledValue<i16>>,

    // AS5438 - Table E.7, Section E.4.5
    pub der_trip_lv_must: Option<Curve<u16, u32>>,
    pub der_trip_lv_may: Option<Curve<u16, u32>>,
    pub der_trip_lv_mom_cess: Option<Curve<u16, u32>>,
    pub der_trip_hv_must: Option<Curve<u16, u32>>,
    pub der_trip_hv_may: Option<Curve<u16, u32>>,
    pub der_trip_hv_mom_cess: Option<Curve<u16, u32>>,

    // AS5438 - Table E.8, Section E.4.6
    // Note: CSIP does not define a mom_cess for lfrt and hfrt curves.
    pub der_trip_lf_must: Option<Curve<u32, u32>>,
    pub der_trip_lf_may: Option<Curve<u32, u32>>,
    pub der_trip_hf_must: Option<Curve<u32, u32>>,
    pub der_trip_hf_may: Option<Curve<u32, u32>>,

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
    pub w_set_pct: Option<ScaledValue<i16>>,
    // Extension - ena and mod are required to implement w_set_pct. w_set is an
    // alternative way to specify the set point.
    pub w_set_ena: Option<model704::WSetEna>,
    pub w_set_mod: Option<model704::WSetMod>,
    pub w_set: Option<ScaledValue<i32>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model711Ctl {
    pub db_of: ScaledValue<u32>,
    pub db_uf: ScaledValue<u32>,
    pub k_of: ScaledValue<u16>,
    pub k_uf: ScaledValue<u16>,
    pub rsp_tms: ScaledValue<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Curve<TX, TY> {
    pub points: Vec<(TX, TY)>,
    pub sf_x: i16,
    pub sf_y: i16,
}

impl<TX: ScaledValueInner, TY: ScaledValueInner> Curve<TX, TY> {
    /// Returns an iterator over the points as ScaledValues including the scaling factor.
    pub fn iter_scaled(&self) -> impl Iterator<Item = (ScaledValue<TX>, ScaledValue<TY>)> {
        self.points.iter().map(|(x, y)| {
            (
                ScaledValue::new(*x, self.sf_x),
                ScaledValue::new(*y, self.sf_y),
            )
        })
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u16 {
        // We never expect more points than a few. To avoid type errors we
        // convert safely and clamp this to maximum if it's overflowing.
        u16::try_from(self.points.len()).unwrap_or(u16::MAX)
    }
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
    if device_opt.is_some()
        && let Some(metrics) = metrics::metrics()
    {
        metrics.modbus_connection_drops.inc();
    }
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
    send_model703_parameters(device, parameters).await?;
    send_model704_parameters(device, parameters).await?;
    send_model705_parameters(device, parameters).await?;
    send_model706_parameters(device, parameters).await?;
    send_model707_parameters(device, parameters).await?;
    send_model708_parameters(device, parameters).await?;
    send_model709_parameters(device, parameters).await?;
    send_model710_parameters(device, parameters).await?;
    send_model711_parameters(device, parameters).await?;

    Ok(())
}

/// Conditionally writes only when the value is Some(_).
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

/// As for write_if_some, but with a ScaledValue and a target scale factor.
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

/////
// Writes arranged according to the specific models.
async fn send_model703_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    if !device.models.supported_model_ids().contains(&703) {
        return Ok(());
    }

    // AS5438 - Table E.10, Section E.4.8
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

    Ok(())
}

async fn send_model704_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    if !device.models.supported_model_ids().contains(&704) {
        return Ok(());
    }

    // AS5438 - Table E.3, Section E.4.1
    write_if_some(device, Model704::PFW_INJ_ENA, parameters.pfw_inj_ena).await?;
    write_if_some(device, Model704::PFW_ABS_ENA, parameters.pfw_abs_ena).await?;

    // These points are in non-repeating groups near the end of the model. It
    // makes them annoying to write to as we must calculate the offsets
    // manually.
    let pfw_inj_offset = Model704::addr(&device.models).addr + Model704::LEN;
    let pfw_abs_offset = pfw_inj_offset + model704::PfwInj::LEN + model704::PfwInjRvrt::LEN;
    if let Some(pfw_inj_ext) = parameters.pfw_inj_ext {
        write_offset_point(
            device,
            pfw_inj_offset,
            model704::PfwInj::EXT,
            Some(pfw_inj_ext),
        )
        .await?;
    }
    if let Some(pfw_abs_ext) = parameters.pfw_abs_ext {
        write_offset_point(
            device,
            pfw_abs_offset,
            model704::PfwAbs::EXT,
            Some(pfw_abs_ext),
        )
        .await?;
    }
    // Both pfw_inj_pf and pfw_abs_pf share pf_sf so group these and read that
    // SF only once.
    if parameters.pfw_inj_pf.is_some() || parameters.pfw_abs_pf.is_some() {
        let pf_sf = device
            .read_point(Model704::PF_SF)
            .await
            .map_err(comm_err)?
            .unwrap_or_default();
        if let Some(pfw_inj_pf) = parameters.pfw_inj_pf {
            write_offset_point(
                device,
                pfw_inj_offset,
                model704::PfwInj::PF,
                Some(pfw_inj_pf.rescale(pf_sf).value),
            )
            .await?;
        }
        if let Some(pfw_abs_pf) = parameters.pfw_abs_pf {
            write_offset_point(
                device,
                pfw_abs_offset,
                model704::PfwAbs::PF,
                Some(pfw_abs_pf.rescale(pf_sf).value),
            )
            .await?;
        }
    }

    // AS5438 - Table E.5, Section E.4.3
    write_if_some(device, Model704::VAR_SET_ENA, parameters.var_set_ena).await?;
    if parameters.var_set_pct.is_some() {
        let pct_sf = device
            .read_point(Model704::VAR_SET_PCT_SF)
            .await
            .map_err(comm_err)?
            .unwrap_or_default();
        write_rescaled_if_some(
            device,
            Model704::VAR_SET_PCT,
            parameters.var_set_pct,
            pct_sf,
        )
        .await?;
    }
    write_if_some(device, Model704::VAR_SET_MOD, parameters.var_set_mod).await?;

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
        write_rescaled_if_some(device, Model704::W_SET_PCT, parameters.w_set_pct, pct_sf).await?;
    }
    // Extension: also write WSet if available and write WSetMod to indicate
    // which is chosen.
    if parameters.w_set.is_some() {
        let w_set_sf = device
            .read_point(Model704::W_SET_SF)
            .await
            .map_err(comm_err)?
            .unwrap_or_default();
        write_rescaled_if_some(device, Model704::W_SET, parameters.w_set, w_set_sf).await?;
    }
    write_if_some(device, Model704::W_SET_MOD, parameters.w_set_mod).await?;

    Ok(())
}

async fn send_model705_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    // AS5438 - Table E.4, Section E.4.2
    if !device.models.supported_model_ids().contains(&705) {
        return Ok(());
    }

    let wrote_curve = if let Some(der_volt_var) = parameters.der_volt_var.as_ref() {
        if let Some(curve_offset) = Model705::write_curve(device, der_volt_var).await? {
            let tms_sf = device
                .read_point(Model705::RSP_TMS_SF)
                .await
                .map_err(comm_err)?;
            let v_sf = device.read_point(Model705::V_SF).await.map_err(comm_err)?;
            if let Some(tms) = parameters.der_volt_var_tms {
                write_offset_point(
                    device,
                    curve_offset,
                    model705::Crv::RSP_TMS,
                    Some(tms.rescale(tms_sf).value),
                )
                .await
                .map_err(comm_err)?;
            }
            if let Some(dept_ref) = parameters.der_volt_var_dept_ref {
                write_offset_point(device, curve_offset, model705::Crv::DEPT_REF, dept_ref)
                    .await
                    .map_err(comm_err)?;
            }
            if let Some(vref) = parameters.vref {
                write_offset_point(
                    device,
                    curve_offset,
                    model705::Crv::V_REF,
                    Some(vref.rescale(v_sf).value),
                )
                .await
                .map_err(comm_err)?;
            }
            if let Some(auto_ena) = parameters.vref_auto_ena {
                write_offset_point(
                    device,
                    curve_offset,
                    model705::Crv::V_REF_AUTO_ENA,
                    Some(auto_ena),
                )
                .await
                .map_err(comm_err)?;
            }
            if let Some(auto_tms) = parameters.vref_auto_tms {
                write_offset_point(
                    device,
                    curve_offset,
                    model705::Crv::V_REF_AUTO_TMS,
                    Some(auto_tms),
                )
                .await
                .map_err(comm_err)?;
            }

            device
                .write_point(Model705::ADPT_CRV_REQ, Model705::TARGET_CURVE)
                .await
                .map_err(comm_err)?;

            true
        } else {
            false
        }
    } else {
        false
    };

    device
        .write_point(
            Model705::ENA,
            match wrote_curve {
                false => model705::Ena::Disabled,
                true => model705::Ena::Enabled,
            },
        )
        .await
        .map_err(comm_err)?;

    Ok(())
}

async fn send_model706_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    // AS5438 - Table E.6, Section E.4.4
    if !device.models.supported_model_ids().contains(&706) {
        return Ok(());
    }

    let wrote_curve = if let Some(der_volt_watt) = parameters.der_volt_watt.as_ref() {
        if let Some(curve_offset) = Model706::write_curve(device, der_volt_watt).await? {
            if let Some(tms) = parameters.der_volt_watt_tms {
                let tms_sf = device
                    .read_point(Model706::RSP_TMS_SF)
                    .await
                    .map_err(comm_err)?;
                write_offset_point(
                    device,
                    curve_offset,
                    model706::Crv::RSP_TMS,
                    Some(tms.rescale(tms_sf).value),
                )
                .await
                .map_err(comm_err)?;
            }
            if let Some(dept_ref) = parameters.der_volt_watt_dept_ref {
                write_offset_point(device, curve_offset, model706::Crv::DEPT_REF, dept_ref)
                    .await
                    .map_err(comm_err)?;
            }

            device
                .write_point(Model706::ADPT_CRV_REQ, Model706::TARGET_CURVE)
                .await
                .map_err(comm_err)?;

            true
        } else {
            false
        }
    } else {
        false
    };

    device
        .write_point(
            Model706::ENA,
            match wrote_curve {
                false => model706::Ena::Disabled,
                true => model706::Ena::Enabled,
            },
        )
        .await
        .map_err(comm_err)?;

    Ok(())
}

async fn send_model707_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    // AS5438 - Table E.7, Section E.4.5
    if !device.models.supported_model_ids().contains(&707) {
        return Ok(());
    }

    let wrote_curve = Model707::write_curves(
        device,
        parameters.der_trip_lv_must.as_ref(),
        parameters.der_trip_lv_may.as_ref(),
        parameters.der_trip_lv_mom_cess.as_ref(),
    )
    .await?;
    device
        .write_point(
            Model707::ENA,
            if wrote_curve {
                model707::Ena::Enabled
            } else {
                model707::Ena::Disabled
            },
        )
        .await
        .map_err(comm_err)?;

    Ok(())
}

async fn send_model708_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    // AS5438 - Table E.7, Section E.4.5
    if !device.models.supported_model_ids().contains(&708) {
        return Ok(());
    }

    let wrote_curve = Model708::write_curves(
        device,
        parameters.der_trip_hv_must.as_ref(),
        parameters.der_trip_hv_may.as_ref(),
        parameters.der_trip_hv_mom_cess.as_ref(),
    )
    .await?;
    device
        .write_point(
            Model708::ENA,
            if wrote_curve {
                model708::Ena::Enabled
            } else {
                model708::Ena::Disabled
            },
        )
        .await
        .map_err(comm_err)?;

    Ok(())
}

async fn send_model709_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    if !device.models.supported_model_ids().contains(&709) {
        return Ok(());
    }

    // AS5438 - Table E.8, Section E.4.6
    let wrote_curve = Model709::write_curves(
        device,
        parameters.der_trip_lf_must.as_ref(),
        parameters.der_trip_lf_may.as_ref(),
        None,
    )
    .await?;

    device
        .write_point(
            Model709::ENA,
            if wrote_curve {
                model709::Ena::Enabled
            } else {
                model709::Ena::Disabled
            },
        )
        .await
        .map_err(comm_err)?;

    Ok(())
}

async fn send_model710_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    if !device.models.supported_model_ids().contains(&710) {
        return Ok(());
    }

    // AS5438 - Table E.8, Section E.4.6
    let wrote_curve = Model710::write_curves(
        device,
        parameters.der_trip_hf_must.as_ref(),
        parameters.der_trip_hf_may.as_ref(),
        None,
    )
    .await?;

    device
        .write_point(
            Model710::ENA,
            if wrote_curve {
                model710::Ena::Enabled
            } else {
                model710::Ena::Disabled
            },
        )
        .await
        .map_err(comm_err)?;

    Ok(())
}

async fn send_model711_parameters(
    device: &AsyncDevice<TokioModbusContext>,
    parameters: &Parameters,
) -> Result<()> {
    if !device.models.supported_model_ids().contains(&711) {
        return Ok(());
    }

    // AS5438 - Table E.9, Section E.4.7
    let ena = if let Some(droop_ctl) = parameters.droop_ctl.as_ref() {
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

            device
                .write_point(Model711::ADPT_CTL_REQ, 2)
                .await
                .map_err(comm_err)?;
            model711::Ena::Enabled
        } else {
            model711::Ena::Disabled
        }
    } else {
        model711::Ena::Disabled
    };

    device
        .write_point(Model711::ENA, ena)
        .await
        .map_err(comm_err)?;

    Ok(())
}

/// Helper trait to avoid typos when calculating curve offsets.
///
/// A user of this trait should implement it for a given model (e.g. Model710)
/// and define the associated types and constants. The `write_curves` function
/// should then fill in the necessary curve data and requested curve to adopt.
///
/// A trip curve model has 3 curves inside each curve set for MustTrip, MayTrip
/// and MomCess.
trait TripCurveModel<TX, TY>
where
    Self: Model,
    TX: ScaledValueInner + FixedSize,
    TY: ScaledValueInner + FixedSize,
{
    type GCrv: Group;
    type GMustTrip: Group;
    type GMayTrip: Group;
    type GMomCess: Group;
    type GPt: Group;
    const ADPT_CRV_REQ: Point<Self, u16>;
    const N_CRV_SET: Point<Self, u16>;
    const N_PT: Point<Self, u16>;
    const X_SF: Point<Self, i16>;
    const Y_SF: Point<Self, i16>;
    // Note that CRV_ACT_PT must be the same between all 3 curves, so we pick
    // GMustTrip here.
    const CRV_ACT_PT: Point<Self::GMustTrip, Option<u16>>;
    const X_PT: Point<Self::GPt, Option<TX>>;
    const Y_PT: Point<Self::GPt, Option<TY>>;

    // Curve set 1 is defined to be read-only in Sunspec so we require at least
    // two curves to be able to write a new control. We use this const to make the
    // intent clearer where it is used.
    const TARGET_CURVE_SET: u16 = 2;

    fn curve_set_len(n_pt: u16) -> u16 {
        Self::GCrv::LEN
            + Self::curve_len(TripCurve::MustTrip, n_pt)
            + Self::curve_len(TripCurve::MayTrip, n_pt)
            + Self::curve_len(TripCurve::MomCess, n_pt)
    }

    fn curve_static_len(curve: TripCurve) -> u16 {
        match curve {
            TripCurve::MustTrip => Self::GMustTrip::LEN,
            TripCurve::MayTrip => Self::GMayTrip::LEN,
            TripCurve::MomCess => Self::GMomCess::LEN,
        }
    }

    fn curve_len(curve: TripCurve, n_pt: u16) -> u16 {
        Self::curve_static_len(curve) + Self::GPt::LEN * n_pt
    }

    /// The offset of a curve inside the curve set. `curve_set_index` follows 1-based indexing.
    fn curve_offset(curve: TripCurve, curve_set_index: u16, n_pt: u16) -> u16 {
        let curve_set_offset = Self::LEN + Self::curve_set_len(n_pt) * (curve_set_index - 1);
        let additional_offset = match curve {
            TripCurve::MustTrip => Self::GCrv::LEN,
            TripCurve::MayTrip => Self::GCrv::LEN + Self::curve_len(TripCurve::MustTrip, n_pt),
            TripCurve::MomCess => {
                Self::GCrv::LEN
                    + Self::curve_len(TripCurve::MustTrip, n_pt)
                    + Self::curve_len(TripCurve::MayTrip, n_pt)
            }
        };
        curve_set_offset + additional_offset
    }

    /// Write a single curve to its offset location, returning true if the curve was written or not.
    ///
    /// This function should only be called internally by write_curves. It
    /// assumes that n_curves has already been checked for valid length.
    async fn write_curve(
        device: &AsyncDevice<TokioModbusContext>,
        curve: TripCurve,
        curve_data: &Curve<TX, TY>,
        n_pt: u16,
        x_sf: i16,
        y_sf: i16,
    ) -> Result<bool> {
        if curve_data.len() > n_pt {
            log::warn!(
                "Curve data is longer ({}) than the length supported by the device ({}) in model {}.",
                curve_data.len(),
                n_pt,
                Self::ID
            );
            return Ok(false);
        }

        let curve_addr = Self::addr(&device.models).addr
            + Self::curve_offset(curve, Self::TARGET_CURVE_SET, n_pt);

        // Write the number of active points first.
        write_offset_point(device, curve_addr, Self::CRV_ACT_PT, Some(curve_data.len())).await?;

        for (i, point) in curve_data.iter_scaled().enumerate() {
            let point_addr =
                curve_addr + Self::curve_static_len(curve) + Self::GPt::LEN * (i as u16);
            write_offset_point(
                device,
                point_addr,
                Self::X_PT,
                Some(point.0.rescale(x_sf).value),
            )
            .await?;
            write_offset_point(
                device,
                point_addr,
                Self::Y_PT,
                Some(point.1.rescale(y_sf).value),
            )
            .await?;
        }

        Ok(true)
    }

    /// Writes up to 3 curves if they are present in the arguments, filling in
    /// the AdptCrvReq register as well and returning true if any curve was
    /// written or false if not.
    async fn write_curves(
        device: &AsyncDevice<TokioModbusContext>,
        must_trip_data: Option<&Curve<TX, TY>>,
        may_trip_data: Option<&Curve<TX, TY>>,
        mom_cess_data: Option<&Curve<TX, TY>>,
    ) -> Result<bool> {
        let wrote_any = {
            // Short circuit quickly if no models are set so we don't read from the model unnecessarily.
            if must_trip_data.is_none() && may_trip_data.is_none() && mom_cess_data.is_none() {
                false
            } else {
                let n_curves = device.read_point(Self::N_CRV_SET).await.map_err(comm_err)?;
                let n_pt = device.read_point(Self::N_PT).await.map_err(comm_err)?;

                if n_curves < Self::TARGET_CURVE_SET {
                    log::warn!(
                        "Unable to write curve to model {} because the device does not support at least {} curves.",
                        Self::ID,
                        Self::TARGET_CURVE_SET
                    );
                    false
                } else {
                    let x_sf = device.read_point(Self::X_SF).await.map_err(comm_err)?;
                    let y_sf = device.read_point(Self::Y_SF).await.map_err(comm_err)?;

                    let wrote_must_trip = if let Some(data) = must_trip_data {
                        Self::write_curve(device, TripCurve::MustTrip, data, n_pt, x_sf, y_sf)
                            .await?
                    } else {
                        false
                    };
                    let wrote_may_trip = if let Some(data) = may_trip_data {
                        Self::write_curve(device, TripCurve::MayTrip, data, n_pt, x_sf, y_sf)
                            .await?
                    } else {
                        false
                    };
                    let wrote_mom_cess = if let Some(data) = mom_cess_data {
                        Self::write_curve(device, TripCurve::MomCess, data, n_pt, x_sf, y_sf)
                            .await?
                    } else {
                        false
                    };

                    wrote_must_trip || wrote_may_trip || wrote_mom_cess
                }
            }
        };

        if wrote_any {
            device
                .write_point(Self::ADPT_CRV_REQ, Self::TARGET_CURVE_SET)
                .await
                .map_err(comm_err)?;

            // Note that we never check the result emitted in ADPT_CURV_RSLT.
            // This is intentional, as there is nothing for us to do other than
            // warn on it. There is no mechanism for us to respond upstream
            // about a failure and we should continue trying to apply a curve
            // even if there's a failure.
        }

        Ok(wrote_any)
    }
}

#[derive(Clone, Copy)]
enum TripCurve {
    MustTrip,
    MayTrip,
    MomCess,
}

impl TripCurveModel<u16, u32> for Model707 {
    type GCrv = model707::Crv;
    type GMustTrip = model707::MustTrip;
    type GMayTrip = model707::MayTrip;
    type GMomCess = model707::MomCess;
    type GPt = model707::Pt;

    const ADPT_CRV_REQ: Point<Self, u16> = Self::ADPT_CRV_REQ;
    const N_CRV_SET: Point<Self, u16> = Self::N_CRV_SET;
    const N_PT: Point<Self, u16> = Self::N_PT;
    const X_SF: Point<Self, i16> = Self::V_SF;
    const Y_SF: Point<Self, i16> = Self::TMS_SF;
    const CRV_ACT_PT: Point<Self::GMustTrip, Option<u16>> = model707::MustTrip::ACT_PT;
    const X_PT: Point<Self::GPt, Option<u16>> = model707::Pt::V;
    const Y_PT: Point<Self::GPt, Option<u32>> = model707::Pt::TMS;
}

impl TripCurveModel<u16, u32> for Model708 {
    type GCrv = model708::Crv;
    type GMustTrip = model708::MustTrip;
    type GMayTrip = model708::MayTrip;
    type GMomCess = model708::MomCess;
    type GPt = model708::Pt;

    const ADPT_CRV_REQ: Point<Self, u16> = Self::ADPT_CRV_REQ;
    const N_CRV_SET: Point<Self, u16> = Self::N_CRV_SET;
    const N_PT: Point<Self, u16> = Self::N_PT;
    const X_SF: Point<Self, i16> = Self::V_SF;
    const Y_SF: Point<Self, i16> = Self::TMS_SF;
    const CRV_ACT_PT: Point<Self::GMustTrip, Option<u16>> = model708::MustTrip::ACT_PT;
    const X_PT: Point<Self::GPt, Option<u16>> = model708::Pt::V;
    const Y_PT: Point<Self::GPt, Option<u32>> = model708::Pt::TMS;
}

impl TripCurveModel<u32, u32> for Model709 {
    type GCrv = model709::Crv;
    type GMustTrip = model709::MustTrip;
    type GMayTrip = model709::MayTrip;
    type GMomCess = model709::MomCess;
    type GPt = model709::Pt;

    const ADPT_CRV_REQ: Point<Self, u16> = Self::ADPT_CRV_REQ;
    const N_CRV_SET: Point<Self, u16> = Self::N_CRV_SET;
    const N_PT: Point<Self, u16> = Self::N_PT;
    const X_SF: Point<Self, i16> = Self::HZ_SF;
    const Y_SF: Point<Self, i16> = Self::TMS_SF;
    const CRV_ACT_PT: Point<Self::GMustTrip, Option<u16>> = model709::MustTrip::ACT_PT;
    const X_PT: Point<Self::GPt, Option<u32>> = model709::Pt::HZ;
    const Y_PT: Point<Self::GPt, Option<u32>> = model709::Pt::TMS;
}

impl TripCurveModel<u32, u32> for Model710 {
    type GCrv = model710::Crv;
    type GMustTrip = model710::MustTrip;
    type GMayTrip = model710::MayTrip;
    type GMomCess = model710::MomCess;
    type GPt = model710::Pt;

    const ADPT_CRV_REQ: Point<Self, u16> = Self::ADPT_CRV_REQ;
    const N_CRV_SET: Point<Self, u16> = Self::N_CRV_SET;
    const N_PT: Point<Self, u16> = Self::N_PT;
    const X_SF: Point<Self, i16> = Self::HZ_SF;
    const Y_SF: Point<Self, i16> = Self::TMS_SF;
    const CRV_ACT_PT: Point<Self::GMustTrip, Option<u16>> = model710::MustTrip::ACT_PT;
    const X_PT: Point<Self::GPt, Option<u32>> = model710::Pt::HZ;
    const Y_PT: Point<Self::GPt, Option<u32>> = model710::Pt::TMS;
}

/// Helper trait to avoid typos when calculating curve offsets.
///
/// The volt curve models (705, 706) have only a single repeating group but
/// with a different amount of static points inside each. Otherwise, the layout
/// is very similar to TripCurveModel.
trait VoltCurveModel<TX, TY>
where
    Self: Model,
    TX: ScaledValueInner + FixedSize,
    TY: ScaledValueInner + FixedSize,
{
    type GCrv: Group;
    type GPt: Group;
    const N_CRV: Point<Self, u16>;
    const N_PT: Point<Self, u16>;
    const X_SF: Point<Self, i16>;
    const Y_SF: Point<Self, i16>;
    const CRV_ACT_PT: Point<Self::GCrv, u16>;
    const X_PT: Point<Self::GPt, Option<TX>>;
    const Y_PT: Point<Self::GPt, Option<TY>>;

    // Curve 1 is defined to be read-only in Sunspec so we require at least two
    // curves to be able to write a new control. We use this const to make the
    // intent clearer where it is used.
    const TARGET_CURVE: u16 = 2;

    fn curve_len(n_pt: u16) -> u16 {
        Self::GCrv::LEN + Self::GPt::LEN * n_pt
    }

    /// The offset of a curve inside the list. `curve_index` follows 1-based indexing.
    fn curve_offset(curve_index: u16, n_pt: u16) -> u16 {
        Self::LEN + Self::curve_len(n_pt) * (curve_index - 1)
    }

    fn data_point_offset(curve_addr: u16, data_point_index: u16) -> u16 {
        curve_addr + Self::GCrv::LEN + Self::GPt::LEN * data_point_index
    }

    /// Write the curve to its offset location, returning the curve address if
    /// the curve was written and None otherwise.
    ///
    /// It is the caller's responsibility to set the ADPT_CRV_REQ to
    /// TARGET_CURVE when the caller has finished populating the curve data.
    /// This function cannot know all of the other group points.
    async fn write_curve(
        device: &AsyncDevice<TokioModbusContext>,
        curve_data: &Curve<TX, TY>,
    ) -> Result<Option<u16>> {
        let n_curves = device.read_point(Self::N_CRV).await.map_err(comm_err)?;
        let n_pt = device.read_point(Self::N_PT).await.map_err(comm_err)?;

        if n_curves < Self::TARGET_CURVE {
            log::warn!(
                "Unable to write curve to model {} because the device does not support at least {} curves.",
                Self::ID,
                Self::TARGET_CURVE
            );
            return Ok(None);
        }
        if curve_data.len() > n_pt {
            log::warn!(
                "Curve data is longer ({}) than the length supported by the device ({}) in model {}.",
                curve_data.len(),
                n_pt,
                Self::ID
            );
            return Ok(None);
        }

        let x_sf = device.read_point(Self::X_SF).await.map_err(comm_err)?;
        let y_sf = device.read_point(Self::Y_SF).await.map_err(comm_err)?;

        let curve_addr =
            Self::addr(&device.models).addr + Self::curve_offset(Self::TARGET_CURVE, n_pt);

        // Write the number of active points first.
        write_offset_point(device, curve_addr, Self::CRV_ACT_PT, curve_data.len()).await?;

        for (i, point) in curve_data.iter_scaled().enumerate() {
            let data_point_offset = Self::data_point_offset(curve_addr, i as u16);
            write_offset_point(
                device,
                data_point_offset,
                Self::X_PT,
                Some(point.0.rescale(x_sf).value),
            )
            .await?;
            write_offset_point(
                device,
                data_point_offset,
                Self::Y_PT,
                Some(point.1.rescale(y_sf).value),
            )
            .await?;
        }

        Ok(Some(curve_addr))
    }
}

impl VoltCurveModel<u16, i16> for Model705 {
    type GCrv = model705::Crv;
    type GPt = model705::Pt;

    const N_CRV: Point<Self, u16> = Model705::N_CRV;
    const N_PT: Point<Self, u16> = Model705::N_PT;
    const X_SF: Point<Self, i16> = Model705::V_SF;
    const Y_SF: Point<Self, i16> = Model705::DEPT_REF_SF;
    const CRV_ACT_PT: Point<Self::GCrv, u16> = model705::Crv::ACT_PT;
    const X_PT: Point<Self::GPt, Option<u16>> = model705::Pt::V;
    const Y_PT: Point<Self::GPt, Option<i16>> = model705::Pt::VAR;
}

impl VoltCurveModel<u16, i16> for Model706 {
    type GCrv = model706::Crv;
    type GPt = model706::Pt;

    const N_CRV: Point<Self, u16> = Model706::N_CRV;
    const N_PT: Point<Self, u16> = Model706::N_PT;
    const X_SF: Point<Self, i16> = Model706::V_SF;
    const Y_SF: Point<Self, i16> = Model706::DEPT_REF_SF;
    const CRV_ACT_PT: Point<Self::GCrv, u16> = model706::Crv::ACT_PT;
    const X_PT: Point<Self::GPt, Option<u16>> = model706::Pt::V;
    const Y_PT: Point<Self::GPt, Option<i16>> = model706::Pt::W;
}
