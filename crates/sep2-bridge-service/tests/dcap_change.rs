use std::{str::FromStr, time::Duration};

use chrono::Utc;
use sep2_bridge::{Result, deactivated_broadcast, dispatch, scheduler, sep2_connection};
use sep2_client::{client::Client, device::SEDevice};
use sep2_common::{
    Pen,
    packages::{
        dcap::DeviceCapability,
        der::{DER, DERList},
        edev::{EndDevice, EndDeviceList, Registration},
        fsa::{FunctionSetAssignments, FunctionSetAssignmentsList},
        identification::{Link, ListLink},
        primitives::{HexBinary160, Int64, Uint32},
        time::Time,
        types::{DeviceCategoryType, MRIDType, PINType, SFDIType},
    },
    traits::SEType,
};
use tokio::{sync::mpsc, task::JoinSet, time};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

// The time to wait for all events to be emitted.
const FLUSH_TIME: Duration = Duration::from_millis(500);

const CHANNEL_SIZE: usize = 50;

const MOCK_POLL_RATE: u32 = 1;
const MOCK_PIN: u32 = 123456;

const HREF_DCAP: &str = "/dcap";
const HREF_TM: &str = "/tm";
const HREF_EDEVL: &str = "/edev";
// The new location for the edev list.
const HREF_EDEVL_MOVED: &str = "/edev2";
const HREF_EDEV: &str = "/edev/1";
const HREF_REGISTRATION: &str = "/edev/1/rg";
const HREF_DERL: &str = "/edev/1/der";
const HREF_DER: &str = "/edev/1/der/1";
const HREF_FSAL: &str = "/edev/1/fsa";
const HREF_FSA: &str = "/edev/1/fsa/1";

/// Tests that updating the dcap's link to the EndDeviceList causes the
/// sep2_connection task to re-register the device. At the same time, no other
/// part of the heirachy is changed, so the scheduler should not lose knowledge
/// of its resources.
#[tokio::test]
async fn dcap_change() {
    let (mock, _join_set, sep2_conn_input_tx, mut scheduler_output) = setup().await;

    // Wait for the complete set of reported links from the scheduler.
    let initial_links = collect_link_events(&mut scheduler_output).await;
    assert!(initial_links.contains(&LinkEvent::Added(String::from(HREF_EDEVL))));
    assert!(initial_links.contains(&LinkEvent::Added(String::from(HREF_FSAL))));

    // Move the EndDeviceList to a new href. Its contents are unchanged.
    // Note that resetting also clears the received_requests().
    mock.reset().await;
    setup_mocks(&mock, HREF_EDEVL_MOVED).await;

    // Tell the task the dcap changed, emulating the dcap poll.
    sep2_conn_input_tx
        .send(sep2_connection::Command::ResetConnection)
        .await
        .expect("Send error");

    // Get the set of links emitted since sending ResetConnection.
    let links = collect_link_events(&mut scheduler_output).await;

    // The sep2_connectino task should have checked the device registration again.
    let paths: Vec<String> = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|req| String::from(req.url.path()))
        .collect();
    assert!(paths.contains(&HREF_EDEVL_MOVED.to_string()));
    assert!(paths.contains(&HREF_REGISTRATION.to_string()));

    // The scheduler should swap over the EndDeviceList and nothing else.
    assert_eq!(
        links,
        vec![
            LinkEvent::Added(String::from(HREF_EDEVL_MOVED)),
            LinkEvent::Removed(String::from(HREF_EDEVL)),
        ]
    );
}

/////
// Helpers

/// Only the href information we care about: enables easier comparisons.
#[derive(Debug, PartialEq, Eq)]
enum LinkEvent {
    Added(String),
    Removed(String),
}

async fn collect_link_events(
    output_ch: &mut async_broadcast::Receiver<scheduler::Event>,
) -> Vec<LinkEvent> {
    let mut links = Vec::new();
    while let Ok(event) = time::timeout(FLUSH_TIME, output_ch.recv()).await {
        match event.expect("Recv failure") {
            scheduler::Event::LinkAddedOrUpdated { href, .. } => links.push(LinkEvent::Added(href)),
            scheduler::Event::LinkRemoved { href, .. } => links.push(LinkEvent::Removed(href)),
            _ => {}
        }
    }
    links
}

fn mock_lfdi() -> HexBinary160 {
    HexBinary160::from_str("00112233").expect("Invalid LFDI in test")
}

fn mock_sfdi() -> SFDIType {
    SFDIType::new(42).expect("Invalid SFDI in test")
}

