use async_broadcast::Sender as BroadcastSender;
use exponential_backoff::Backoff;
use sep2_client::{
    client::{Client, SEPResponse},
    device::SEDevice,
};
use sep2_common::{
    mrid_gen,
    packages::{
        dcap::DeviceCapability,
        der::{
            DER, DERCapability, DERControlList, DERCurveList, DERList, DERProgramList, DERSettings,
            DERStatus, DefaultDERControl,
        },
        edev::{EndDevice, EndDeviceList, Registration},
        fsa::FunctionSetAssignmentsList,
        identification::ResponseStatus,
        metering_mirror::{MirrorMeterReading, MirrorUsagePoint, MirrorUsagePointList},
        primitives::{HexBinary160, Int64, Uint32},
        response::DERControlResponse,
        time::Time,
        types::{
            MRIDType, PINType, PhaseCode, PowerOfTenMultiplierType, UomType, UsagePointStatus,
        },
    },
};
use std::hash::{Hash, Hasher};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::mpsc::{Receiver as MpscReceiver, Sender as MpscSender},
    task::{self, JoinHandle},
    time::{self, Instant},
};

use crate::{Error, ResourceKind, Result};

mod polling;

use polling::{paginated_uri, start_poll_for};

const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Wraps incoming messages from the SEP2 server about resources
#[derive(Clone, Debug)]
pub enum Sep2ResourceEvent {
    Time(Arc<Time>),
    EndDeviceList(Arc<EndDeviceList>),
    FunctionSetAssignmentsList(Arc<FunctionSetAssignmentsList>),
    DERProgramList(Arc<DERProgramList>),
    DefaultDERControl(Arc<DefaultDERControl>),
    DERControlList(Arc<DERControlList>),
    DERCurveList(Arc<DERCurveList>),
}

