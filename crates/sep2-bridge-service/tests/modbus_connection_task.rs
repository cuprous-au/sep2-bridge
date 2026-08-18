mod modbus_server_mock;

use std::time::Duration;

use modbus_server_mock::SunSpecMock;
use sep2_bridge::{
    Result,
    modbus_connection::{self, Capabilities, Settings, Status, Transport},
};
use sunspec::models::{model701, model703};
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
    let (mock, _task, input_ch, _output_ch) = setup().await;

    // Provide a parameters command
    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                es: Some(model703::Es::Enabled),
                esvhi: None,
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

/// Tests the task reads and emits the device capabilities, status and state.
#[tokio::test]
async fn reads_device_state() {
    // Setup
    let (mock, _task, _input_ch, mut output_ch) = setup().await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect the default parameters from the mock.
    let all_events = collect_all(&mut output_ch).await;

    let expected_w_max_rtg = mock.get_value::<Option<u16>>("model702::W_MAX_RTG");
    let expected_esv_hi = mock.get_value::<Option<u16>>("model703::ESV_HI");
    let expected_st = mock.get_value::<Option<model701::St>>("model701::ST");
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
                Status {
                    st, ..
                },
                Settings {
                    esv_hi
                }
            ) if esv_hi == &expected_esv_hi && st == &expected_st
            ))
    );
}

/// Tests the reconnection of the task when the server goes down.
#[tokio::test]
async fn reconnects() {
    // Setup
    let (mut mock, _task, _input_ch, mut output_ch) = setup().await;

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

/////
// Helpers

/// Sets up the modbus server mock and starts the modbus connection task.
async fn setup() -> (
    SunSpecMock,
    JoinHandle<Result<()>>,
    mpsc::Sender<modbus_connection::Command>,
    async_broadcast::Receiver<modbus_connection::Event>,
) {
    let mut mock = SunSpecMock::new()
        .await
        .expect("Couldn't create mock modbus server");
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
