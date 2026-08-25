// Tests the full path a control takes from the SEP2 server to the device.
// Assertions are made against the mocked device's registers.

mod modbus_server_mock;

use std::{str::FromStr, time::Duration};

use chrono::Utc;
use modbus_server_mock::SunSpecMock;
use sep2_bridge::{
    Result, deactivated_broadcast, dispatch, modbus_connection, scheduler, sep2_connection,
};
use sep2_client::{client::Client, device::SEDevice};
use sep2_common::{
    packages::{
        der::{
            DERControl, DERControlBase, DERControlList, DERProgram, DERProgramList,
            DefaultDERControl, FreqDroopType,
        },
        fsa::{FunctionSetAssignments, FunctionSetAssignmentsList},
        identification::{Link, ListLink},
        primitives::{HexBinary160, Int16, Int64, Uint16, Uint32},
        types::{
            DateTimeInterval, DeviceCategoryType, MRIDType, Percent, PrimacyType, SFDIType,
            SignedPercent,
        },
    },
    traits::SEType,
};
use sunspec::{
    Value,
    models::{model703, model704, model711},
};
use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{self, Instant},
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

// How long a value is given to travel from the SEP2 server to the device.
// Because we setup the mocks before tasks are started, we avoid much of the
// usual wait time so this value can be quite short.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(1);

// How often the device registers are re-read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

const MOCK_POLL_RATE: u32 = 1;

const HREF_EDEVL: &str = "/edev";
const HREF_EDEV: &str = "/edev/1";
const HREF_FSAL: &str = "/edev/1/fsa";
const HREF_FSA: &str = "/edev/1/fsa/1";
const HREF_DERPL: &str = "/edev/1/fsa/1/derp";
const HREF_DERP: &str = "/edev/1/derp/1";
const HREF_DDERC: &str = "/edev/1/derp/1/dderc";
const HREF_DERCL: &str = "/edev/1/derp/1/derc";
const HREF_DERC_1: &str = "/edev/1/derp/1/derc/1";

// The values to be mocked and verified. Table numbers from AS5438
// Table 9
const DROOP_DB_OF: u32 = 360;
const DROOP_DB_UF: u32 = 350;
const DROOP_K_OF: u16 = 50;
const DROOP_K_UF: u16 = 40;
const DROOP_OPEN_LOOP_TMS: u16 = 5;
// Table 10
const SET_ES_CONNECT: bool = true;
const SET_ES_HIGH_VOLT: i16 = 2450;
const SET_ES_LOW_VOLT: i16 = 2000;
const SET_ES_HIGH_FREQ: u16 = 5100;
const SET_ES_LOW_FREQ: u16 = 4900;
const SET_ES_DELAY: u32 = 300;
const SET_ES_RANDOM_DELAY: u32 = 60;
const SET_ES_RAMP_TMS: u32 = 120;
// Table 11
const OP_MOD_MAX_LIM_W: u16 = 8000;
// Table 12
const OP_MOD_FIXED_W: i16 = -2500;

/// Tests that the DefaultDERControl parameters reach model 703.
/// (AS5438 - Table 10)
#[tokio::test]
async fn applies_as5438_table_10() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    assert_register(&mock, "model703::ESV_HI", Some(SET_ES_HIGH_VOLT as u16)).await;
    assert_register(&mock, "model703::ESV_LO", Some(SET_ES_LOW_VOLT as u16)).await;
    assert_register(
        &mock,
        "model703::ES_HZ_HI",
        Some(u32::from(SET_ES_HIGH_FREQ)),
    )
    .await;
    assert_register(
        &mock,
        "model703::ES_HZ_LO",
        Some(u32::from(SET_ES_LOW_FREQ)),
    )
    .await;
    assert_register(&mock, "model703::ES_DLY_TMS", Some(SET_ES_DELAY)).await;
    assert_register(&mock, "model703::ES_RND_TMS", Some(SET_ES_RANDOM_DELAY)).await;
    assert_register(&mock, "model703::ES_RMP_TMS", Some(SET_ES_RAMP_TMS)).await;

    // The mock seeds this as Disabled, so this cannot pass vacuously.
    // We manually are doing the conversion from true to the enum, so assert the
    // const is what we expect.
    const {
        assert!(SET_ES_CONNECT);
    }
    assert_register(&mock, "model703::ES", Some(model703::Es::Enabled)).await;
}