impl From<EndDeviceList> for Sep2ResourceEvent {
    fn from(resource: EndDeviceList) -> Self {
        Sep2ResourceEvent::EndDeviceList(Arc::new(resource))
    }
}
impl From<FunctionSetAssignmentsList> for Sep2ResourceEvent {
    fn from(resource: FunctionSetAssignmentsList) -> Self {
        Sep2ResourceEvent::FunctionSetAssignmentsList(Arc::new(resource))
    }
}
impl From<DERProgramList> for Sep2ResourceEvent {
    fn from(resource: DERProgramList) -> Self {
        Sep2ResourceEvent::DERProgramList(Arc::new(resource))
    }
}
impl From<DefaultDERControl> for Sep2ResourceEvent {
    fn from(resource: DefaultDERControl) -> Self {
        Sep2ResourceEvent::DefaultDERControl(Arc::new(resource))
    }
}
impl From<DERControlList> for Sep2ResourceEvent {
    fn from(resource: DERControlList) -> Self {
        Sep2ResourceEvent::DERControlList(Arc::new(resource))
    }
}
impl From<DERCurveList> for Sep2ResourceEvent {
    fn from(resource: DERCurveList) -> Self {
        Sep2ResourceEvent::DERCurveList(Arc::new(resource))
    }
}
impl From<Time> for Sep2ResourceEvent {
    fn from(resource: Time) -> Self {
        Sep2ResourceEvent::Time(Arc::new(resource))
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    SendDeviceStatus(DERStatus),
    SendDeviceCapability(DERCapability),
    SendDeviceSettings(DERSettings),
    SendControlResponse(ControlResponse),
    SendMeterReadings(Vec<MirrorMeterReading>),

    /// Sent on initial start or by the retry task.
    Wake,

    SubscribeToResource {
        href: String,
        kind: ResourceKind,
        poll_rate: Option<u32>,
    },
    UnsubscribeFromResource {
        href: String,
    },
}

#[derive(Debug, Clone)]
pub struct ControlResponse {
    subject: MRIDType,
    status: ResponseStatus,
    reply_to: String,
    time: Int64,
}

impl ControlResponse {
    pub fn new(
        subject: MRIDType,
        status: ResponseStatus,
        reply_to: String,
        time: Int64,
    ) -> ControlResponse {
        ControlResponse {
            subject,
            status,
            reply_to,
            time,
        }
    }
}

pub struct Sep2ConnectionArgs {
    pub client: Client,
    pub dcap_uri: String,
    pub max_list_size: u32,
    pub default_poll_rate: u32,
    pub device_to_register: SEDevice,
    pub expected_pin: Option<PINType>,
    pub pen: u32,
}

/// Manages communication to and from the SEP2 server.
///
/// We work in the single-DER single-site mode where the expectations is we only
/// see a single EndDevice although we may be managing a bunch of devices
/// ourselves. This aligns with the GFEMS architecture of the CSIP
/// implementation guide.
pub async fn task(
    output_ch: BroadcastSender<Sep2ResourceEvent>,
    mut input_ch: MpscReceiver<Command>,
    input_ch_tx: MpscSender<Command>,
    args: Sep2ConnectionArgs,
) -> Result<()> {
    // Various resources we only need to lookup once.
    let mut dcap_option = None;
    let mut server_device = None;
    let mut server_registration_info = None;
    let mut setup_root_polling_done = false;
    let mut der_option = None;

    const POST_RATE_GENERIC: Duration = Duration::from_secs(60);
    const POST_RATE_CAPABILITIES: Duration = Duration::from_hours(24);
    // Queues for messages that we need to send or retry.
    // The choice of 30 is intended to be larger than the SEP2 maximum number of
    // controls of 24 with a bit of extra leeway. In practical usage this should
    // never happen.
    const MAX_RESPONSE_QUEUE_SIZE: usize = 30;
    // The response queue has newer messages pushed onto the back and popped from the front.
    let mut control_response_queue = VecDeque::new();
    let mut latest_device_settings = None;
    let mut last_sent_device_settings = None;
    let mut latest_device_status = None;
    let mut last_sent_device_status = None;
    // Capabilities is a little different, we won't get these often so keep a record of them.
    let mut device_capabilities = None;
    let mut last_sent_device_capabilities = None;
    // Meter readings
    let mut latest_meter_readings = None;
    let mut last_sent_meter_readings = None;
    let mut mirror_usage_point = None;
    let mut reading_mrid_cache = HashMap::new();

    let mut retry_task_handle: Option<JoinHandle<_>> = None;
    let mut retry_requested: bool = false;

    // A workaround for sep2_client so we avoid setting up polls on the same URI
    // multiple times.
    let mut poll_history = HashSet::new();

    loop {
        // Spawn a retry task if needed.
        if retry_requested {
            if retry_task_handle.is_none() {
                retry_task_handle = Some(task::spawn(retry_task(input_ch_tx.clone())));
            }
        } else {
            if let Some(handle) = retry_task_handle.take() {
                handle.abort();
                if let Err(join_error) = handle.await
                    && !join_error.is_cancelled()
                {
                    log::warn!("Joining retry task returned an error: {join_error}.");
                }
            }
        }

        let Some(command) = input_ch.recv().await else {
            break;
        };

        let is_wake_command = matches!(command, Command::Wake);

        match command {
            Command::SubscribeToResource {
                href,
                kind,
                poll_rate,
            } => {
                if poll_history.contains(&href) {
                    log::debug!("Not setting up poll for {href}, already polling this URI.");
                    continue;
                }

                start_poll_for(
                    kind,
                    args.client.clone(),
                    &href,
                    poll_rate.unwrap_or(args.default_poll_rate),
                    args.max_list_size,
                    output_ch.clone(),
                )
                .await;

                poll_history.insert(href);
                // Don't fall through to the main sending tasks.
                continue;
            }
            Command::UnsubscribeFromResource { href } => {
                log::warn!("TODO: removal of subscriptions ({href})");
                // Don't fall through to the main sending tasks.
                continue;
            }
            Command::SendDeviceSettings(settings) => {
                log::trace!("Received device settings");
                latest_device_settings = Some(settings);
            }
            Command::SendDeviceCapability(capabilities) => {
                log::trace!("Received device capabilities");
                if Some(&capabilities) != device_capabilities.as_ref() {
                    device_capabilities = Some(capabilities);
                    // Reset the last sent as we need to update the server immediately.
                    last_sent_device_capabilities = None;
                }
            }
            Command::SendDeviceStatus(status) => {
                log::trace!("Received device status");
                latest_device_status = Some(status);
            }
            Command::SendMeterReadings(readings) => {
                log::trace!("Received meter readings");
                latest_meter_readings = Some(readings);
            }
            Command::SendControlResponse(response) => {
                control_response_queue.push_back(response);

                // Limit the queue size.
                while control_response_queue.len() >= MAX_RESPONSE_QUEUE_SIZE {
                    let msg = control_response_queue.pop_front();
                    log::error!(
                        "Control response queue grew too large, dropping message: {:?}",
                        msg
                    );
                }
            }
            Command::Wake => {}
        }

        // If a retry task is running, we shouldn't try sending new messages,
        // wait until it returns. The only exception is if the retry task woke
        // up us.
        if retry_task_handle.is_some() && !is_wake_command {
            continue;
        }

        // At this point we either get to the end of the loop successfully, or
        // we encounter a problem that requires a retry. Hence we set
        // retry_requested preemptively.
        retry_requested = true;

        // Ensuring we have a dcap is highest priority.
        if dcap_option.is_none() {
            dcap_option = get_dcap(args.client.clone(), &args.dcap_uri).await;
        }
        let Some(dcap) = dcap_option.as_ref() else {
            continue;
        };

        // Validate the response - if we don't have the appropriate links, then this is not something we can continue with.
        let (Some(edev_link), Some(tm_link)) =
            (dcap.end_device_list_link.as_ref(), dcap.time_link.as_ref())
        else {
            log::error!(
                "dcap response doesn't have all expected links. We will retry and refetch the dcap endpoint."
            );
            dcap_option = None;
            continue;
        };

        // Ensure our device is registered with the server.
        if server_device.is_none() {
            server_device = ensure_device_registered(
                args.client.clone(),
                &edev_link.href,
                &args.device_to_register,
                args.max_list_size,
            )
            .await?;
        }
        let Some(server_device) = &server_device else {
            continue;
        };

        // If there is a registration link provided and the client was started
        // with a PIN, we validate this PIN matches the server's known PIN.
        if server_registration_info.is_none()
            && let Some(reg_link) = server_device.registration_link.as_ref()
            && let Some(expected_pin) = args.expected_pin
        {
            let reg_info = server_registration_info.insert(
                match args.client.get::<Registration>(&reg_link.href).await {
                    Ok(reg) => reg,
                    Err(_) => {
                        log::error!("Unable to get device registration. Retrying");
                        continue;
                    }
                },
            );

            // Only validate the PIN if the user requested it.
            if expected_pin != reg_info.pin {
                // We assume the server is temporarily misconfigured and retry.
                // However, the likely case is an incorrect PIN setup on the
                // device, so we also raise an error-level log message.
                log::error!("Server device PIN doesn't match expected PIN.");
                continue;
            }
        }

        // Ensure the root polling is setup.
        if !setup_root_polling_done {
            // Setup polls for edev list and time link. All other polls will be created
            // after receiving these responses.
            start_root_polling(
                args.client.clone(),
                &edev_link.href,
                &tm_link.href,
                args.default_poll_rate,
                args.max_list_size,
                output_ch.clone(),
            )
            .await;
            poll_history.insert(edev_link.href.clone());
            poll_history.insert(tm_link.href.clone());
            setup_root_polling_done = true;
        }

        // If there are updates in the status update queue, try and send them out.
        if send_control_responses(
            &mut control_response_queue,
            args.client.clone(),
            args.device_to_register.lfdi,
        )
        .await
        {
            continue;
        }

        // Ensure we have the DER resource with endpoints to post device settings and capabilities to.
        if der_option.is_none() {
            der_option = ensure_der(args.client.clone(), server_device, args.max_list_size).await;
        }
        let Some(der) = der_option.as_ref() else {
            continue;
        };

        // Attempt to send new device settings if we have a valid link.
        if last_sent_device_settings
            .is_none_or(|time| Instant::now().duration_since(time) > POST_RATE_GENERIC)
            && let (Some(device_settings), Some(link)) = (
                latest_device_settings.as_ref(),
                der.der_settings_link.as_ref(),
            )
        {
            log::trace!("Sending settings update");
            match args.client.put(&link.href, device_settings).await {
                Ok(_) => {
                    // This is done now, don't retry sending these same settings.
                    log::debug!("Sent settings");
                    latest_device_settings = None;
                    last_sent_device_settings = Some(Instant::now());
                }
                Err(err) => {
                    log::error!("Unable to post our settings to upstream ({err}). Will retry.");
                    continue;
                }
            }
        }

        // Attempt to send a new device status if we have a valid link
        if last_sent_device_status
            .is_none_or(|time| Instant::now().duration_since(time) > POST_RATE_GENERIC)
            && let (Some(der_status), Some(link)) =
                (latest_device_status.as_ref(), der.der_status_link.as_ref())
        {
            log::trace!("Sending status update");
            match args.client.put(&link.href, der_status).await {
                Ok(_) => {
                    // This is done now, don't retry sending these same settings.
                    log::debug!("Sent status");
                    latest_device_status = None;
                    last_sent_device_status = Some(Instant::now());
                }
                Err(err) => {
                    log::error!("Unable to post our status to upstream ({err}). Will retry.");
                    continue;
                }
            }
        }

        // Attempt to send device capabilities if we have a valid link.
        if last_sent_device_capabilities
            .is_none_or(|time| Instant::now().duration_since(time) > POST_RATE_CAPABILITIES)
            && let (Some(der_capabilities), Some(link)) = (
                device_capabilities.as_ref(),
                der.der_capability_link.as_ref(),
            )
        {
            log::trace!("Sending capabilities");
            match args.client.put(&link.href, der_capabilities).await {
                Ok(_) => {
                    // This is done now, don't retry sending these capabilities.
                    log::debug!("Sent capabilities");
                    last_sent_device_capabilities = Some(Instant::now());
                }
                Err(err) => {
                    log::error!("Unable to post our capabilities to upstream ({err}). Will retry.");
                    continue;
                }
            }
        }

        // Attempt to send meter readings.
        if let Some(readings) = &latest_meter_readings
            && last_sent_meter_readings
                .is_none_or(|time| Instant::now().duration_since(time) > POST_RATE_GENERIC)
        {
            if send_meter_readings(
                &args,
                dcap,
                &mut mirror_usage_point,
                readings,
                &mut reading_mrid_cache,
            )
            .await
            {
                // Retry requested
                continue;
            } else {
                // We succeeded, forget the previous readings.
                last_sent_meter_readings = Some(Instant::now());
                latest_meter_readings = None;
            }
        }

        // If we get to the end, we haven't encountered any errors so don't need to retry
        retry_requested = false;
    }

    log::info!("Input channel closed, stopping SEP2 connection loop");

    Ok(())
}

async fn get_dcap(client: Client, dcap_uri: &str) -> Option<DeviceCapability> {
    match client.get(dcap_uri).await {
        Ok(response) => Some(response),
        Err(err) => {
            log::error!("Failed trying to get dcap: {err}");
            None
        }
    }
}

async fn ensure_device_registered(
    client: Client,
    edev_uri: &str,
    device_to_register: &SEDevice,
    max_list_size: u32,
) -> Result<Option<EndDevice>> {
    // Retrieve edev list. Three options:
    // - list is empty, we need to register ourselves
    // - list has one item, verify it is us.
    // - two or more items: invalid state.
    let Ok(edev_response) = client
        .get::<EndDeviceList>(&paginated_uri(edev_uri, max_list_size))
        .await
    else {
        log::error!("Could not get end device list, retrying.");
        return Ok(None);
    };

    match edev_response.all {
        Uint32(0) => {
            log::info!("Our device is not in server EndDeviceList. Registering device");
            match register_new_device(client.clone(), edev_uri, device_to_register).await {
                Ok(device) => Ok(Some(device)),
                Err(err) => {
                    log::error!("Failed to register our device ({err}). Retrying");
                    Ok(None)
                }
            }
        }
        Uint32(1) => {
            log::info!("Our device is known to the server.");
            // We should definitely have an entry in the list in this branch, but check in case the protocol is inconsistent.
            let device = edev_response
                .end_device
                .into_iter()
                .next()
                .ok_or(Error::InvalidInput(String::from(
                    "EndDeviceList is missing an item",
                )))?;

            // Assert the device returned is us.
            if device.lfdi != Some(device_to_register.lfdi) {
                log::error!("Server knows us by a different LFDI. Stopping.");
                return Err(Error::UnexpectedDevice);
            }
            Ok(Some(device))
        }
        _ => {
            log::error!("Server returned more than one device, this is unexpected. Stopping.");
            Err(Error::UnexpectedDevice)
        }
    }
}

/// Utility for registering a new device.
async fn register_new_device(
    client: Client,
    edev_link: &str,
    device_info: &SEDevice,
) -> Result<EndDevice> {
    let new_device = match client.post(edev_link, &device_info.edev).await {
        Err(_err) => {
            return Err(Error::ItemDetailsUnknown);
        }
        Ok(SEPResponse::Created(loc)) => {
            let loc = loc.ok_or(Error::InvalidInput(String::from(
                "Expected a valid href for the newly registered device",
            )))?;
            // Get the server's representation of the end device.
            client
                .get::<EndDevice>(&loc)
                .await
                .map_err(|_| Error::ItemDetailsUnknown)?
        }
        _ => {
            return Err(Error::ServerRejected);
        }
    };

    log::info!(
        "New device has href: {}",
        new_device.href.as_ref().unwrap_or(&String::from("missing"))
    );

    Ok(new_device)
}

/// Setup the polls we need for all other resources to be discovered.
async fn start_root_polling(
    client: Client,
    edev_uri: &str,
    tm_uri: &str,
    default_poll_rate: u32,
    max_list_size: u32,
    output_ch: BroadcastSender<Sep2ResourceEvent>,
) {
    start_poll_for(
        ResourceKind::EndDeviceList,
        client.clone(),
        edev_uri,
        default_poll_rate,
        max_list_size,
        output_ch.clone(),
    )
    .await;
    start_poll_for(
        ResourceKind::Time,
        client.clone(),
        tm_uri,
        default_poll_rate,
        max_list_size,
        output_ch.clone(),
    )
    .await;
}

async fn ensure_der(client: Client, end_device: &EndDevice, max_list_size: u32) -> Option<DER> {
    let Some(derl_link) = end_device.der_list_link.as_ref() else {
        log::error!("EndDevice is missing DERList link.");
        return None;
    };

    let derl_response = match client
        .get::<DERList>(&paginated_uri(&derl_link.href, max_list_size))
        .await
    {
        Ok(derl) => derl,
        Err(err) => {
            log::error!("Unable to retrieve DERList ({err}). Retrying.");
            return None;
        }
    };

    if derl_response.der.len() != 1 {
        log::error!("Unexpected length of DERList: {}", derl_response.der.len());
        return None;
    }
    derl_response.der.into_iter().next()
}

async fn retry_task(input_ch_tx: MpscSender<Command>) {
    // TODO: Make these configurable settings.
    let mut backoff =
        Backoff::new(u32::MAX, Duration::from_secs(10), Duration::from_secs(300)).into_iter();

    while let Some(Some(wait_time)) = backoff.next() {
        log::debug!("Next retry wait duration: {}s", wait_time.as_secs());
        time::sleep(wait_time).await;

        log::trace!("Waking up main communication task");
        if input_ch_tx.send(Command::Wake).await.is_err() {
            log::error!("Retry task encountered closed channel");
            return;
        }
    }
}

/// Attempts to send all control responses that are queued. Returns true if a retry is requested.
async fn send_control_responses(
    control_response_queue: &mut VecDeque<ControlResponse>,
    client: Client,
    lfdi: HexBinary160,
) -> bool {
    while let Some(response) = control_response_queue.pop_front() {
        let payload = DERControlResponse {
            created_date_time: Some(response.time),
            end_device_lfdi: lfdi,
            status: Some(response.status),
            subject: response.subject,
            href: None,
        };

        let retryable_error = match time::timeout(
            HTTP_TIMEOUT,
            client.post(&response.reply_to, &payload),
        )
        .await
        {
            Err(_) => {
                // Timeout, we should retry.
                Some(format!("Timeout after {}s", HTTP_TIMEOUT.as_secs()))
            }
            Ok(Ok(_)) => {
                log::debug!(
                    "Sent response for control ({subject}) changing status to {status:?} to {href}",
                    subject = response.subject,
                    href = response.reply_to,
                    status = response.status
                );
                None
            }
            Ok(Err(err)) => {
                // Distinguish the kinds of errors: 429 or a connection issue
                // should retry. 404, 500 errors should discard the message.
                // TODO: update the upstream sep2_client library to retry more
                // specific errors instead of string matching.
                let err_s = err.to_string();
                let is_failure_retriable = err_s.contains("Connection refused")
                    || (err_s.contains("Unexpected HTTP response") && err_s.contains("429"));
                if is_failure_retriable {
                    Some(err.to_string())
                } else {
                    // Otherwise this response is discarded.
                    log::error!(
                        "Unable to send response for control ({subject}) changing status to {status:?} to {href} because of {err}. Discarding response.",
                        subject = response.subject,
                        href = response.reply_to,
                        status = response.status,
                    );
                    None
                }
            }
        };

        // If retriable failure, push back onto the queue.
        if let Some(err) = retryable_error {
            log::debug!(
                "Unable to send response for control ({subject}) changing status to {status:?} to {href} because of {err}. Queueing for retry.",
                subject = response.subject,
                href = response.reply_to,
                status = response.status,
            );

            control_response_queue.push_front(response);
            return true;
        }
    }

    // No retry required.
    false
}

/// Attempts to send meter readings that are queued. Returns true if a retry is required.
async fn send_meter_readings(
    args: &Sep2ConnectionArgs,
    dcap: &DeviceCapability,
    mirror_usage_point: &mut Option<String>,
    readings: &Vec<MirrorMeterReading>,
    reading_mrid_cache: &mut HashMap<CacheKey, MRIDType>,
) -> bool {
    // Note: we can't create a MUP until we have readings to attach to it. Kind
    // of silly in my opinion and means we need to ensure the MUP link in the
    // middle of our readings logic.

    // Make sure we've initialised the mrid cache
    if reading_mrid_cache.is_empty() {
        *reading_mrid_cache = initialise_reading_mrid_cache(
            args.client.clone(),
            dcap,
            args.device_to_register.lfdi,
            args.max_list_size,
        )
        .await;
    }

    for reading in readings {
        let Some(key) = reading_key(reading) else {
            log::error!(
                "Got an unknown cache key for a reading we are attempting to send. This should never happen, discarding reading."
            );
            // Don't retry, this will keep on happening. Instead discard this reading.
            continue;
        };

        // Fill in a MRID from the cache, or generate one if the cache is empty.
        let mrid = *reading_mrid_cache
            .entry(key)
            .or_insert_with(|| mrid_gen(args.pen));
        let modified_reading = MirrorMeterReading {
            mrid,
            ..reading.clone()
        };
        if let Some(mup) = mirror_usage_point {
            if let Err(err) = args.client.post(mup, &modified_reading).await {
                log::error!("Unable to send meter readings ({err}). Will retry.");
                return true;
            }
        } else {
            // We lookup or create a MUP with this reading.
            let Some(mupl_link) = dcap.mirror_usage_point_list_link.as_ref() else {
                // It is unusual if the server doesn't have a MUP link. Keep retrying.
                log::debug!("No MUP list link found. This is unexpected.");
                return true;
            };

            *mirror_usage_point = ensure_mup(args, &mupl_link.href, modified_reading).await;

            if mirror_usage_point.is_none() {
                // If ensure_mup returned None, then this means we need to retry.
                return true;
            }
        }
    }

    log::debug!("Sent meter readings");

    false
}

/// Lookup the MirrorUsagePoint for this device. Returns the full MUP details or
/// None if no MUP is registered
async fn lookup_mup(
    client: Client,
    mupl_href: &str,
    lfdi: HexBinary160,
    max_list_size: u32,
) -> Option<MirrorUsagePoint> {
    let mup_list = match client
        .get::<MirrorUsagePointList>(&paginated_uri(mupl_href, max_list_size))
        .await
    {
        Ok(mupl) => mupl,
        Err(err) => {
            log::error!("Unable to retrieve MirrorUsagePointList ({err}). Retrying.");
            return None;
        }
    };

    // See if we already have a MUP registered.
    mup_list
        .mirror_usage_point
        .iter()
        .find(|mup| mup.device_lfdi == lfdi)
        .cloned()
}

/// Looks up the MirrorUsagePoint for this device or creates it if not
/// registered. Returns only the href to the MUP on success and returns None on
/// failure.
///
/// Requires a reading to be able to send to the server. In both cases of the
/// MUP being created, or the MUP pre-existing, this function will send the
/// reading.
async fn ensure_mup(
    args: &Sep2ConnectionArgs,
    mupl_href: &str,
    reading: MirrorMeterReading,
) -> Option<String> {
    // See if we already have a MUP registered.
    if let Some(mup) = lookup_mup(
        args.client.clone(),
        mupl_href,
        args.device_to_register.lfdi,
        args.max_list_size,
    )
    .await
    {
        let Some(href) = mup.href else {
            log::error!("No href in MirrorUsagePoint response.");
            return None;
        };
        log::debug!("Identified {} as MirrorUsagePoint", href);
        // In this path we have a MUP and haven't sent any readings to the
        // server. In the alternative path, a MUP is created and it must send a
        // reading to do so. Hence, we also send the reading here.
        if let Err(err) = args.client.post(&href, &reading).await {
            log::error!("Unable to send meter readings ({err}). Will retry.");
            return None;
        }
        return Some(href);
    }

    // If not, then we register a new MUP.
    match args
        .client
        .post(
            mupl_href,
            &MirrorUsagePoint {
                mrid: mrid_gen(args.pen),
                device_lfdi: args.device_to_register.lfdi,
                status: UsagePointStatus::On,
                mirror_meter_reading: vec![reading],
                ..Default::default()
            },
        )
        .await
    {
        Ok(SEPResponse::Created(Some(location))) => {
            log::debug!("Registered MirrorUsagePoint at {}", location);
            Some(location)
        }
        Ok(response) => {
            log::error!("Unknown response to creating MirrorUsagePoint ({response}). Retrying.");
            None
        }
        Err(err) => {
            log::error!("Couldn't register MirrorUsagePoint ({err}). Retrying.");
            None
        }
    }
}

/// Prepares the MRID cache for meter readings. Looks up existing
/// MirrorMeterReadings in the server and identifies their MRIDs. Falls back to
/// an empty cache if any issues are found.
async fn initialise_reading_mrid_cache(
    client: Client,
    dcap: &DeviceCapability,
    lfdi: HexBinary160,
    max_list_size: u32,
) -> HashMap<CacheKey, MRIDType> {
    let Some(mupl_link) = dcap.mirror_usage_point_list_link.as_ref() else {
        // We hope that this will get returned by the server soon.
        log::debug!("No MUP list link found. This is unexpected.");
        return HashMap::new();
    };

    let Some(mup) = lookup_mup(client.clone(), &mupl_link.href, lfdi, max_list_size).await else {
        return HashMap::new();
    };

    // Look at the readings and extract a key for the hashmap.
    // Note that we might find multiple readings with different MRIDs. In which
    // case we let the HashMap automatically select one of them by key
    // collisions and overwriting.
    mup.mirror_meter_reading
        .iter()
        .filter_map(|reading| reading_key(reading).map(|key| (key, reading.mrid)))
        .inspect(|(key, mrid)| {
            log::debug!("Found existing MRID for MirrorMeterReading: {key:?} -> {mrid}")
        })
        .collect()
}

/// Returns a hashable key that is unique for all of the meter readings that we
/// can send, that is a combination of measurement unit and a phase option. If
/// the meter reading type is incomplete, returns None.
fn reading_key(reading: &MirrorMeterReading) -> Option<CacheKey> {
    reading.reading_type.as_ref().and_then(|reading_type| {
        reading_type.uom.as_ref().map(|uom| {
            CacheKey(
                *uom,
                reading_type.phase,
                // Default the power of ten to POTM::None so that there's no
                // confusion between a None and a Some(None).
                reading_type.power_of_ten_multiplier.unwrap_or_default(),
            )
        })
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CacheKey(UomType, Option<PhaseCode>, PowerOfTenMultiplierType);

impl Hash for CacheKey {
    /// Manual implementation to extract the integers of the key for hashing.
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0 as u8).hash(state);
        (self.1.map(|x| x as u8)).hash(state);
        (self.2 as i8).hash(state);
    }
}
