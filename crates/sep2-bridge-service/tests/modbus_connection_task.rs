mod modbus_server_mock;

use std::time::Duration;

use modbus_server_mock::SunSpecMock;
use sep2_bridge::{
    Result, ScaledValue,
    modbus_connection::{self, Capabilities, Metering, Model711Ctl, Settings, Status, Transport},
};
use sunspec::models::{model701, model703, model711};
use tokio::{
    sync::mpsc,
    task::{self, JoinHandle},
    time,
};

const WAIT_TIME: Duration = Duration::from_millis(100);
const WAIT_POLL_TIME: Duration = Duration::from_millis(1200);

/// Tests that a device receives parameters when a control is applied.
#[tokio::test]
async fn sends_parameters_to_device() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;

    // Provide a parameters command
    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                es: Some(model703::Es::Enabled),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    // Wait for processing
    time::sleep(WAIT_TIME).await;

    // Ensure the device mock has received the parameters.
    let value = mock.get_value::<model703::Es>("model703::ES");
    assert_eq!(value, model703::Es::Enabled);
}

/// Tests that parameters are rescaled from whatever scale factor they arrive
/// with to the one the device advertises.
///
/// The scale factors here are deliberately not the ones SEP2 fixes its values
/// at, so this covers the rescale itself rather than the SEP2 translation.
#[tokio::test]
async fn rescales_parameters_to_device_scale_factors() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // 24% and 20% of nominal voltage, to the mock's V_SF of -1.
                esv_hi: Some(ScaledValue::new(24, 0)),
                esv_lo: Some(ScaledValue::new(200, -1)),
                // 51.0 Hz and 49.0 Hz, to the mock's HZ_SF of -3.
                es_hz_hi: Some(ScaledValue::new(510, -1)),
                es_hz_lo: Some(ScaledValue::new(4900, -2)),
                // 80% and -25%, to the mock's SFs of 0 and -1.
                w_max_lim_pct: Some(ScaledValue::new(800, -1)),
                w_set_pct: Some(ScaledValue::new(-25, 0)),
                droop_ctl: Some(Model711Ctl {
                    // 1.00 Hz and 0.50 Hz, to the mock's DB_SF of -2.
                    db_of: ScaledValue::new(1, 0),
                    db_uf: ScaledValue::new(500, -3),
                    // 0.05 and 0.04 per unit, to the mock's K_SF of -4.
                    k_of: ScaledValue::new(5, -2),
                    k_uf: ScaledValue::new(40, -3),
                    // 5 seconds, to the mock's RSP_TMS_SF of 0.
                    rsp_tms: ScaledValue::new(5000, -3),
                }),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(mock.get_value::<Option<u16>>("model703::ESV_HI"), Some(240));
    assert_eq!(mock.get_value::<Option<u16>>("model703::ESV_LO"), Some(200));
    assert_eq!(
        mock.get_value::<Option<u32>>("model703::ES_HZ_HI"),
        Some(51_000)
    );
    assert_eq!(
        mock.get_value::<Option<u32>>("model703::ES_HZ_LO"),
        Some(49_000)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-250)
    );
    assert_eq!(mock.get_value::<u32>("model711::CTL_2::DB_OF"), 100);
    assert_eq!(mock.get_value::<u32>("model711::CTL_2::DB_UF"), 50);
    assert_eq!(mock.get_value::<u16>("model711::CTL_2::K_OF"), 500);
    assert_eq!(mock.get_value::<u16>("model711::CTL_2::K_UF"), 400);
    assert_eq!(mock.get_value::<u32>("model711::CTL_2::RSP_TMS"), 5);
    assert_eq!(
        mock.get_value::<model711::Ena>("model711::ENA"),
        model711::Ena::Enabled
    );
    assert_eq!(mock.get_value::<u16>("model711::ADPT_CTL_REQ"), 2);
}