/// Tests that an active DERControl's power limits reach model 704.
/// (AS5438 - Tables 11 and 12)
#[tokio::test]
async fn applies_as5438_tables_11_12() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    assert_register(
        &mock,
        "model704::W_MAX_LIM_PCT_ENA",
        Some(model704::WMaxLimPctEna::Enabled),
    )
    .await;
    assert_register(&mock, "model704::W_MAX_LIM_PCT", Some(OP_MOD_MAX_LIM_W)).await;

    assert_register(
        &mock,
        "model704::W_SET_ENA",
        Some(model704::WSetEna::Enabled),
    )
    .await;
    assert_register(&mock, "model704::W_SET_PCT", Some(OP_MOD_FIXED_W)).await;
}

/// Tests that an active DERControl's opModFreqDroop reaches model 711. The
/// values must land in the second (writable) control group, leaving the first
/// read-only group alone.
/// (AS5438 - Table 9)
#[tokio::test]
async fn applies_as5438_table_9() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    assert_register(&mock, "model711::CTL_1::DB_OF", DROOP_DB_OF).await;
    assert_register(&mock, "model711::CTL_1::DB_UF", DROOP_DB_UF).await;
    assert_register(&mock, "model711::CTL_1::K_OF", DROOP_K_OF).await;
    assert_register(&mock, "model711::CTL_1::K_UF", DROOP_K_UF).await;
    assert_register(
        &mock,
        "model711::CTL_1::RSP_TMS",
        u32::from(DROOP_OPEN_LOOP_TMS),
    )
    .await;

    // The read-only group reporting the current settings must not be touched,
    // and the writable group must stay marked writable.
    assert_register(&mock, "model711::CTL_0::DB_OF", 0u32).await;
    assert_register(
        &mock,
        "model711::CTL_1::READ_ONLY",
        model711::CtlReadOnly::Rw,
    )
    .await;
}

/////
// Helpers

/// Waits for a named register on the device mock to reach `expected` and then
/// asserts on it, so a failure reports the last value actually seen.
///
/// Note that a failure to translate the SEP2 attributes is only logged as a
/// warning by `dispatch::control_change_dispatcher`, so it surfaces here as a
/// register that never changes.
async fn assert_register<T>(mock: &SunSpecMock, name: &str, expected: T)
where
    T: Value + PartialEq + std::fmt::Debug,
{
    let deadline = Instant::now() + SETTLE_TIMEOUT;

    let mut actual = mock.get_value::<T>(name);
    while actual != expected && Instant::now() < deadline {
        time::sleep(POLL_INTERVAL).await;
        actual = mock.get_value::<T>(name);
    }

    assert_eq!(
        actual, expected,
        "register {name} never reached the expected value"
    );
}

fn mock_lfdi() -> HexBinary160 {
    HexBinary160::from_str("00112233").expect("Invalid LFDI")
}

