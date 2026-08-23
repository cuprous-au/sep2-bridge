// Tests for the scheduler task on its own, can it produce the right events given a series of commands.

use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use sep2_bridge::{Result, scheduler, sep2_connection::Sep2ResourceEvent};
use sep2_common::packages::{
    der::{
        ActivePower, DERControl, DERControlBase, DERControlList, DERProgram, DERProgramList,
        DefaultDERControl,
    },
    edev::{EndDevice, EndDeviceList},
    fsa::{FunctionSetAssignments, FunctionSetAssignmentsList},
    identification::{Link, ListLink, ResponseRequired, ResponseStatus},
    primitives::{HexBinary160, Int16, Int64, Uint16, Uint32},
    types::{DateTimeInterval, MRIDType, PowerOfTenMultiplierType, PrimacyType, SFDIType},
};
use tokio::{sync::mpsc, task::JoinHandle, time};

// Generic timeout for waits on the scheduler task.
const TIMEOUT: Duration = Duration::from_millis(200);

/// Tests whether the scheduler will request all of the required polls to be performed given a set of resources.
#[tokio::test]
async fn requests_polling() {
    // This test mocks the sep2_connection task by responding with a resource if a poll is requested.

    let (_task, input_ch, mut output_ch) = prepare_scheduler_task().await;

    // Fill map of responses for hrefs.
    let (resource_map, _) = resource_map_no_controls();

    // Send first resource
    input_ch
        .send(scheduler::Command::ResourceUpdated(
            resource_map[HREF_EDEVL].clone(),
        ))
        .await
        .expect("Send failure");

    // Additional poll requests that will be received but we won't respond with a resource.
    let additional_hrefs = [HREF_EDEV, HREF_FSA, HREF_DERP, HREF_DDERC];

    // Expect that each of the resources was queried.
    let mut resources_to_be_queried: HashSet<_> = resource_map
        .keys()
        .cloned()
        .filter(|k| *k != HREF_EDEVL)
        .chain(additional_hrefs.iter().cloned())
        .collect();

    while !resources_to_be_queried.is_empty() {
        let event = time::timeout(TIMEOUT, output_ch.recv())
            .await
            .expect("Scheduler stopped sending events")
            .expect("Recv error");

        match event {
            scheduler::Event::LinkAdded { href, kind: _ } => {
                if !resources_to_be_queried.remove(href.as_str()) {
                    panic!("Resource {href} queried twice or should never be queried.");
                }

                if !additional_hrefs.contains(&href.as_str()) {
                    let resource = resource_map
                        .get(href.as_str())
                        .unwrap_or_else(|| panic!("Unknown href: {}", href))
                        .clone();
                    input_ch
                        .send(scheduler::Command::ResourceUpdated(resource))
                        .await
                        .expect("Send failure");
                }
            }
            scheduler::Event::LinkRemoved { .. } => {
                panic!("Unexpected LinkRemoved");
            }
            scheduler::Event::ParametersChanged(_) => {
                // We expect at least one of these, ignore this one.
            }
            scheduler::Event::DERControlStatusChanged { .. } => {
                panic!("Unexpected DERControlStatusChanged");
            }
        }
    }

    // Expect that no other resources were queried.
    time::timeout(TIMEOUT, output_ch.recv())
        .await
        .expect_err("Additional event received from scheduler");
}

/// Tests whether the scheduler produces a relevant set of parameters that indicate the current desired state of the device.
#[tokio::test]
async fn emits_parameters() {
    let (_task, input_ch, mut output_ch) = prepare_scheduler_task().await;

    // Send all resources
    let (ordered_resources, expected_number_of_events) = resource_map_no_controls();
    for resource in ordered_resources.values() {
        input_ch
            .send(scheduler::Command::ResourceUpdated(resource.clone()))
            .await
            .expect("Send failure")
    }

    // Flush all expected events out from the queue.
    collect_n_events(&mut output_ch, expected_number_of_events).await;

    // Send a change to the default controls
    let set_grad_w = Uint16(19);
    let set_es_high_volt = Int16(123);
    let dderc = Sep2ResourceEvent::DefaultDERControl(Arc::new(DefaultDERControl {
        href: Some(HREF_DDERC.into()),

        set_grad_w: Some(set_grad_w),
        set_es_high_volt: Some(set_es_high_volt),
        ..Default::default()
    }));
    input_ch
        .send(scheduler::Command::ResourceUpdated(dderc))
        .await
        .expect("Send failure");

    // Extract the parameters emitted.
    let event = time::timeout(TIMEOUT, output_ch.recv())
        .await
        .expect("No events from scheduler")
        .expect("Recv error");
    match event {
        scheduler::Event::ParametersChanged(parameters) => {
            assert_eq!(parameters.num_active(), 2);
            assert_eq!(parameters.set_grad_w, Some(set_grad_w));
            assert_eq!(parameters.set_es_high_volt, Some(set_es_high_volt));
        }
        event => {
            panic!("Unexpected event {:?} from scheduler", event);
        }
    }
}

