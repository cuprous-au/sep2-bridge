// This is a helper module shared by multiple tests
#![allow(dead_code)]

//! Runs the full set of bridge tasks against a mocked SEP2 server and a mocked
//! sunspec device.

use std::{net::SocketAddr, str::FromStr, time::Duration};

use chrono::Utc;
use sep2_bridge::{
    Result, deactivated_broadcast, dispatch, modbus_connection, scheduler, sep2_connection,
};
use sep2_client::{client::Client, device::SEDevice};
use sep2_common::{
    Pen,
    packages::{
        primitives::HexBinary160,
        types::{DeviceCategoryType, SFDIType},
    },
    traits::SEType,
};
use tokio::{sync::mpsc, task::JoinSet};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

pub const MOCK_POLL_RATE: u32 = 1;

pub const HREF_EDEVL: &str = "/edev";
pub const HREF_EDEV: &str = "/edev/1";
pub const HREF_FSAL: &str = "/edev/1/fsa";
pub const HREF_DERCAP: &str = "/edev/1/der/1/dercap";
pub const HREF_DERG: &str = "/edev/1/der/1/derg";
pub const HREF_DERS: &str = "/edev/1/der/1/ders";

pub fn mock_lfdi() -> HexBinary160 {
    HexBinary160::from_str("00112233").expect("Invalid LFDI")
}

pub fn mock_sfdi() -> SFDIType {
    SFDIType::new(42).expect("Invalid SFDI")
}

/// Starts the full set of bridge tasks wiring the sunspec device to the SEP2
/// server, similar to what's done in main.rs.
///
/// All mocks should be mounted and registers seeded before calling this, so
/// that no polling cycle has to elapse before they are seen.
pub async fn start_bridge(sunspec_addr: SocketAddr, sep2_mock: &MockServer) -> JoinSet<Result<()>> {
    let lfdi = mock_lfdi();
    let device = SEDevice::new(lfdi, mock_sfdi(), DeviceCategoryType::empty());

    let client = Client::new(
        &format!("http://{}", sep2_mock.address()),
        None,
        Some(Duration::from_secs(1)),
    )
    .expect("Unable to create client");

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
                sep2_connection::Sep2ConnectionArgs {
                    client,
                    dcap_uri: String::from("/dcap"),
                    max_list_size: 30,
                    default_poll_rate: 1,
                    device_to_register: device,
                    expected_pin: None,
                    pen: Pen::csipaus(42).expect("valid pen"),
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
        None,
    ));

    // Start the modbus task.
    let (modbus_input_tx, modbus_input_rx) = mpsc::channel(10);
    let (modbus_output_tx, modbus_output_rx) = deactivated_broadcast(10);
    join_set.spawn(modbus_connection::task(
        modbus_output_tx,
        modbus_input_rx,
        modbus_connection::Transport::Tcp(sunspec_addr),
        1,
    ));

    // Dispatch sep2_conn events to the scheduler.
    join_set.spawn(dispatch::resource_update_dispatcher(
        sep2_conn_output_rx.activate_cloned(),
        scheduler_input_tx.clone(),
    ));

    // Dispatch scheduler events.
    join_set.spawn(dispatch::sep2_subscription_and_notification_dispatcher(
        scheduler_output_rx.activate_cloned(),
        sep2_conn_input_tx.clone(),
    ));
    join_set.spawn(dispatch::control_change_dispatcher(
        scheduler_output_rx.activate_cloned(),
        modbus_input_tx.clone(),
    ));

    // Dispatch modbus_conn events to the sep2_conn task.
    join_set.spawn(dispatch::sep2_device_state_dispatcher(
        modbus_output_rx.activate_cloned(),
        sep2_conn_input_tx.clone(),
    ));

    // Wake up the sep2_connection task to begin its work. Because all mocks are
    // in place already, this should speed through.
    sep2_conn_input_tx
        .send(sep2_connection::Command::Wake)
        .await
        .expect("Send error");

    join_set
}

pub async fn mock_get(mock: &MockServer, path: String, body: String) {
    Mock::given(matchers::method("GET"))
        .and(matchers::path(path.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/sep+xml"))
        .expect(1..)
        .named(path)
        .mount(mock)
        .await;
}

/// Serialises a SEP2 resource and mounts it at the given path.
pub async fn mock_resource<R: SEType>(mock: &MockServer, path: &str, resource: &R) {
    let body = sep2_common::serialize(resource).expect("Unable to serialize resource");
    mock_get(mock, String::from(path), body).await;
}

/// Mounts the endpoints the sep2_connection task always queries.
pub async fn setup_base_mocks(mock: &MockServer) {
    let lfdi = mock_lfdi();
    let sfdi = mock_sfdi();
    let now = Utc::now().timestamp();

    mock_get(mock, String::from("/dcap"),
        format!(r#"<DeviceCapability xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/dcap" pollRate="{MOCK_POLL_RATE}">
  <TimeLink href="/tm"/>
  <EndDeviceListLink href="{HREF_EDEVL}" all="1"/>
</DeviceCapability>"#))
        .await;

    mock_get(mock, String::from(HREF_EDEVL),
        format!(r#"<EndDeviceList xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="{HREF_EDEVL}" subscribable="1" all="1" results="1" pollRate="{MOCK_POLL_RATE}">
  <EndDevice href="{HREF_EDEV}" subscribable="1">
    <DERListLink href="/edev/1/der" all="1"/>
    <deviceCategory>00</deviceCategory>
    <lFDI>{lfdi}</lFDI>
    <LogEventListLink href="/edev/1/lel"/>
    <sFDI>{sfdi}</sFDI>
    <changedTime>{now}</changedTime>
    <enabled>true</enabled>
    <FunctionSetAssignmentsListLink href="{HREF_FSAL}" all="1"/>
    <RegistrationLink href="/edev/1/rg"/><csipaus:ConnectionPointLink href="/edev/1/cp"/>
  </EndDevice>
</EndDeviceList>"#))
        .await;

    mock_get(mock, String::from("/tm"),
        format!(r#"<Time xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/tm">
  <currentTime>{now}</currentTime>
  <dstEndTime>0</dstEndTime>
  <dstOffset>0</dstOffset>
  <dstStartTime>0</dstStartTime>
  <quality>4</quality>
  <tzOffset>0</tzOffset>
</Time>"#))
        .await;

    mock_get(mock, String::from("/edev/1/der"),
        format!(r#"<DERList xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/edev/1/der" all="1" results="1" pollRate="{MOCK_POLL_RATE}">
  <DER href="/edev/1/der/1">
    <AssociatedDERProgramListLink href="/edev/1/der/1/derp"/>
    <DERAvailabilityLink href="/edev/1/der/1/dera"/>
    <DERCapabilityLink href="{HREF_DERCAP}"/>
    <DERSettingsLink href="{HREF_DERG}"/>
    <DERStatusLink href="{HREF_DERS}"/>
  </DER>
</DERList>"#))
        .await;
}