/// Tests that a device which doesn't implement a scale factor register is
/// treated as using a scale factor of zero, rather than failing the write.
#[tokio::test]
async fn tolerates_missing_scale_factors() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<i16>>("model703::V_SF", None);
        mock.set_value::<Option<i16>>("model703::HZ_SF", None);
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // 24.50% and 51.00 Hz, which fall back to whole units.
                esv_hi: Some(ScaledValue::new(2450, -2)),
                es_hz_hi: Some(ScaledValue::new(5100, -2)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(mock.get_value::<Option<u16>>("model703::ESV_HI"), Some(25));
    assert_eq!(
        mock.get_value::<Option<u32>>("model703::ES_HZ_HI"),
        Some(51)
    );
}

/// Tests the task reads and emits the device capabilities, status and state.
#[tokio::test]
async fn reads_device_state() {
    // Setup
    let (mock, _task, _input_ch, mut output_ch) = setup(None).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect the default parameters from the mock.
    let all_events = collect_all(&mut output_ch).await;

    let expected_w_max_rtg = mock.get_value::<Option<u16>>("model702::W_MAX_RTG");
    let expected_st = mock.get_value::<Option<model701::St>>("model701::ST");
    let expected_w = mock.get_value::<Option<i16>>("model701::W");

    // Values that carry a scale factor must be reported with the scale factor
    // the device advertises alongside them, not bare.
    let expected_esv_hi = mock
        .get_value::<Option<u16>>("model703::ESV_HI")
        .map(|esv_hi| {
            ScaledValue::new(
                esv_hi,
                mock.get_value::<Option<i16>>("model703::V_SF")
                    .expect("Mock has no V_SF"),
            )
        });
    let expected_soc = mock.get_value::<Option<u16>>("model713::SOC").map(|soc| {
        ScaledValue::new(
            soc,
            mock.get_value::<Option<i16>>("model713::PCT_SF")
                .expect("Mock has no PCT_SF"),
        )
    });
    // Expect received capabilities struct.
    assert!(all_events.iter().any(
        |ev| matches!(ev, modbus_connection::Event::CapabilitiesPolled(
            Capabilities { w_max_rtg, .. }
        )
            if w_max_rtg == &expected_w_max_rtg
        )
    ));

    // Expect received status and settings structs.
    assert!(
        all_events
            .iter()
            .any(|ev| matches!(ev, modbus_connection::Event::StatePolled(
                Some(Status {
                    st, soc, ..
                }),
                Some(Settings {
                    esv_hi
                }),
                Some(Metering {
                    w, ..
                }),
            ) if esv_hi == &expected_esv_hi
                && soc == &expected_soc
                && st == &expected_st
                && w == &expected_w
            ))
    );
}

/// Tests the reconnection of the task when the server goes down.
#[tokio::test]
async fn reconnects() {
    // Setup
    let (mut mock, _task, _input_ch, mut output_ch) = setup(None).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect to have received a capabilities from the mock.
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));

    // Now disconnect the mock server.
    mock.stop().await;

    time::sleep(WAIT_POLL_TIME).await;

    // After wait, change the capaibilities and reconnect mock.
    let expected_w_max_rtg = Some(999u16);
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", expected_w_max_rtg);
    mock.start()
        .await
        .expect("Couldn't start mock modbus server");

    // Wait for the task to reconnect
    time::sleep(WAIT_POLL_TIME).await;

    // And we expect to again receive the capabilities from the mock.
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(
        |ev| matches!(ev, modbus_connection::Event::CapabilitiesPolled(
            Capabilities { w_max_rtg, .. }
        ) if w_max_rtg == &expected_w_max_rtg
        )
    ));
}

