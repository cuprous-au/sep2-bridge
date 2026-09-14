use regex::regex;
use std::{str::FromStr, time::Duration};

use chrono::Utc;
use sep2_bridge::{Result, sep2_connection};
use sep2_client::{client::Client, device::SEDevice};
use sep2_common::packages::{
    der::{ActivePower, DERCapability},
    metering::ReadingType,
    metering_mirror::MirrorMeterReading,
    primitives::{HexBinary160, Int16},
    types::{DeviceCategoryType, PowerOfTenMultiplierType, SFDIType, UomType},
};
use tokio::{
    sync::mpsc,
    task::{self, JoinHandle},
    time::{self, Instant},
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

// Generic timeout
const TIMEOUT: Duration = Duration::from_secs(1);
// Wait time long enough that a task should have flushed its queue.
const FLUSH_TIME: Duration = Duration::from_millis(100);

/// Tests the querying of the server in returning resources.
#[tokio::test]
async fn reads_resources() {
    // Setup
    let (_task, mock, input_ch, mut output_ch) = setup().await;

    // Ensure we get the first few events from the endpoints that are always queried.
    assert!(matches!(
        get_event(&mut output_ch).await,
        sep2_connection::Sep2ResourceEvent::EndDeviceList(_)
    ));
    assert!(matches!(
        get_event(&mut output_ch).await,
        sep2_connection::Sep2ResourceEvent::Time(_)
    ));

    // Now request a new subscription to FSA:
    setup_fsal_mock(&mock).await;

    input_ch
        .send(sep2_connection::Command::SubscribeToResource {
            href: String::from("/edev/1/fsa"),
            kind: sep2_bridge::ResourceKind::FunctionSetAssignmentsList,
            poll_rate: None,
        })
        .await
        .expect("Send error");

    // And ensure it is read
    assert!(matches!(
        get_event(&mut output_ch).await,
        sep2_connection::Sep2ResourceEvent::FunctionSetAssignmentsList(_)
    ));
}

/// Tests that the task does not perform multiple queries unless needed.
#[tokio::test]
async fn no_duplicate_polls() {
    // Setup
    let (_task, mock, input_ch, mut output_ch) = setup().await;

    // Clear out the first few events
    time::sleep(FLUSH_TIME).await;
    clear_channel(&mut output_ch).await;

    // Prepare the FSAL mock
    setup_fsal_mock(&mock).await;

    // Ask for it to be polled the first time.
    let poll_request = sep2_connection::Command::SubscribeToResource {
        href: String::from("/edev/1/fsa"),
        kind: sep2_bridge::ResourceKind::FunctionSetAssignmentsList,
        poll_rate: None,
    };
    input_ch
        .send(poll_request.clone())
        .await
        .expect("Send error");
    // We expect a resource returned.
    assert!(matches!(
        get_event(&mut output_ch).await,
        sep2_connection::Sep2ResourceEvent::FunctionSetAssignmentsList(_)
    ));

    // Ask for it to be polled a second time. Note that this relies on other
    // polls not getting in the way, which is fine if we are only waiting at
    // most FLUSH_TIME plus the small amount time spent processing up till this
    // point.
    input_ch
        .send(poll_request.clone())
        .await
        .expect("Send error");

    // We should not have a resource returned immediately:
    assert!(
        find_event_satisfying(&mut output_ch, FLUSH_TIME, |ev| matches!(
            ev,
            sep2_connection::Sep2ResourceEvent::FunctionSetAssignmentsList(_)
        ))
        .await
        .is_none()
    );

    // And we should see only one attempt to poll the FSAL endpoint:
    let count: u32 = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|req| req.url.path() == "/edev/1/fsa")
        .map(|_| 1)
        .sum();
    assert_eq!(count, 1);
}