/// Tests whether the scheduler will produce a schedule at the right time.
#[tokio::test]
async fn produces_schedule_on_time() {
    // Set up resources for scheduler without a control
    let (_task, input_ch, mut output_ch) = prepare_scheduler_task().await;

    // Send all resources
    let (ordered_resources, expected_number_of_events) = resource_map_no_controls();
    for resource in ordered_resources.values() {
        input_ch
            .send(scheduler::Command::ResourceUpdated(resource.clone()))
            .await
            .expect("Send failure")
    }

    // Flush all expected events out from the queue.
    collect_n_events(&mut output_ch, expected_number_of_events).await;

    // Provide a control which should start in 3s and last for 2s. However, we
    // can only specify a control's time to the nearest second, so round "now".
    let now = DateTime::from_timestamp_secs(Utc::now().timestamp()).unwrap();
    let start_delay = Duration::from_secs(3);
    let duration = Duration::from_secs(2);
    let start_time = now + start_delay;
    let end_time = start_time + duration;
    let tolerance = Duration::from_millis(100);

    let derc_reply_to = String::from("/reply/here");
    let derc = DERControl {
        href: Some(HREF_DERC_1.into()),
        mrid: MRIDType(42),
        reply_to: Some(derc_reply_to.clone()),
        response_required: Some(ResponseRequired::all()),

        der_control_base: DERControlBase {
            op_mod_connect: Some(true),
            op_mod_exp_lim_w: Some(ActivePower {
                multiplier: PowerOfTenMultiplierType::None,
                value: Int16(42),
            }),
            ..Default::default()
        },

        interval: DateTimeInterval {
            start: Int64(start_time.timestamp()),
            duration: Uint32(u32::try_from(duration.as_secs()).unwrap()),
        },
        ..Default::default()
    };
    let dercl = Sep2ResourceEvent::DERControlList(Arc::new(DERControlList {
        href: Some(HREF_DERCL.into()),
        der_control: vec![derc.clone()],

        all: Uint32(1),
        results: Uint32(1),

        ..Default::default()
    }));
    input_ch
        .send(scheduler::Command::ResourceUpdated(dercl))
        .await
        .expect("Send failure");
    // Swallow the LinkAdded event
    assert!(matches!(
        time::timeout(TIMEOUT, output_ch.recv())
            .await
            .expect("Timeout")
            .expect("Recv failure"),
        scheduler::Event::LinkAdded { .. }
    ));
    // And check for the expected EventReceived notification.
    assert!(matches!(
        time::timeout(TIMEOUT, output_ch.recv())
            .await
            .expect("Timeout")
            .expect("Recv failure"),
        scheduler::Event::DERControlStatusChanged { subject, status, reply_to }
            if subject == derc.mrid
            && status == ResponseStatus::EventReceived
            && reply_to == derc_reply_to
    ));

    // Wait for parameters to be emitted, expect the time to be within 100ms.
    let events = time::timeout(start_delay + TIMEOUT, collect_n_events(&mut output_ch, 2))
        .await
        .expect("Scheduler too late");
    let actual_start_time = Utc::now();
    // We expect one event with the parameters, and one event with the control started notification.
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| match event {
        scheduler::Event::ParametersChanged(parameters) => {
            assert_eq!(parameters.base.op_mod_connect, Some(true));
            true
        }
        _ => false,
    }));
    assert!(events.iter().any(|event| match event {
        scheduler::Event::DERControlStatusChanged {
            subject,
            status,
            reply_to,
        } => {
            assert_eq!(subject, &derc.mrid);
            assert_eq!(status, &ResponseStatus::EventStarted);
            assert_eq!(reply_to, &derc_reply_to);
            true
        }
        _ => false,
    }));
    // And the start time should be accurate to within 100ms
    assert!(
        actual_start_time >= (start_time - tolerance)
            && actual_start_time <= (start_time + tolerance),
        "start time ({}) different than expected time ({})",
        actual_start_time,
        start_time,
    );

    // Now wait for end of the control
    let events = time::timeout(duration + TIMEOUT, collect_n_events(&mut output_ch, 2))
        .await
        .expect("Scheduler too late");
    let actual_end_time = Utc::now();
    // We expect one event with the parameters, and one event with the control completed notification.
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| match event {
        scheduler::Event::ParametersChanged(parameters) => {
            assert_eq!(parameters.base.op_mod_connect, None);
            true
        }
        _ => false,
    }));
    assert!(events.iter().any(|event| match event {
        scheduler::Event::DERControlStatusChanged {
            subject,
            status,
            reply_to,
        } => {
            assert_eq!(subject, &derc.mrid);
            assert_eq!(status, &ResponseStatus::EventCompleted);
            assert_eq!(reply_to, derc.reply_to.as_ref().unwrap());
            true
        }
        _ => false,
    }));
    // And the end time should be accurate to within 100ms
    assert!(actual_end_time >= (end_time - tolerance) && actual_end_time <= (end_time + tolerance));
}

