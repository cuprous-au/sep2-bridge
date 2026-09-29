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
        dcap::DeviceCapability,
        der::{DER, DERList},
        edev::{EndDevice, EndDeviceList},
        identification::{Link, ListLink},
        primitives::{HexBinary160, Int64, Uint32},
        time::Time,
        types::{DeviceCategoryType, SFDIType},
    },
    traits::SEType,
};
use tokio::{sync::mpsc, task::JoinSet};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

pub const MOCK_POLL_RATE: u32 = 1;

pub const HREF_DCAP: &str = "/dcap";
pub const HREF_TM: &str = "/tm";
pub const HREF_EDEVL: &str = "/edev";
pub const HREF_MUPL: &str = "/mup";
pub const HREF_MUP: &str = "/mup/1";
pub const HREF_EDEV: &str = "/edev/1";
pub const HREF_FSAL: &str = "/edev/1/fsa";
pub const HREF_DERL: &str = "/edev/1/der";
pub const HREF_DER: &str = "/edev/1/der/1";
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
                    dcap_uri: String::from(HREF_DCAP),
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
pub async fn setup_base_mocks(mock: &MockServer, include_mup_link: bool) {
    let now = Int64(Utc::now().timestamp());

    mock_resource(
        mock,
        HREF_DCAP,
        &DeviceCapability {
            href: Some(HREF_DCAP.into()),
            poll_rate: Some(Uint32(MOCK_POLL_RATE)),
            time_link: Some(Link {
                href: HREF_TM.into(),
            }),
            end_device_list_link: Some(ListLink {
                href: HREF_EDEVL.into(),
                all: Some(Uint32(1)),
            }),
            mirror_usage_point_list_link: include_mup_link.then_some(ListLink {
                href: HREF_MUPL.into(),
                all: Some(Uint32(1)),
            }),
            ..Default::default()
        },
    )
    .await;

    mock_resource(
        mock,
        HREF_EDEVL,
        &EndDeviceList {
            href: Some(HREF_EDEVL.into()),
            poll_rate: Some(Uint32(MOCK_POLL_RATE)),
            end_device: vec![EndDevice {
                href: Some(HREF_EDEV.into()),
                der_list_link: Some(ListLink {
                    href: HREF_DERL.into(),
                    all: Some(Uint32(1)),
                }),
                device_category: Some(DeviceCategoryType::empty()),
                lfdi: Some(mock_lfdi()),
                sfdi: mock_sfdi(),
                enabled: Some(true),
                function_set_assignments_list_link: Some(ListLink {
                    href: HREF_FSAL.into(),
                    all: Some(Uint32(1)),
                }),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        },
    )
    .await;

    mock_resource(
        mock,
        HREF_TM,
        &Time {
            href: Some(HREF_TM.into()),
            current_time: now,
            ..Default::default()
        },
    )
    .await;

    mock_resource(
        mock,
        HREF_DERL,
        &DERList {
            href: Some(HREF_DERL.into()),
            poll_rate: Some(Uint32(MOCK_POLL_RATE)),

            der: vec![DER {
                href: Some(HREF_DER.into()),
                der_capability_link: Some(Link {
                    href: HREF_DERCAP.into(),
                }),
                der_settings_link: Some(Link {
                    href: HREF_DERG.into(),
                }),
                der_status_link: Some(Link {
                    href: HREF_DERS.into(),
                }),

                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),
        },
    )
    .await;
}
