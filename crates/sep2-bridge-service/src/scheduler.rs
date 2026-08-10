use chrono::{DateTime, Utc};
use sep2_common::packages::{
    der::DERControl,
    identification::{ResponseRequired, ResponseStatus},
    primitives::{HexBinary160, Int64},
    types::MRIDType,
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle, time};

use crate::{Error, ResourceKind, Result, sep2_connection::Sep2ResourceEvent};

mod attributes;
mod sep2_model;

pub use attributes::ControlAttributes;
use sep2_model::{ControlRef, Sep2Model};

pub enum Command {
    /// Recalculate the parameters and find the next schedule event.
    NextSchedule,

    /// Apply a new incoming resource definition into the model.
    ResourceUpdated(Sep2ResourceEvent),
}

/// Events emitted when changes made to the model require external effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A new link has appeared in the model.
    LinkAdded { href: String, kind: ResourceKind },

    /// A link has disappeared from the model.
    LinkRemoved { href: String, kind: ResourceKind },

    /// The parameters derived from the model have changed.
    ParametersChanged(Arc<ControlAttributes>),

    /// A control's status has changed.
    DERControlStatusChanged {
        subject: MRIDType,
        status: ResponseStatus,
        reply_to: String,
    },
}

/// The scheduler loop.
///
/// Receives commands on `input_ch` and emits events on `output_ch`. The
/// EndDevice to be managed is known by the `device_lfdi`.
///
/// Requires the input channel sender `input_ch_tx` as well to manage scheduling
/// wake up commands.
pub async fn task(
    output_ch: async_broadcast::Sender<Event>,
    mut input_ch: mpsc::Receiver<Command>,
    input_ch_tx: mpsc::Sender<Command>,
    device_lfdi: HexBinary160,
) -> Result<()> {
    // Initialisation
    let mut model = load_persisted_state();
    let mut wait_task: Option<JoinHandle<_>> = None;
    let mut prior_next_events = Vec::new();
    let mut prior_next_scheduler_time = None;
    let mut prior_parameters = ControlAttributes::default();

    while let Some(command) = input_ch.recv().await {
        match command {
            Command::NextSchedule => {
                log::trace!("Next schedule trigger");
                // Nothing to do here, we are simply waking up to perform scheduling.
            }
            Command::ResourceUpdated(resource) => {
                log::trace!("Applying model update");
                // Apply the model update, and broadcast out any events generated.
                let events = model.apply_update(resource);

                for event in events {
                    output_ch
                        .broadcast(event)
                        .await
                        .map_err(|_| Error::ChannelClosed)?;
                }
                // Continue through to scheduling calculations.
            }
        };

        let now = Utc::now();

        let cur_parameters = match calc_parameters(&model, device_lfdi, now) {
            Ok(x) => x,
            Err(err) => {
                // We assume this is not a fatal error but will recover once a
                // model update is applied.
                log::warn!("Scheduler was unable to calculate parameters: {}", err);
                continue;
            }
        };

        // If the parameters changed send it out.
        if cur_parameters != prior_parameters {
            log::trace!(
                "Found new parameters with {} attributes",
                cur_parameters.num_active()
            );
            output_ch
                .broadcast(cur_parameters.clone().into())
                .await
                .map_err(|_| Error::ChannelClosed)?;
            prior_parameters = cur_parameters;
        }

        // Determine when the model state will next change.
        let (next_scheduler_time, next_events) =
            match calc_next_schedule_time(&model, device_lfdi, now) {
                Ok(x) => x,
                Err(err) => {
                    log::warn!(
                        "Scheduler was unable to calculate next scheduling time: {}",
                        err
                    );
                    continue;
                }
            };

        // If the scheduler hasn't predicted anything new, don't update the task.
        // Note: technically an ordering in the events Vec is an unnecessary change
        // here. However, it doesn't hurt to recalculate in that scenario.
        if next_scheduler_time == prior_next_scheduler_time && next_events == prior_next_events {
            continue;
        }

        // Kill off any previous wait task.
        if let Some(handle) = wait_task.take() {
            handle.abort();
            if let Err(e) = handle.await {
                log::warn!("Joining aborted scheduler task returned an error: {e}");
            }
        }

        // Replace the wait task with a new one.
        wait_task = Some(tokio::spawn(scheduler_action_task(
            next_scheduler_time,
            next_events.clone(),
            output_ch.clone(),
            input_ch_tx.clone(),
        )));

        prior_next_scheduler_time = next_scheduler_time;
        prior_next_events = next_events;
    }

    log::info!("Input channel closed, stopping scheduler loop");
    Ok(())
}

fn load_persisted_state() -> Sep2Model {
    // FIXME: This is currently a stub acting as if there is no persisted state
    // and using a fresh slate.
    Sep2Model::default()
}