/// Tests the reconnection logic when the device only supports model 1, then
/// disconnects and reappears with more models.
#[tokio::test]
async fn reconnects_and_discovers_new_models() {
    // Setup
    let (mut mock, _task, _input_ch, mut output_ch) = setup(Some(&[1])).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect a device connected message and at least one poll with no data
    let all_events = collect_all(&mut output_ch).await;
    assert!(
        all_events
            .iter()
            .any(|ev| matches!(ev, modbus_connection::Event::DeviceConnected(_)))
    );
    assert!(
        all_events
            .iter()
            .any(|ev| matches!(ev, modbus_connection::Event::StatePolled(None, None, None)))
    );

    // Now disconnect the mock server.
    mock.stop().await;

    time::sleep(WAIT_POLL_TIME).await;

    // After wait, change the mock to support capabilities.
    mock.reinit(Some(&[1, 701, 702, 703]));
    mock.start()
        .await
        .expect("Couldn't start mock modbus server");

    // Wait for the task to reconnect
    time::sleep(WAIT_POLL_TIME).await;

    // And we expect to receive some capabilities from the mock.
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));
}

/// Tests whether the task tolerates missing capabilities (model 702)
#[tokio::test]
async fn tolerates_missing_capabilities() {
    // Setup
    let (_mock, task, _input_ch, mut output_ch) = setup(Some(&[1, 701, 703])).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect to have received a status from the mock, but no capabilities.
    let all_events = collect_all(&mut output_ch).await;
    assert!(!all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::StatePolled(Some(_status), Some(_settings), Some(_metering)),
    )));

    // And the task should still be active.
    assert!(!task.is_finished());
}

/// Tests whether the task tolerates missing meter readings (model 701)
#[tokio::test]
async fn tolerates_missing_metering() {
    // Setup
    let (_mock, task, _input_ch, mut output_ch) = setup(Some(&[1, 702, 703])).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect to have received a status from the mock, but no capabilities.
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));
    assert!(!all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::StatePolled(_status, _settings, Some(_metering)),
    )));

    // And the task should still be active.
    assert!(!task.is_finished());
}

/// Tests whether the task tolerates missing control parameter locations
#[tokio::test]
async fn tolerates_missing_control_parameters() {
    // Setup
    let (_mock, task, input_ch, mut output_ch) = setup(Some(&[1, 701, 702])).await;

    // Provide a parameters command
    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                es: Some(model703::Es::Enabled),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect to have received a polled state but no errors
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::StatePolled(_status, _settings, Some(_metering)),
    )));

    // And the task should still be active.
    assert!(!task.is_finished());
}

/////
// Helpers

/// Sets up the modbus server mock with default values, and starts the modbus
/// connection task.
async fn setup(
    enabled_models: Option<&[u32]>,
) -> (
    SunSpecMock,
    JoinHandle<Result<()>>,
    mpsc::Sender<modbus_connection::Command>,
    async_broadcast::Receiver<modbus_connection::Event>,
) {
    setup_with(enabled_models, |_| {}).await
}

/// Like setup, but also runs the callback `seed` against the mock before the
/// server starts, so a test can vary what the device advertises without a race
/// condition.
async fn setup_with(
    enabled_models: Option<&[u32]>,
    seed: impl FnOnce(&SunSpecMock),
) -> (
    SunSpecMock,
    JoinHandle<Result<()>>,
    mpsc::Sender<modbus_connection::Command>,
    async_broadcast::Receiver<modbus_connection::Event>,
) {
    let mut mock = SunSpecMock::new(enabled_models)
        .await
        .expect("Couldn't create mock modbus server");
    seed(&mock);
    mock.start()
        .await
        .expect("Couldn't start mock modbus server");

    let (input_tx, input_rx) = mpsc::channel(10);
    let (output_tx, output_rx) = async_broadcast::broadcast(10);
    let task = task::spawn(modbus_connection::task(
        output_tx,
        input_rx,
        Transport::Tcp(mock.addr.unwrap()),
        1,
    ));

    (mock, task, input_tx, output_rx)
}

async fn collect_all<T: Clone>(output_ch: &mut async_broadcast::Receiver<T>) -> Vec<T> {
    let mut events = Vec::new();
    loop {
        match time::timeout(WAIT_TIME, output_ch.recv()).await {
            Err(_) => break,
            Ok(Err(_)) => panic!("Recv error"),
            Ok(Ok(event)) => events.push(event),
        }
    }
    events
}