/// Starts a mocked sunspec device, a mocked SEP2 server exposing one active
/// control, and the full set of bridge tasks wiring the two together.
async fn setup() -> (
    SunSpecMock,
    MockServer,
    JoinSet<Result<()>>,
    async_broadcast::Receiver<modbus_connection::Event>,
) {
    let lfdi = mock_lfdi();
    let sfdi = SFDIType::new(42).expect("Invalid SFDI");
    let device = SEDevice::new(lfdi, sfdi, DeviceCategoryType::empty());

    // The mocked modbus device.
    let mut sunspec_mock = SunSpecMock::new(None)
        .await
        .expect("Couldn't create mock modbus server");
    sunspec_mock
        .start()
        .await
        .expect("Couldn't start mock modbus server");

    // The mocked SEP2 server. All endpoints are mounted before the tasks start
    // so that no polling cycle has to elapse before they are seen.
    let sep2_mock = MockServer::start().await;
    setup_base_mocks(&sep2_mock, lfdi, sfdi).await;
    setup_control_mocks(&sep2_mock).await;

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
                    pen: 42,
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
    let (mut modbus_output_tx, modbus_output_rx) = async_broadcast::broadcast(10);
    // Nothing is consuming from the output channel in these tests, so allow it to overflow.
    modbus_output_tx.set_overflow(true);
    join_set.spawn(modbus_connection::task(
        modbus_output_tx,
        modbus_input_rx,
        modbus_connection::Transport::Tcp(sunspec_mock.addr.unwrap()),
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

    // Wake up the sep2_connection task to begin its work. Because all mocks are
    // in place already, this should speed through.
    sep2_conn_input_tx
        .send(sep2_connection::Command::Wake)
        .await
        .expect("Send error");

    (sunspec_mock, sep2_mock, join_set, modbus_output_rx)
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

/// Serialises a SEP2 resource and mounts it at the given path.
async fn mock_resource<R: SEType>(mock: &MockServer, path: &str, resource: &R) {
    let body = sep2_common::serialize(resource).expect("Unable to serialize resource");
    mock_get(mock, String::from(path), body).await;
}

/// Mounts the endpoints the sep2_connection task always queries.
async fn setup_base_mocks(mock: &MockServer, lfdi: HexBinary160, sfdi: SFDIType) {
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
    <DERCapabilityLink href="/edev/1/der/1/dercap"/>
    <DERSettingsLink href="/edev/1/der/1/derg"/>
    <DERStatusLink href="/edev/1/der/1/ders"/>
  </DER>
</DERList>"#))
        .await;
}

/// Mounts the chain of resources leading from the EndDevice to a single active
/// DERControl and the program's DefaultDERControl.
async fn setup_control_mocks(mock: &MockServer) {
    mock_resource(
        mock,
        HREF_FSAL,
        &FunctionSetAssignmentsList {
            href: Some(HREF_FSAL.into()),

            function_set_assignments: vec![FunctionSetAssignments {
                href: Some(HREF_FSA.into()),
                der_program_list_link: Some(ListLink {
                    href: HREF_DERPL.into(),
                    ..Default::default()
                }),
                mrid: MRIDType(1),
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
        HREF_DERPL,
        &DERProgramList {
            href: Some(HREF_DERPL.into()),

            der_program: vec![DERProgram {
                href: Some(HREF_DERP.into()),
                default_der_control_link: Some(Link {
                    href: HREF_DDERC.into(),
                }),
                der_control_list_link: Some(ListLink {
                    href: HREF_DERCL.into(),
                    ..Default::default()
                }),

                mrid: MRIDType(123),

                primacy: PrimacyType::NonContractualServiceProvider,
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
        HREF_DDERC,
        &DefaultDERControl {
            href: Some(HREF_DDERC.into()),

            set_es_high_volt: Some(Int16(SET_ES_HIGH_VOLT)),
            set_es_low_volt: Some(Int16(SET_ES_LOW_VOLT)),
            set_es_high_freq: Some(Uint16(SET_ES_HIGH_FREQ)),
            set_es_low_freq: Some(Uint16(SET_ES_LOW_FREQ)),
            set_es_delay: Some(Uint32(SET_ES_DELAY)),
            set_es_random_delay: Some(Uint32(SET_ES_RANDOM_DELAY)),
            set_es_ramp_tms: Some(Uint32(SET_ES_RAMP_TMS)),

            ..Default::default()
        },
    )
    .await;

    // A control that has already started and stays active well past the end of
    // the test.
    let der_control = DERControl {
        href: Some(HREF_DERC_1.into()),
        mrid: MRIDType(42),

        der_control_base: DERControlBase {
            op_mod_connect: Some(SET_ES_CONNECT),
            op_mod_max_lim_w: Some(Percent::new(OP_MOD_MAX_LIM_W).expect("Invalid percent")),
            op_mod_fixed_w: Some(SignedPercent::new(OP_MOD_FIXED_W).expect("Invalid percent")),
            op_mod_freq_droop: Some(FreqDroopType {
                d_bof: Uint32(DROOP_DB_OF),
                d_buf: Uint32(DROOP_DB_UF),
                k_of: Uint16(DROOP_K_OF),
                k_uf: Uint16(DROOP_K_UF),
                open_loop_tms: Uint16(DROOP_OPEN_LOOP_TMS),
            }),
            ..Default::default()
        },

        interval: DateTimeInterval {
            start: Int64(Utc::now().timestamp()),
            duration: Uint32(3600),
        },
        ..Default::default()
    };

    mock_resource(
        mock,
        HREF_DERCL,
        &DERControlList {
            href: Some(HREF_DERCL.into()),
            der_control: vec![der_control],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        },
    )
    .await;
}