/// Determine the new parameters given a time `now`.
fn calc_parameters(
    model: &Sep2Model,
    device_lfdi: HexBinary160,
    now: DateTime<Utc>,
) -> Result<ControlAttributes> {
    let i64_now = Int64(now.timestamp());
    let controls = model.all_controls_for_device(device_lfdi, i64_now)?;
    calc_parameters_for_controls(controls, i64_now)
}

fn calc_parameters_for_controls(
    controls: Vec<ControlRef>,
    i64_now: Int64,
) -> Result<ControlAttributes> {
    let parameters = controls
        .into_iter()
        // Filter out any controls that are not active right now
        .filter(|control| match control {
            ControlRef::Default(_) => true,
            ControlRef::Scheduled(control) => {
                control.start_time.0 <= i64_now.0 && control.end_time().0 > i64_now.0
            }
        })
        // Convert them to attributes
        .map(|control| match control {
            ControlRef::Default(default) => default.into(),
            ControlRef::Scheduled(control) => (&control.der_control).into(),
        })
        // And project them down in the order given
        .fold(ControlAttributes::default(), |a, b| a.overlay_on(b));

    Ok(parameters)
}

/// Return the reply_to address if it is given on the control and if the
/// response is stated as being required by the control.
fn reply_to_if_required(
    der_control: &DERControl,
    response_type: ResponseRequired,
) -> Option<String> {
    der_control.response_required.and_then(|rr| {
        rr.contains(response_type)
            .then(|| der_control.reply_to.clone())
            .flatten()
    })
}

/// Determines the next time for an event that might cause a change in the
/// parameters. Does not guarantee that an event will cause a change and expects
/// the caller to calculate the parameters at this new time when it is reached.
fn calc_next_schedule_time(
    model: &Sep2Model,
    device_lfdi: HexBinary160,
    now: DateTime<Utc>,
) -> Result<(Option<DateTime<Utc>>, Vec<Event>)> {
    let i64_now = Int64(now.timestamp());
    let controls = model.all_controls_for_device(device_lfdi, i64_now)?;

    calc_next_schedule_time_for_controls(controls, i64_now)
}

fn calc_next_schedule_time_for_controls(
    controls: Vec<ControlRef>,
    i64_now: Int64,
) -> Result<(Option<DateTime<Utc>>, Vec<Event>)> {
    // For debugging purposes.
    let num_controls = controls.len();
    let mut num_active_controls = 0;

    // Find the next control event that is happening in the future.
    // Not being fussy here - we might find events which won't change the
    // parameters because they are superseeded, but easier to be overeager than
    // carefully figuring out what might happen in the future.
    let all_future_events: Vec<(Int64, Option<Event>)> = controls
        .into_iter()
        .filter_map(|control| match control {
            ControlRef::Scheduled(control) => Some(vec![
                (
                    control.start_time,
                    // Add an EventStarted response if required.
                    reply_to_if_required(&control.der_control, ResponseRequired::SpecificResponse)
                        .map(|reply_to| Event::DERControlStatusChanged {
                            subject: control.der_control.mrid,
                            status: ResponseStatus::EventStarted,
                            reply_to,
                        }),
                ),
                (
                    control.end_time(),
                    // Add an EventCompleted response if required.
                    reply_to_if_required(&control.der_control, ResponseRequired::SpecificResponse)
                        .map(|reply_to| Event::DERControlStatusChanged {
                            subject: control.der_control.mrid,
                            status: ResponseStatus::EventCompleted,
                            reply_to,
                        }),
                ),
            ]),
            ControlRef::Default(_) => None,
        })
        .inspect(|_| num_active_controls += 1)
        .flatten()
        // Note > and not >= because of inclusive min time interval boundary.
        .filter(|(time, _)| time.0 > i64_now.0)
        .collect();

    let earliest_time = all_future_events
        .iter()
        .map(|(time, _)| time)
        .min()
        .cloned();

    let next_events = all_future_events
        .into_iter()
        .filter(|(time, _)| Some(*time) == earliest_time)
        .filter_map(|(_, action)| action)
        .collect();

    let nice_time = earliest_time.and_then(|time| DateTime::from_timestamp(time.0, 0));
    log::trace!(
        "Schedule next time: {}, number of controls (active): {} ({})",
        nice_time.map_or(String::from("None"), |time| time
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()),
        num_controls,
        num_active_controls
    );

    // Convert the time to DateTime for the caller.
    let earliest_time = earliest_time.map(datetime_from_int64);

    Ok((earliest_time, next_events))
}