async fn setup() -> (
    MockServer,
    JoinSet<Result<()>>,
    mpsc::Sender<sep2_connection::Command>,
    async_broadcast::Receiver<scheduler::Event>,
) {
    let device = SEDevice::new(mock_lfdi(), mock_sfdi(), DeviceCategoryType::empty());

    // The mocked SEP2 server. All endpoints are mounted before the tasks start.
    let mock = MockServer::start().await;
    setup_mocks(&mock, HREF_EDEVL).await;

    let client = Client::new(
        &format!("http://{}", mock.address()),
        None,
        Some(Duration::from_secs(1)),
    )
    .expect("Unable to create client");

    let mut join_set = JoinSet::new();

    // Start the SEP2 connection management task.
    let (sep2_conn_input_tx, sep2_conn_input_rx) = mpsc::channel(10);
    let (sep2_conn_output_tx, sep2_conn_output_rx) = deactivated_broadcast(CHANNEL_SIZE);
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
                    default_poll_rate: MOCK_POLL_RATE,
                    device_to_register: device,
                    // A PIN must be given to have the sep2_connection task
                    // reads the registration link.
                    expected_pin: Some(PINType::new(MOCK_PIN).expect("Invalid PIN in test")),
                    pen: Pen::csipaus(42).expect("valid pen"),
                },
            )
            .await
        }
    });

    // Start the scheduler task.
    let (scheduler_input_tx, scheduler_input_rx) = mpsc::channel(10);
    let (scheduler_output_tx, scheduler_output_rx) = deactivated_broadcast(CHANNEL_SIZE);
    join_set.spawn(scheduler::task(
        scheduler_output_tx,
        scheduler_input_rx,
        scheduler_input_tx.clone(),
        mock_lfdi(),
        None,
    ));

    // Feed the resources read from the server into the scheduler's model.
    join_set.spawn(dispatch::resource_update_dispatcher(
        sep2_conn_output_rx.activate_cloned(),
        scheduler_input_tx.clone(),
    ));

    // And turn the scheduler's link events back into polls.
    join_set.spawn(dispatch::sep2_subscription_and_notification_dispatcher(
        scheduler_output_rx.activate_cloned(),
        sep2_conn_input_tx.clone(),
    ));

    // Listen in on the scheduler before anything is emitted.
    let scheduler_output = scheduler_output_rx.activate();

    // Wake up the sep2_connection task to begin its work.
    sep2_conn_input_tx
        .send(sep2_connection::Command::Wake)
        .await
        .expect("Send error");

    (mock, join_set, sep2_conn_input_tx, scheduler_output)
}

async fn mock_get(mock: &MockServer, path: String, body: String) {
    Mock::given(matchers::method("GET"))
        .and(matchers::path(path.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/sep+xml"))
        .named(path)
        .mount(mock)
        .await;
}

/// Serialises a SEP2 resource and mounts it at the given path.
async fn mock_resource<R: SEType>(mock: &MockServer, path: &str, resource: &R) {
    let body = sep2_common::serialize(resource).expect("Unable to serialize resource");
    mock_get(mock, String::from(path), body).await;
}

async fn setup_mocks(mock: &MockServer, edevl_href: &str) {
    let now = Int64(Utc::now().timestamp());

    mock_resource(
        mock,
        HREF_DCAP,
        &DeviceCapability {
            href: Some(HREF_DCAP.into()),
            time_link: Some(Link {
                href: HREF_TM.into(),
            }),
            end_device_list_link: Some(ListLink {
                href: edevl_href.into(),
                all: Some(Uint32(1)),
            }),
            poll_rate: Some(Uint32(MOCK_POLL_RATE)),
            ..Default::default()
        },
    )
    .await;

    mock_resource(
        mock,
        edevl_href,
        &EndDeviceList {
            href: Some(edevl_href.into()),

            end_device: vec![EndDevice {
                href: Some(HREF_EDEV.into()),
                lfdi: Some(mock_lfdi()),
                sfdi: mock_sfdi(),
                changed_time: now,
                enabled: Some(true),
                der_list_link: Some(ListLink {
                    href: HREF_DERL.into(),
                    all: Some(Uint32(1)),
                }),
                function_set_assignments_list_link: Some(ListLink {
                    href: HREF_FSAL.into(),
                    all: Some(Uint32(1)),
                }),
                registration_link: Some(Link {
                    href: HREF_REGISTRATION.into(),
                }),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),
            poll_rate: Some(Uint32(MOCK_POLL_RATE)),

            ..Default::default()
        },
    )
    .await;

    mock_resource(
        mock,
        HREF_REGISTRATION,
        &Registration {
            href: Some(HREF_REGISTRATION.into()),
            date_time_registered: now,
            pin: PINType::new(MOCK_PIN).expect("Invalid PIN in test"),
            ..Default::default()
        },
    )
    .await;

    mock_resource(
        mock,
        HREF_FSAL,
        &FunctionSetAssignmentsList {
            href: Some(HREF_FSAL.into()),

            function_set_assignments: vec![FunctionSetAssignments {
                href: Some(HREF_FSA.into()),
                mrid: MRIDType(1),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),
            poll_rate: Some(Uint32(MOCK_POLL_RATE)),

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

            der: vec![DER {
                href: Some(HREF_DER.into()),
                der_capability_link: Some(Link {
                    href: "/edev/1/der/1/dercap".into(),
                }),
                der_settings_link: Some(Link {
                    href: "/edev/1/der/1/derg".into(),
                }),
                der_status_link: Some(Link {
                    href: "/edev/1/der/1/ders".into(),
                }),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),
            poll_rate: Some(Uint32(MOCK_POLL_RATE)),
        },
    )
    .await;
}