/////
// Helpers

fn mock_lfdi() -> HexBinary160 {
    HexBinary160::from_str("00112233").expect("Invalid LFDI in test")
}

async fn prepare_scheduler_task() -> (
    JoinHandle<Result<()>>,
    mpsc::Sender<scheduler::Command>,
    async_broadcast::Receiver<scheduler::Event>,
) {
    // Start the scheduler task.
    let (scheduler_input_tx, scheduler_input_rx) = mpsc::channel(10);
    let (scheduler_output_tx, scheduler_output_rx) = async_broadcast::broadcast(10);
    let task = tokio::spawn(scheduler::task(
        scheduler_output_tx,
        scheduler_input_rx,
        scheduler_input_tx.clone(),
        mock_lfdi(),
    ));

    (task, scheduler_input_tx, scheduler_output_rx)
}

const HREF_EDEVL: &str = "/edev";
const HREF_EDEV: &str = "/edev/1";
const HREF_FSAL: &str = "/edev/1/fsa";
const HREF_FSA: &str = "/edev/1/fsa/1";
const HREF_DERPL: &str = "/edev/1/fsa/1/derp";
const HREF_DERP: &str = "/edev/1/derp/1";
const HREF_DDERC: &str = "/edev/1/derp/1/dderc";
const HREF_DERCL: &str = "/edev/1/derp/1/derc";
const HREF_DERC_1: &str = "/edev/1/derp/1/derc/1";

/// Returns a resource map and the expected number of events that would be
/// generated from applying all resources.
fn resource_map_no_controls() -> (HashMap<&'static str, Sep2ResourceEvent>, usize) {
    let mut resource_map: HashMap<&'static str, Sep2ResourceEvent> = HashMap::new();

    let lfdi = mock_lfdi();
    let sfdi = SFDIType::new(42).expect("Invalid SFDI in test");
    resource_map.insert(
        HREF_EDEVL,
        Sep2ResourceEvent::EndDeviceList(Arc::new(EndDeviceList {
            href: Some(HREF_EDEVL.into()),

            end_device: vec![EndDevice {
                lfdi: Some(lfdi),
                sfdi,
                href: Some(HREF_EDEV.into()),
                function_set_assignments_list_link: Some(ListLink {
                    href: HREF_FSAL.into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        })),
    );

    resource_map.insert(
        HREF_FSAL,
        Sep2ResourceEvent::FunctionSetAssignmentsList(Arc::new(FunctionSetAssignmentsList {
            href: Some(HREF_FSAL.into()),

            function_set_assignments: vec![FunctionSetAssignments {
                href: Some(HREF_FSA.into()),
                der_program_list_link: Some(ListLink {
                    href: HREF_DERPL.into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        })),
    );

    resource_map.insert(
        HREF_DERPL,
        Sep2ResourceEvent::DERProgramList(Arc::new(DERProgramList {
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
        })),
    );

    resource_map.insert(
        HREF_DERCL,
        Sep2ResourceEvent::DERControlList(Arc::new(DERControlList {
            href: Some(HREF_DERCL.into()),
            der_control: vec![],

            all: Uint32(0),
            results: Uint32(0),

            ..Default::default()
        })),
    );
    resource_map.insert(
        HREF_DDERC,
        Sep2ResourceEvent::DefaultDERControl(Arc::new(DefaultDERControl {
            href: Some(HREF_DDERC.into()),

            set_grad_w: Some(Uint16(42)),
            ..Default::default()
        })),
    );

    // Hard-coding this number: it will need adjustment if the resources above
    // are modified or added to.
    let expected_number_of_events = 9;

    (resource_map, expected_number_of_events)
}

async fn collect_n_events<T: Clone>(
    output_ch: &mut async_broadcast::Receiver<T>,
    n: usize,
) -> Vec<T> {
    let mut events = Vec::new();

    while events.len() < n {
        let event = time::timeout(Duration::from_secs(10), output_ch.recv())
            .await
            .expect("Timeout in collect_n_events")
            .expect("Recv failure");

        events.push(event);
    }

    events
}