fn datetime_from_int64(time: Int64) -> DateTime<Utc> {
    DateTime::from_timestamp_secs(time.0).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Waits until ready to send out events and wake up the scheduler.
///
/// If no future event is known, then this task will complete without waking up
/// the scheduler.
async fn scheduler_action_task(
    next_event_time: Option<DateTime<Utc>>,
    next_events: Vec<Event>,
    output_ch: async_broadcast::Sender<Event>,
    input_ch_tx: mpsc::Sender<Command>,
) -> Result<()> {
    let Some(next_event_time) = next_event_time else {
        log::trace!("No next time, scheduler task is exiting");
        return Ok(());
    };

    // TODO: For long waits, possibly better to wait in small increments to avoid clock drift.
    let dt = next_event_time
        .signed_duration_since(Utc::now())
        .num_seconds();
    if dt > 0 {
        log::trace!("Sleeping for {dt}s");
        time::sleep(Duration::from_secs(dt as u64)).await;
    }

    // Send out the events we predicted.
    for event in next_events {
        output_ch
            .broadcast(event)
            .await
            .map_err(|_| Error::ChannelClosed)?;
    }

    // Wake scheduler back up.
    input_ch_tx
        .send(Command::NextSchedule)
        .await
        .map_err(|_| Error::ChannelClosed)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use sep2_common::packages::{
        der::{ActivePower, DERControlBase, DefaultDERControl},
        objects::{EventStatus, EventStatusType},
        primitives::{Int16, Uint32},
        types::PowerOfTenMultiplierType,
    };

    use crate::scheduler::sep2_model::ScheduledControl;

    use super::*;

    #[test]
    fn predicts_no_time_for_only_defaults() {
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);

        let (time, events) =
            calc_next_schedule_time_for_controls(ordered_controls, data.time_only_defaults)
                .unwrap();

        assert_eq!(time, None);
        assert!(events.is_empty());
    }

    #[test]
    fn predicts_earliest_time() {
        // Should predict the start of the second events.
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let (time, _) =
            calc_next_schedule_time_for_controls(ordered_controls, data.time_first_active).unwrap();
        assert_eq!(time, Some(datetime_from_int64(data.controls[1].start_time)));

        // Should predict the end of the first event.
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let (time, _) =
            calc_next_schedule_time_for_controls(ordered_controls, data.time_all_active).unwrap();
        assert_eq!(time, Some(datetime_from_int64(data.controls[0].end_time())));

        // Should predict the end of the second events.
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let (time, _) =
            calc_next_schedule_time_for_controls(ordered_controls, data.time_second_active)
                .unwrap();
        assert_eq!(time, Some(datetime_from_int64(data.controls[1].end_time())));
    }

    #[test]
    fn predicts_simultaneous_events() {
        // Should predict 2 events starting as the 2nd and 3rd events begin and end together.

        // The starting events
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let (_, events) =
            calc_next_schedule_time_for_controls(ordered_controls, data.time_first_active).unwrap();
        assert_eq!(events.len(), 2);

        // The ending events
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let (_, events) =
            calc_next_schedule_time_for_controls(ordered_controls, data.time_second_active)
                .unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parameters_includes_only_active_controls() {
        // Calculating when there are only defaults
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let parameters =
            calc_parameters_for_controls(ordered_controls, data.time_only_defaults).unwrap();
        assert_eq!(parameters.base.op_mod_connect, Some(true));
        assert_eq!(
            parameters.base.op_mod_imp_lim_w,
            data.defaults[1].der_control_base.op_mod_imp_lim_w
        );
        assert_eq!(parameters.num_active(), 2);

        // Calculating when only the first is active
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let parameters =
            calc_parameters_for_controls(ordered_controls, data.time_first_active).unwrap();
        assert_eq!(parameters.base.op_mod_connect, Some(true));
        assert_eq!(
            parameters.base.op_mod_imp_lim_w,
            data.controls[0]
                .der_control
                .der_control_base
                .op_mod_imp_lim_w
        );
        assert_eq!(
            parameters.base.op_mod_gen_lim_w,
            data.controls[0]
                .der_control
                .der_control_base
                .op_mod_gen_lim_w
        );
        assert_eq!(parameters.num_active(), 3);

        // Calculating when all are active
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let parameters =
            calc_parameters_for_controls(ordered_controls, data.time_all_active).unwrap();
        assert_eq!(parameters.base.op_mod_connect, Some(true));
        assert_eq!(
            parameters.base.op_mod_imp_lim_w,
            data.controls[0]
                .der_control
                .der_control_base
                .op_mod_imp_lim_w
        );
        assert_eq!(
            parameters.base.op_mod_gen_lim_w,
            data.controls[0]
                .der_control
                .der_control_base
                .op_mod_gen_lim_w
        );
        assert_eq!(
            parameters.base.op_mod_load_lim_w,
            data.controls[1]
                .der_control
                .der_control_base
                .op_mod_load_lim_w
        );
        assert_eq!(
            parameters.base.op_mod_target_w,
            data.controls[2]
                .der_control
                .der_control_base
                .op_mod_target_w
        );
        assert_eq!(parameters.num_active(), 5);

        // Calculating when only the second are active
        let data = mock_controls();
        let ordered_controls = mock_ordered_controls(&data);
        let parameters =
            calc_parameters_for_controls(ordered_controls, data.time_second_active).unwrap();
        assert_eq!(parameters.base.op_mod_connect, Some(true));
        assert_eq!(
            parameters.base.op_mod_imp_lim_w,
            data.controls[1]
                .der_control
                .der_control_base
                .op_mod_imp_lim_w
        );
        assert_eq!(
            parameters.base.op_mod_load_lim_w,
            data.controls[1]
                .der_control
                .der_control_base
                .op_mod_load_lim_w
        );
        assert_eq!(
            parameters.base.op_mod_target_w,
            data.controls[2]
                .der_control
                .der_control_base
                .op_mod_target_w
        );
        assert_eq!(parameters.num_active(), 4);
    }

    #[test]
    fn parameters_combines_controls() {}

    struct MockControls {
        controls: Vec<ScheduledControl>,
        defaults: Vec<DefaultDERControl>,

        time_only_defaults: Int64,
        time_first_active: Int64,
        time_all_active: Int64,
        time_second_active: Int64,
    }

    fn mock_controls() -> MockControls {
        let now = Utc::now().timestamp();

        let default2_power = ActivePower {
            value: Int16(43),
            multiplier: PowerOfTenMultiplierType::Kilo,
        };
        let first_power = ActivePower {
            value: Int16(1),
            multiplier: PowerOfTenMultiplierType::Deca,
        };
        let second_power = ActivePower {
            value: Int16(2),
            multiplier: PowerOfTenMultiplierType::Hecto,
        };
        let third_power = ActivePower {
            value: Int16(3),
            multiplier: PowerOfTenMultiplierType::Kilo,
        };

        let der_control_template = DERControl {
            event_status: EventStatus {
                current_status: EventStatusType::Scheduled,
                ..Default::default()
            },
            reply_to: Some(String::from("")),
            response_required: Some(ResponseRequired::all()),
            ..Default::default()
        };
        // Create some controls and defaults with a mix of which attribute is assigned.
        let controls = vec![
            ScheduledControl {
                der_control: DERControl {
                    der_control_base: DERControlBase {
                        op_mod_imp_lim_w: Some(first_power.clone()),
                        op_mod_gen_lim_w: Some(first_power),
                        ..Default::default()
                    },
                    ..der_control_template.clone()
                },

                start_time: Int64(now + 1000),
                duration: Uint32(1000),
            },
            ScheduledControl {
                der_control: DERControl {
                    der_control_base: DERControlBase {
                        op_mod_imp_lim_w: Some(second_power.clone()),
                        op_mod_load_lim_w: Some(second_power),
                        ..Default::default()
                    },
                    ..der_control_template.clone()
                },

                start_time: Int64(now + 1500),
                duration: Uint32(1000),
            },
            ScheduledControl {
                der_control: DERControl {
                    der_control_base: DERControlBase {
                        op_mod_imp_lim_w: Some(third_power.clone()),
                        op_mod_target_w: Some(third_power),
                        ..Default::default()
                    },
                    ..der_control_template.clone()
                },

                start_time: Int64(now + 1500),
                duration: Uint32(1000),
            },
        ];

        let defaults = vec![
            DefaultDERControl {
                der_control_base: DERControlBase {
                    op_mod_connect: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
            DefaultDERControl {
                der_control_base: DERControlBase {
                    op_mod_connect: Some(false),
                    op_mod_imp_lim_w: Some(default2_power),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];

        let time_only_defaults = Int64(now + 3000);
        let time_first_active = Int64(now + 1250);
        let time_all_active = Int64(now + 1750);
        let time_second_active = Int64(now + 2250);

        MockControls {
            controls,
            defaults,

            time_only_defaults,
            time_first_active,
            time_all_active,
            time_second_active,
        }
    }

    fn mock_ordered_controls<'a>(data: &'a MockControls) -> Vec<ControlRef<'a>> {
        vec![
            ControlRef::Scheduled(&data.controls[0]),
            ControlRef::Default(&data.defaults[0]),
            ControlRef::Scheduled(&data.controls[1]),
            ControlRef::Scheduled(&data.controls[2]),
            ControlRef::Default(&data.defaults[1]),
        ]
    }
}