/// Tests the pushing to upstream.
#[tokio::test]
async fn sends_capabilities() {
    // Setup
    let (_task, mock, input_ch, mut output_ch) = setup().await;

    // Setup mock at the edev capabilities endpoint.
    Mock::given(matchers::method("PUT"))
        .and(matchers::path("/edev/1/der/1/dercap"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .named("dercap")
        .mount(&mock)
        .await;

    // Send the capabilities
    let capabilities = sep2_connection::Command::SendDeviceCapability(DERCapability {
        rtg_max_w: ActivePower {
            multiplier: PowerOfTenMultiplierType::None,
            value: Int16(42),
        },
        ..Default::default()
    });
    input_ch.send(capabilities).await.expect("Send error");

    // Clear out any events
    time::sleep(FLUSH_TIME).await;
    clear_channel(&mut output_ch).await;

    // Check the received message for the right details.
    let expected = regex!(r"(?ms)<rtgMaxW>.*42.*</rtgMaxW>");
    assert!(
        mock.received_requests()
            .await
            .unwrap()
            .into_iter()
            .any(|req| req.url.path() == "/edev/1/der/1/dercap"
                && expected.is_match(&String::from_utf8(req.body).unwrap()))
    );
}

/// Tests whether the task registers a MUP and pushes metering readings.
#[tokio::test]
async fn sends_metering_readings() {
    // Setup
    let (_task, mock, input_ch, mut output_ch) = setup().await;

    setup_mup_mocks(&mock).await;

    // Send some metering readings - we need enough information that the
    // sep2_connection task can build a mrid cache key out of it
    let metering = sep2_connection::Command::SendMeterReadings(vec![
        MirrorMeterReading {
            reading_type: Some(ReadingType {
                uom: Some(UomType::W),
                ..Default::default()
            }),
            ..Default::default()
        },
        // Only one reading will only cause a registration (as registration
        // requires at least one reading to be present). To test posting to the
        // endpoint itself, pass a second reading.
        MirrorMeterReading {
            reading_type: Some(ReadingType {
                uom: Some(UomType::Hz),
                ..Default::default()
            }),
            ..Default::default()
        },
    ]);
    input_ch.send(metering).await.expect("Send error");

    // Clear out any events
    time::sleep(FLUSH_TIME).await;
    clear_channel(&mut output_ch).await;

    // We expect the MUP endpoints to have been hit.
    let requests = mock.received_requests().await.unwrap();
    assert!(requests.iter().any(|r| r.url.path() == "/mup"));
    assert!(requests.iter().any(|r| r.url.path() == "/mup/2"));
}

/// Tests whether the meter readings will reuse an existing MRID
#[tokio::test]
async fn reuses_existing_meter_mrid() {
    // Setup
    let (_task, mock, input_ch, mut output_ch) = setup().await;

    let mrid = "AAE241C10056E16C7A87BBBF00000000";

    // Setup mock at the mup listing endpoint with previous meter readings.
    mock_get(&mock, String::from("/mup"), format!(r#"
<MirrorUsagePointList xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/mup" all="0" results="0" pollRate="{MOCK_POLL_RATE}">
  <MirrorUsagePoint href="/mup/2">
    <mRID>EEF6A7789439E7456F9D2CDF00000000</mRID>
    <roleFlags>00</roleFlags>
    <serviceCategoryKind>0</serviceCategoryKind>
    <status>1</status>
    <deviceLFDI>{lfdi}</deviceLFDI>
    <MirrorMeterReading>
      <mRID>{mrid}</mRID>
      <description>active_power</description>
      <ReadingType>
        <accumulationBehaviour>12</accumulationBehaviour>
        <commodity>1</commodity>
        <dataQualifier>0</dataQualifier>
        <flowDirection>19</flowDirection>
        <intervalLength>0</intervalLength>
        <kind>0</kind>
        <powerOfTenMultiplier>0</powerOfTenMultiplier>
        <uom>38</uom>
      </ReadingType>
    </MirrorMeterReading>
  </MirrorUsagePoint>
</MirrorUsagePointList>
"#, lfdi=mock_lfdi())).await;

    // Send a meter reading with the same UoM as the mock.
    let metering = sep2_connection::Command::SendMeterReadings(vec![MirrorMeterReading {
        reading_type: Some(ReadingType {
            uom: Some(UomType::W),
            ..Default::default()
        }),
        ..Default::default()
    }]);
    input_ch.send(metering).await.expect("Send error");

    // Clear out any events
    time::sleep(FLUSH_TIME).await;
    clear_channel(&mut output_ch).await;

    // Ensure the request sent to the SEP2 server contains the same MRID.
    assert!(
        mock.received_requests()
            .await
            .unwrap()
            .into_iter()
            .inspect(|req| eprintln!("{:?}", req))
            .any(|req| String::from_utf8(req.body).unwrap().contains(mrid))
    );
}

/////
// Helpers

async fn get_event<T: Clone>(output_ch: &mut async_broadcast::Receiver<T>) -> T {
    time::timeout(TIMEOUT, output_ch.recv())
        .await
        .expect("No events")
        .expect("Recv error")
}

async fn clear_channel<T: Clone>(output_ch: &mut async_broadcast::Receiver<T>) {
    while !output_ch.is_empty() {
        output_ch.recv().await.expect("Recv error");
    }
}

/// Waits for up to timeout for any event that satisfies the supplied predicate.
/// Returns None if no event occurred satisfying the predicate.
async fn find_event_satisfying<T, F>(
    output_ch: &mut async_broadcast::Receiver<T>,
    timeout: Duration,
    predicate: F,
) -> Option<T>
where
    T: Clone,
    F: Fn(&T) -> bool,
{
    let target_max_time = Instant::now() + timeout;
    loop {
        match time::timeout_at(target_max_time, output_ch.recv()).await {
            Err(_) => {
                return None;
            }
            Ok(Err(_)) => {
                panic!("Recv error");
            }
            Ok(Ok(ev)) if predicate(&ev) => {
                return Some(ev);
            }
            _ => continue,
        }
    }
}

fn mock_lfdi() -> HexBinary160 {
    HexBinary160::from_str("00112233").expect("Invalid LFDI")
}

async fn setup() -> (
    JoinHandle<Result<()>>,
    MockServer,
    mpsc::Sender<sep2_connection::Command>,
    async_broadcast::Receiver<sep2_connection::Sep2ResourceEvent>,
) {
    // Our fake device
    let lfdi = mock_lfdi();
    let sfdi = SFDIType::new(42).expect("Invalid SFDI");
    let device = SEDevice::new(lfdi, sfdi, DeviceCategoryType::empty());

    // The mocked SEP2 server
    let mock = MockServer::start().await;

    // The sep2_connection task will always query the dcap, tm, and edev lists
    // so ensure those are mocked and ready before we start the task, otherwise
    // we'd have to wait for a polling cycle.
    setup_base_mocks(&mock, lfdi, sfdi).await;

    // The tickrate of 1s here sets the upper limit for a test to perform
    // queries before it might see the "second round" of polling. This is quite
    // tight, but as long as we don't create complex tests, it should be easy to
    // stay within this budget.
    let client = Client::new(
        &format!("http://{}", mock.address()),
        None,
        Some(Duration::from_secs(1)),
    )
    .expect("Unable to create client");

    let (input_tx, input_rx) = mpsc::channel(10);
    let (output_tx, output_rx) = async_broadcast::broadcast(10);
    let task = task::spawn({
        let input_tx = input_tx.clone();
        async move {
            sep2_connection::task(
                output_tx,
                input_rx,
                input_tx,
                sep2_connection::Sep2ConnectionArgs {
                    client,
                    dcap_uri: String::from("/dcap"),
                    max_list_size: 30,
                    default_poll_rate: 1,
                    device_to_register: device,
                    expected_pin: None,
                    pen: 42,
                },
            )
            .await
        }
    });

    // Start the task off.
    input_tx
        .send(sep2_connection::Command::Wake)
        .await
        .expect("Send error");

    (task, mock, input_tx, output_rx)
}

async fn mock_get(mock: &MockServer, path: String, body: String) {
    Mock::given(matchers::method("GET"))
        .and(matchers::path(path.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/sep+xml"))
        .expect(1..)
        .named(path)
        .mount(mock)
        .await;
}

const MOCK_POLL_RATE: u32 = 1;
async fn setup_base_mocks(mock: &MockServer, lfdi: HexBinary160, sfdi: SFDIType) {
    let now = Utc::now().timestamp();

    mock_get(mock, String::from("/dcap"),
        format!(r#"<DeviceCapability xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/dcap" pollRate="{MOCK_POLL_RATE}">
  <TimeLink href="/tm"/>
  <EndDeviceListLink href="/edev" all="1"/>
  <MirrorUsagePointListLink href="/mup" all="0"/>
</DeviceCapability>"#))
        .await;

    mock_get(mock, String::from("/edev"),
        format!(r#"<EndDeviceList xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/edev" subscribable="1" all="1" results="1" pollRate="{MOCK_POLL_RATE}">
  <EndDevice href="/edev/1" subscribable="1">
    <DERListLink href="/edev/1/der" all="1"/>
    <deviceCategory>00</deviceCategory>
    <lFDI>{lfdi}</lFDI>
    <LogEventListLink href="/edev/1/lel"/>
    <sFDI>{sfdi}</sFDI>
    <changedTime>{now}</changedTime>
    <enabled>true</enabled>
    <FunctionSetAssignmentsListLink href="/edev/1/fsa" all="1"/>
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
    <DERCapabilityLink href="/edev/1/der/1/dercap"/>
    <DERSettingsLink href="/edev/1/der/1/derg"/>
    <DERStatusLink href="/edev/1/der/1/ders"/>
  </DER>
</DERList>"#))
        .await;
}

async fn setup_fsal_mock(mock: &MockServer) {
    mock_get(mock, String::from("/edev/1/fsa"), String::from(r#"<FunctionSetAssignmentsList xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/edev/1/fsa" subscribable="1" all="1" results="1" pollRate="300">
  <FunctionSetAssignments href="/edev/1/fsa/1">
    <DERProgramListLink href="/edev/1/fsa/1/derp" all="2"/>
    <TariffProfileListLink href="/edev/1/fsa/1/tp"/>
    <TimeLink href="/tm"/>
    <mRID>40000000000000010000000100000000</mRID>
    <description></description>
  </FunctionSetAssignments>
</FunctionSetAssignmentsList>"#),
    ).await;
}

async fn setup_mup_mocks(mock: &MockServer) {
    // Setup mock at the mup listing endpoint with no registered MUP.
    mock_get(mock, String::from("/mup"), String::from(r#"
<MirrorUsagePointList xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/mup" all="0" results="0" pollRate="60">
</MirrorUsagePointList>
"#)).await;

    // And setup mock to return a new registered MUP when an attempt is made.
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/mup"))
        .respond_with(ResponseTemplate::new(201).append_header("Location", "/mup/2"))
        .expect(1)
        .named("MUP register")
        .mount(mock)
        .await;

    // Setup mock for the push to the registered MUP
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/mup/2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1..)
        .named("MUP reading post")
        .mount(mock)
        .await;
}
