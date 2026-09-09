// The internal representation caching the upstream state communicated over SEP2.

use chrono::Utc;
use rand::{RngExt, rngs::ThreadRng};
use sep2_common::{
    packages::{
        der::{
            DERControl, DERControlList, DERCurve, DERCurveList, DERProgram, DERProgramList,
            DefaultDERControl,
        },
        edev::EndDevice,
        fsa::{FunctionSetAssignments, FunctionSetAssignmentsList},
        identification::{Link, ListLink, ResponseRequired, ResponseStatus},
        objects::EventStatusType,
        primitives::{HexBinary160, Int64, Uint32},
        time::Time,
        types::{MRIDType, PrimacyType},
    },
    traits::{SEList, SEResource},
};
use std::{
    collections::{HashMap, hash_map::Entry},
    iter,
    sync::Arc,
};

use super::{Event, reply_to_if_required};
use crate::{ResourceKind, Result, sep2_connection::Sep2ResourceEvent};

/// A thin wrapper around DERControl which includes a realised interval.
/// This interval includes randomisation if requested from the server and is
/// done once to avoid statistical bias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledControl {
    // The wrapped data
    pub der_control: DERControl,

    // Realised start/duration of the potentially-randomised interval
    pub start_time: Int64,
    pub duration: Uint32,
}

impl ScheduledControl {
    pub fn end_time(&self) -> Int64 {
        Int64(self.start_time.0 + self.duration.0 as i64)
    }
}

/// Contains the ids of all items that are in a SEList, without
/// storing the rest of the details.
#[derive(Debug, Clone)]
struct MRIDList {
    pub items: Vec<MRIDType>,
    // These two are dead_code for now but are required for when poll rate update handling is needed.
    #[expect(dead_code)]
    pub poll_rate: Option<Uint32>,
    #[expect(dead_code)]
    pub href: String,
}

/// A reference to a DERControl (scheduled control) or a DefaultDERControl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlRef<'a> {
    Scheduled(&'a ScheduledControl),
    Default(&'a DefaultDERControl),
}

/// The data model built up from multiple requests to the SEP2 server.
#[derive(Debug, Default)]
pub struct Sep2Model {
    time: Time,

    // These are lists which we need only store their children's IDs.
    function_set_assignments_lists: HashMap<String, MRIDList>,
    program_lists: HashMap<String, MRIDList>,
    control_lists: HashMap<String, MRIDList>,
    curve_lists: HashMap<String, MRIDList>,

    // The individual resource definitions.
    end_devices: HashMap<String, EndDevice>,
    function_set_assignments: HashMap<MRIDType, FunctionSetAssignments>,
    programs: HashMap<MRIDType, DERProgram>,
    controls: HashMap<MRIDType, ScheduledControl>,
    curves: HashMap<MRIDType, DERCurve>,
    // Note that default controls are linked by href and not mrid.
    default_controls: HashMap<String, DefaultDERControl>,
}

impl Sep2Model {
    /// Takes a resource update from the SEP2 server and merges it into the model.
    /// Some changes caused by the merge are emitted as events to be broadcast to
    /// other tasks.
    pub fn apply_update(self: &mut Sep2Model, update: Sep2ResourceEvent) -> Vec<Event> {
        // TODO: Use a single rng rather than getting the thread rng each time.
        let mut rng = rand::rng();

        // Each of these updates returns a set of events that should be broadcast by the caller.
        match update {
            Sep2ResourceEvent::Time(time) => self.set_time(&time),
            Sep2ResourceEvent::EndDeviceList(edl) => {
                generic_log_list(&edl);

                edl.end_device
                    .iter()
                    .flat_map(|edev| self.set_end_device(edev))
                    .collect()

                // TODO: Identify old items to unsubscribe from. These should get removed
                // from the self which will emit more actions for us to act on.
            }
            Sep2ResourceEvent::FunctionSetAssignmentsList(fsal) => {
                generic_log_list(&fsal);
                self.set_function_set_assignments_list(&fsal)
            }
            Sep2ResourceEvent::DERProgramList(derpl) => {
                generic_log_list(&derpl);
                self.set_der_program_list(&derpl)
            }
            Sep2ResourceEvent::DERControlList(dercl) => {
                generic_log_list(&dercl);
                self.set_der_control_list(&dercl, &mut rng)
            }
            Sep2ResourceEvent::DERCurveList(curves) => {
                generic_log_list(&curves);
                self.set_der_curve_list(&curves)
            }
            Sep2ResourceEvent::DefaultDERControl(dderc) => self.set_default_der_control(&dderc),
        }
    }

    /// Insert/replace information about a single EndDevice in the model.
    pub fn set_end_device(self: &mut Sep2Model, incoming: &EndDevice) -> Vec<Event> {
        let href = safe_href(incoming);

        match self.end_devices.entry(href.clone()) {
            Entry::Occupied(mut entry) => {
                // Events for the FSAL link change
                let events = link_update_events(
                    &entry.get().function_set_assignments_list_link,
                    &incoming.function_set_assignments_list_link,
                    ResourceKind::FunctionSetAssignmentsList,
                );
                entry.insert(incoming.clone());

                events
            }

            Entry::Vacant(entry) => {
                // Event for this object
                let events = iter::once(Event::LinkAdded {
                    href: href.clone(),
                    kind: ResourceKind::EndDevice,
                })
                // Event for the FSAL
                .chain(link_update_events(
                    &None,
                    &incoming.function_set_assignments_list_link,
                    ResourceKind::FunctionSetAssignmentsList,
                ))
                .collect();
                entry.insert(incoming.clone());

                events
            }
        }
    }

    /// Upsert a FunctionSetAssignmentsList. Will upsert FunctionsSetAssignments too.
    pub fn set_function_set_assignments_list(
        self: &mut Sep2Model,
        incoming: &FunctionSetAssignmentsList,
    ) -> Vec<Event> {
        let href = safe_href(incoming);

        // TODO: Determine which entries have disappeared since last poll and
        // unsubscribe from them + delete them from the model.

        // Create a simplified list
        let list = MRIDList {
            items: incoming
                .function_set_assignments
                .iter()
                .map(|fsa| fsa.mrid)
                .collect(),
            poll_rate: incoming.poll_rate,
            href: href.clone(),
        };
        self.function_set_assignments_lists
            .insert(href.clone(), list);

        // Apply all individual FSAs and return their events
        incoming
            .function_set_assignments
            .iter()
            .flat_map(|fsa| self.set_function_set_assignments(fsa))
            .collect()
    }

    /// Upsert a FunctionSetAssignments.
    pub fn set_function_set_assignments(
        self: &mut Sep2Model,
        incoming: &FunctionSetAssignments,
    ) -> Vec<Event> {
        match self.function_set_assignments.entry(incoming.mrid) {
            Entry::Occupied(mut entry) => {
                // Events for the DERPL link change.
                let events = link_update_events(
                    &entry.get().der_program_list_link,
                    &incoming.der_program_list_link,
                    ResourceKind::DERProgramList,
                );

                entry.insert(incoming.clone());

                events
            }

            Entry::Vacant(entry) => {
                let events =
                // Subscribe to this href if it's present.
                incoming.href.iter().map(|href| Event::LinkAdded {
                    href: href.clone(),
                    kind: ResourceKind::FunctionSetAssignments,
                })
                // And the DEPProgramList link.
                .chain(
                    link_update_events(&None, &incoming
                    .der_program_list_link,
                    ResourceKind::DERProgramList
                    )
                ).collect();

                entry.insert(incoming.clone());

                events
            }
        }
    }

    /// Upsert a DERProgramList. Will upsert DERPrograms too.
    pub fn set_der_program_list(self: &mut Sep2Model, incoming: &DERProgramList) -> Vec<Event> {
        let href = safe_href(incoming);

        let list = MRIDList {
            items: incoming.der_program.iter().map(|derp| derp.mrid).collect(),
            poll_rate: incoming.poll_rate,
            href: href.clone(),
        };
        self.program_lists.insert(href.clone(), list);

        // Apply all individual DERPs and return their events
        incoming
            .der_program
            .iter()
            .flat_map(|derp| self.set_der_program(derp))
            .collect()
    }

    /// Upsert a DERProgram.
    pub fn set_der_program(self: &mut Sep2Model, incoming: &DERProgram) -> Vec<Event> {
        match self.programs.entry(incoming.mrid) {
            Entry::Occupied(mut entry) => {
                let events =
                // Check DERControlList for updates
                link_update_events(
                    &entry.get().der_control_list_link,
                    &incoming.der_control_list_link,
                    ResourceKind::DERControlList,
                )
                .into_iter()
                // Check DefaultDERControl for updates
                .chain(link_update_events(
                    &entry.get().default_der_control_link,
                    &incoming.default_der_control_link,
                    ResourceKind::DefaultDERControl,
                ))
                // Check DERCurveList for updates
                .chain(link_update_events(
                    &entry.get().der_curve_list_link,
                    &incoming.der_curve_list_link,
                    ResourceKind::DERCurveList,
                ))
                .collect();

                entry.insert(incoming.clone());

                events
            }

            Entry::Vacant(entry) => {
                let events =
                // Subscribe to this object
                    incoming.href.iter().map(|href| Event::LinkAdded {
                    href: href.clone(),
                    kind: ResourceKind::DERProgram,
                })
                // And its DERControlList link
                .chain(
                    link_update_events(&None, &incoming
                        .der_control_list_link,
                    ResourceKind::DERControlList
                    )
                )
                // And its DefaultDERControl link
                .chain(
                    link_update_events(&None, &incoming
                        .default_der_control_link,
                    ResourceKind::DefaultDERControl
                    )
                )
                // And its DERCurveList link
                .chain(
                    link_update_events(&None, &incoming.der_curve_list_link,
                        ResourceKind::DERCurveList
                    )
                )
                .collect();

                entry.insert(incoming.clone());

                events
            }
        }
    }

    /// Upsert a DERControlList. Will upsert DERControls too.
    pub fn set_der_control_list(
        self: &mut Sep2Model,
        incoming: &DERControlList,
        rng: &mut ThreadRng,
    ) -> Vec<Event> {
        let href = safe_href(incoming);

        let list = MRIDList {
            items: incoming.der_control.iter().map(|derp| derp.mrid).collect(),
            poll_rate: None,
            href: href.clone(),
        };
        self.control_lists.insert(href.clone(), list);

        // Apply all individual DERCs and return their events
        incoming
            .der_control
            .iter()
            .flat_map(|derc| self.set_der_control(derc, rng))
            .collect()
    }

    /// Upsert a DERControl.
    pub fn set_der_control(
        self: &mut Sep2Model,
        incoming: &DERControl,
        rng: &mut ThreadRng,
    ) -> Vec<Event> {
        let mrid = incoming.mrid;
        let href = safe_href(incoming);

        match self.controls.entry(mrid) {
            Entry::Occupied(entry) => {
                let prior_status = entry.get().der_control.event_status.current_status;
                let new_status = incoming.event_status.current_status;

                let maybe_event = match (prior_status, new_status) {
                    (x, y) if x == y => None,
                    (_, EventStatusType::Cancelled) => {
                        reply_to_if_required(incoming, ResponseRequired::SpecificResponse).map(
                            |reply_to| Event::DERControlStatusChanged {
                                subject: mrid,
                                status: ResponseStatus::EventCancelled,
                                reply_to: reply_to.clone(),
                            },
                        )
                    }
                    (_, EventStatusType::Superseded) => {
                        reply_to_if_required(incoming, ResponseRequired::SpecificResponse).map(
                            |reply_to| Event::DERControlStatusChanged {
                                subject: mrid,
                                status: ResponseStatus::EventSuperseded,
                                reply_to: reply_to.clone(),
                            },
                        )
                    }
                    _ => None,
                };

                // Update only the inner part, not the realised random time.
                entry.into_mut().der_control = incoming.clone();

                maybe_event.into_iter().collect()
            }
            Entry::Vacant(entry) => {
                // If required, randomize the time interval.
                let randomize_start = incoming.randomize_start.unwrap_or_default();
                let randomize_duration = incoming.randomize_duration.unwrap_or_default();

                let start_shift = rng.random_range(-randomize_start.get()..=randomize_start.get());
                let duration_shift =
                    rng.random_range(-randomize_duration.get()..=randomize_duration.get());

                let start_time = Int64(incoming.interval.start.0 + start_shift as i64);
                // Note: types force us to be overly cautious with overflow.
                let duration = Uint32(
                    (i64::from(incoming.interval.duration.0) + i64::from(duration_shift)).max(0)
                        as u32,
                );

                log::debug!(
                    "Randomised start_time: {} ({} + {}) and duration: {} ({} + {})",
                    start_time.0,
                    incoming.interval.start.0,
                    start_shift,
                    duration.0,
                    incoming.interval.duration.0,
                    duration_shift
                );

                let control = ScheduledControl {
                    der_control: incoming.clone(),
                    start_time,
                    duration,
                };
                entry.insert(control);

                // TODO: This value of "now" should be passed in.
                let now = Utc::now().timestamp();
                let end_time = start_time.0 + duration.0 as i64;
                let already_started = incoming.event_status.current_status
                    == EventStatusType::Active
                    && start_time.0 <= now
                    && end_time > now;

                // Event that there is a new link.
                iter::once(Event::LinkAdded {
                    href: href.clone(),
                    kind: ResourceKind::DERControl,
                })
                // Event for acknowledging this message has been received.
                .chain(
                    reply_to_if_required(incoming, ResponseRequired::MessageReceived).map(
                        |reply_to| Event::DERControlStatusChanged {
                            subject: mrid,
                            status: ResponseStatus::EventReceived,
                            reply_to: reply_to.clone(),
                        },
                    ),
                )
                // Event mentioning the control has already started.
                .chain(
                    already_started
                        .then(|| {
                            reply_to_if_required(incoming, ResponseRequired::SpecificResponse).map(
                                |reply_to| Event::DERControlStatusChanged {
                                    subject: mrid,
                                    status: ResponseStatus::EventStarted,
                                    reply_to: reply_to.clone(),
                                },
                            )
                        })
                        .flatten(),
                )
                .collect()
            }
        }
    }

    /// Upsert a DefaultDERControl.
    pub fn set_default_der_control(
        self: &mut Sep2Model,
        incoming: &DefaultDERControl,
    ) -> Vec<Event> {
        let href = safe_href(incoming);

        let events = if self.default_controls.contains_key(&href) {
            Vec::new()
        } else {
            vec![Event::LinkAdded {
                href: href.clone(),
                kind: ResourceKind::DefaultDERControl,
            }]
        };

        self.default_controls.insert(href.clone(), incoming.clone());

        events
    }

    /// Upsert a DERCurveList. Will upsert DERCurves too.
    pub fn set_der_curve_list(self: &mut Sep2Model, incoming: &DERCurveList) -> Vec<Event> {
        let href = safe_href(incoming);

        let list = MRIDList {
            items: incoming.der_curve.iter().map(|curve| curve.mrid).collect(),
            poll_rate: None,
            href: href.clone(),
        };
        self.curve_lists.insert(href.clone(), list);

        // Apply all individual DERCurves and return their events
        incoming
            .der_curve
            .iter()
            .flat_map(|curve| self.set_der_curve(curve))
            .collect()
    }

    /// Upsert a DERCurve.
    pub fn set_der_curve(self: &mut Sep2Model, incoming: &DERCurve) -> Vec<Event> {
        let mrid = incoming.mrid;
        let href = safe_href(incoming);

        match self.curves.entry(mrid) {
            Entry::Occupied(mut entry) => {
                entry.insert(incoming.clone());
                Vec::new()
            }
            Entry::Vacant(entry) => {
                entry.insert(incoming.clone());
                // Event that there is a new link.
                vec![Event::LinkAdded {
                    href: href.clone(),
                    kind: ResourceKind::DERCurve,
                }]
            }
        }
    }

    /// Upsert the Time
    pub fn set_time(self: &mut Sep2Model, time: &Time) -> Vec<Event> {
        self.time = time.clone();
        // Setting time causes no events required.
        Vec::new()
    }

    /// Find an EndDevice. This is the entrypoint to the model state, from
    /// which we follow links around.
    pub fn get_end_device(self: &Sep2Model, lfdi: HexBinary160) -> Option<&EndDevice> {
        self.end_devices
            .values()
            .find(|edev| edev.lfdi == Some(lfdi))
    }

    /// Find a curve by href rather than MRID.
    pub fn get_curve_by_href(self: &Sep2Model, href: &str) -> Option<&DERCurve> {
        self.curves
            .values()
            .find(|curve| curve.href.as_ref().is_some_and(|s| s == href))
    }

    /// Finds all controls and default controls for a single EndDevice from any
    /// DERPrograms it is part of, filtering out historical, cancelled and
    /// superseeded controls.
    ///
    /// Controls are returned in an ordered list of descending priority.
    pub fn all_controls_for_device<'a>(
        &'a self,
        lfdi: HexBinary160,
        now: Int64,
    ) -> Result<Vec<ControlRef<'a>>> {
        let Some(end_device) = self.get_end_device(lfdi) else {
            log::debug!("No device found when looking up controls");
            // We should never get here, except when first starting up. In that case let's return with a blank slate.
            return Ok(Vec::new());
        };

        // We try to ignore errors and throw warnings instead.

        // Sequence we must trace:
        // EndDevice -> FSA list -> FSA -> DERProgram list -> DERProgram -> DefaultDERControl / DERControl list -> DERControl.

        let Some(fsal_link) = end_device.function_set_assignments_list_link.as_ref() else {
            // Note that this is a common state to reach, before we have polled
            // all state we need. No need to return an Err here.
            log::warn!("No FSA link found for device");
            return Ok(Vec::new());
        };

        let Some(fsal) = self.function_set_assignments_lists.get(&fsal_link.href) else {
            log::debug!("Unable to find FSA list at {}", fsal_link.href);
            return Ok(Vec::new());
        };

        // Get all DERPrograms by tracing FSAL -> FSA -> DERP.
        let programs: Vec<_> = fsal
            .items
            .iter()
            .filter_map(|mrid| {
                self.function_set_assignments.get(mrid).or_else(|| {
                    log::debug!("Unable to find FSA by {mrid}");
                    None
                })
            })
            .filter_map(|fsa| {
                let href = &fsa
                    .der_program_list_link
                    // While a missing link means the server sent us invalid data, we
                    // will continue without it as best we can.
                    .as_ref()
                    .or_else(|| {
                        log::warn!("FSA ({}) has no DERProgramList link provided.", fsa.mrid);
                        None
                    })?
                    .href;
                self.program_lists.get(href).or_else(|| {
                    log::debug!("Unable to find DERProgramList at {href}");
                    None
                })
            })
            .flat_map(|derpl| {
                derpl.items.iter().filter_map(|mrid| {
                    self.programs.get(mrid).or_else(|| {
                        log::debug!("Unable to find DERProgram by {mrid}");
                        None
                    })
                })
            })
            .collect();

        // It is technically possible to have two programs of the same primacy,
        // which leaves some ambiguity around which program's defaults should
        // win. I am going to ignore this and simply flatten out the controls,
        // annotating them with the primacy from their program.
        let mut annotated_controls: Vec<(PrimacyType, ControlRef)> = programs.into_iter()
            .map(|program| {
                // While a missing link means the server sent us invalid data,
                // we will continue without it as best we can.
                let dercl_href = &program.der_control_list_link.as_ref()
                    .map_or_else(|| {
                        log::warn!("DERProgram ({}) has no DERControlList link provided.", program.mrid);
                        None
                    },
                        |link| Some(&link.href));
                let dercl = dercl_href.and_then(|href| self.control_lists.get(href)
                    .or_else(|| {
                        log::debug!("Unable to find DERControlList at {href}");
                        None
                    })
                );

                let mut num_controls = 0;
                let mut num_retained_by_status = 0;
                let mut num_retained = 0;

                let scheduled_controls = match dercl {
                    None => {
                        Vec::new()
                    },
                    Some(dercl) => {
                        dercl.items.iter().filter_map(|mrid| self.controls.get(mrid)
                        .or_else(|| { log::debug!("Unable to find DERControl by {mrid}"); None })
                        )
                            .inspect(|_| num_controls += 1)
                            // Filter out controls which are cancelled or suspended
                            // (Note we don't filter out partially suspended controls as they may be active on some attributes.
                            .filter(|control| matches!(control.der_control.event_status.current_status, EventStatusType::Scheduled | EventStatusType::Active))
                            .inspect(|_| num_retained_by_status += 1)
                            // Filter out controls which are in the past
                        .filter(|control| control.end_time().0 > now.0)
                            .inspect(|_| num_retained += 1)
                        .map(ControlRef::Scheduled)
                            .collect()
                    }
                };

                let dderc = program.default_der_control_link.as_ref().and_then(|link| self.default_controls.get(&link.href));
                let default_controls: Vec<_> = dderc.iter().map(|derc| ControlRef::Default(derc))
                    .collect();
                log::trace!("Program {} ({:?}): num controls (after filter by status) (after all filtering) (+ default): {} ({}) ({}) (+ {})", program.mrid, program.primacy, num_controls, num_retained_by_status, num_retained, default_controls.len());

                let annotated_controls: Vec<(PrimacyType, ControlRef)> = scheduled_controls.into_iter()
                    .chain(default_controls)
                    .map(|control| (program.primacy, control)).collect();
                Ok(annotated_controls)

            })
            // Bubble up errors
            .collect::<Result<Vec<_>>>()?
            // Then flatten out the nested lists
            .into_iter()
            .flatten()
            .collect();

        // Now let's sort them: first by primacy, then by time they were scheduled.
        annotated_controls.sort_by(|a, b| {
            a.0.cmp(&b.0).then_with(|| match (&a.1, &b.1) {
                (ControlRef::Default(_), _) => std::cmp::Ordering::Greater,
                (ControlRef::Scheduled(_), ControlRef::Default(_)) => std::cmp::Ordering::Less,
                (ControlRef::Scheduled(a_control), ControlRef::Scheduled(b_control)) => a_control
                    .der_control
                    .creation_time
                    .cmp(&b_control.der_control.creation_time)
                    .reverse(),
            })
        });

        // We don't need to return primacy to the caller, they just accept we have sorted them.
        let controls = annotated_controls
            .into_iter()
            .map(|(_, control)| control)
            .collect();

        Ok(controls)
    }
}

fn generic_log_list<T: SEList>(list: &Arc<T>) {
    let type_name = std::any::type_name::<T>();
    let final_name = type_name.rsplit("::").next().unwrap_or("unknown");
    log::debug!(
        "Obtained resource list for {}, returned {}/{} items.",
        final_name,
        list.results(),
        list.all()
    );

    if list.results() != list.all() {
        log::error!(
            "Got less items ({}) than available ({}), when receiving resource list {}. Items not retrieved in list will not be included. Scheduling behaviour may be inconsistent.",
            list.results(),
            list.all(),
            final_name
        );
    }
}

/// Convenience trait and function to return events for new/changed links
trait AllLinks {
    fn href(&self) -> String;
}
impl AllLinks for Link {
    fn href(&self) -> String {
        self.href.clone()
    }
}
impl AllLinks for ListLink {
    fn href(&self) -> String {
        self.href.clone()
    }
}

fn link_update_events<T: AllLinks>(
    original: &Option<T>,
    incoming: &Option<T>,
    resource_kind: ResourceKind,
) -> Vec<Event> {
    // If the hrefs are the same, no need to change subscriptions.
    if original.as_ref().map(|x| x.href()) == incoming.as_ref().map(|x| x.href()) {
        return Vec::new();
    }

    let old_update = original.as_ref().map(|link| Event::LinkRemoved {
        href: link.href(),
        kind: resource_kind,
    });
    let new_update = incoming.as_ref().map(|link| Event::LinkAdded {
        href: link.href(),
        kind: resource_kind,
    });

    [old_update, new_update]
        .into_iter()
        // Filter out Nones
        .flatten()
        .collect()
}

/// A convenience function to provide a href as a string.
///
/// While the protocol dictates that all resources returned in a GET request
/// must have a href, we use a fallback for type safety.
fn safe_href<T: SEResource>(resource: &T) -> String {
    resource
        .href()
        .map_or_else(|| String::from("/missinghref"), String::from)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sep2_common::packages::{
        edev::EndDeviceList,
        objects::EventStatus,
        primitives::Uint16,
        types::{DateTimeInterval, SFDIType},
    };

    use super::*;

    #[test]
    fn find_device_by_lfdi() {
        let mut model = Sep2Model::default();

        let lfdi = mock_lfdi();
        let sfdi = mock_sfdi();
        model.apply_update(mock_edev_list(lfdi, sfdi).into());

        let edev = model.get_end_device(lfdi).expect("End device not found");

        assert_eq!(edev.lfdi, Some(lfdi));
        assert_eq!(edev.sfdi, sfdi);
    }

    #[test]
    fn add_multiple_controls() {
        let mut model = Sep2Model::default();

        let mrid1 = MRIDType(42);
        let mrid2 = MRIDType(56);
        let href = String::from("/edev/1/derp/1/derc");
        let derc_list = DERControlList {
            der_control: vec![
                DERControl {
                    mrid: mrid1,
                    ..Default::default()
                },
                DERControl {
                    mrid: mrid2,
                    ..Default::default()
                },
            ],

            all: Uint32(2),
            results: Uint32(2),

            href: Some(href.clone()),
            ..Default::default()
        };

        model.apply_update(derc_list.into());

        assert!(model.controls.contains_key(&mrid1));
        assert!(model.controls.contains_key(&mrid2));

        let applied_derc_list = model
            .control_lists
            .get(&href)
            .expect("DERControlList not found in model after appplying it");
        assert_eq!(applied_derc_list.items.len(), 2);
    }

    fn all_emitted_responses(events: &[Event]) -> Vec<ResponseStatus> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::DERControlStatusChanged {
                    status,
                    reply_to: _,
                    subject: _,
                } => Some(status),
                _ => None,
            })
            .copied()
            .collect()
    }

    #[test]
    fn adding_a_control_emits_an_acknowledgment() {
        let mut model = Sep2Model::default();

        let events = model.apply_update(
            mock_derc_list(vec![mock_control_with_status(EventStatusType::Scheduled)]).into(),
        );

        assert_eq!(
            all_emitted_responses(&events),
            vec![ResponseStatus::EventReceived]
        );
    }

    #[test]
    fn cancelling_a_control_emits_event() {
        let mut model = Sep2Model::default();
        // Add initial control
        model.apply_update(
            mock_derc_list(vec![mock_control_with_status(EventStatusType::Scheduled)]).into(),
        );
        // Then change status
        let events = model.apply_update(
            mock_derc_list(vec![mock_control_with_status(EventStatusType::Cancelled)]).into(),
        );

        assert_eq!(
            all_emitted_responses(&events),
            vec![ResponseStatus::EventCancelled]
        );
    }

    #[test]
    fn superseding_a_control_emits_event() {
        let mut model = Sep2Model::default();
        // Add initial control
        model.apply_update(
            mock_derc_list(vec![mock_control_with_status(EventStatusType::Scheduled)]).into(),
        );
        // Then change status
        let events = model.apply_update(
            mock_derc_list(vec![mock_control_with_status(EventStatusType::Superseded)]).into(),
        );

        assert_eq!(
            all_emitted_responses(&events),
            vec![ResponseStatus::EventSuperseded]
        );
    }

    #[test]
    fn adding_a_control_already_started_emits_started_event() {
        let mut model = Sep2Model::default();

        let mut derc = mock_control_with_status(EventStatusType::Active);
        let now = Utc::now().timestamp();
        derc.interval.start = Int64(now - 15);

        let events = model.apply_update(mock_derc_list(vec![derc]).into());
        assert_eq!(
            all_emitted_responses(&events),
            vec![ResponseStatus::EventReceived, ResponseStatus::EventStarted]
        );
    }

    #[test]
    fn idempotent_changes_emit_no_events() {
        let mut model = Sep2Model::default();

        setup_model_with_mocks(&mut model);
        // Apply same objects a second time.
        let events = setup_model_with_mocks(&mut model);

        assert!(events.is_empty());
    }

    #[test]
    #[ignore = "List removal events are TODO"]
    fn removing_an_item_from_list_removes_the_item() {
        todo!();
    }

    #[test]
    fn controls_ordered_by_primacy() {
        // Setup a model and add a set of mocks.
        let mut model = Sep2Model::default();

        setup_model_with_mocks(&mut model);
        let now = Int64(Utc::now().timestamp());

        // Check the known controls - should see only one active control and one default control for the only program.
        {
            let ordered_controls = model.all_controls_for_device(mock_lfdi(), now).unwrap();
            assert_eq!(ordered_controls.len(), 2);
            let expected_control = mock_control_with_status(EventStatusType::Scheduled);
            let Some(ControlRef::Scheduled(control)) = ordered_controls.first() else {
                panic!("Did not find active control in index 0");
            };
            assert_eq!(control.der_control.mrid, expected_control.mrid);
            let Some(ControlRef::Default(_)) = ordered_controls.get(1) else {
                panic!("Did not find default control in index 1");
            };
        }

        // Now add another program with a higher primacy.
        let derp2 = DERProgram {
            href: Some(String::from("/edev/1/derp/2")),
            // Deliberately leaving out the default link.
            der_control_list_link: Some(ListLink {
                href: String::from("/edev/1/derp/2/derc"),
                ..Default::default()
            }),

            mrid: MRIDType(321),

            primacy: PrimacyType::InHomeEnergyManagementSystem,
            ..Default::default()
        };
        let mut derp_list = mock_derp_list();
        derp_list.all = Uint32(2);
        derp_list.results = Uint32(2);
        derp_list.der_program.push(derp2);

        model.apply_update(derp_list.into());

        // At this point there is a new program with a link to a DERControlList
        // that doesn't exist yet. Ensure that we get the same set of controls as before.
        {
            let ordered_controls = model.all_controls_for_device(mock_lfdi(), now).unwrap();
            assert_eq!(ordered_controls.len(), 2);
        }

        // Now add the DERControlList containing:
        // - a new control in the future,
        // - a cancelled control in the future,
        // - a superseded control in the future,
        // - a historical control.
        let derc_scheduled = DERControl {
            mrid: MRIDType(1001),
            ..mock_control_with_status(EventStatusType::Scheduled)
        };
        let derc_cancelled = DERControl {
            mrid: MRIDType(1002),
            ..mock_control_with_status(EventStatusType::Cancelled)
        };
        let derc_superseded = DERControl {
            mrid: MRIDType(1003),
            ..mock_control_with_status(EventStatusType::Superseded)
        };
        let derc_historical = DERControl {
            mrid: MRIDType(1004),
            interval: DateTimeInterval {
                start: Int64(now.0 - 3600),
                duration: Uint32(300),
            },
            ..mock_control_with_status(EventStatusType::Scheduled)
        };
        let derc_list = DERControlList {
            href: Some(String::from("/edev/1/derp/2/derc")),
            ..mock_derc_list(vec![
                derc_scheduled.clone(),
                derc_cancelled.clone(),
                derc_superseded.clone(),
                derc_historical.clone(),
            ])
        };
        model.apply_update(derc_list.into());

        // Now we expect to see a third control, which is in first place. The
        // cancelled, superseded and historical controls should not appear.
        {
            let ordered_controls = model.all_controls_for_device(mock_lfdi(), now).unwrap();
            assert_eq!(ordered_controls.len(), 3);
            let Some(ControlRef::Scheduled(control)) = ordered_controls.first() else {
                panic!("Did not find active control in index 0");
            };
            assert_eq!(control.der_control.mrid, derc_scheduled.mrid);
            let mrids: Vec<_> = ordered_controls
                .iter()
                .filter_map(|control| match control {
                    ControlRef::Scheduled(control) => Some(control.der_control.mrid),
                    ControlRef::Default(_) => None,
                })
                .collect();
            assert!(!mrids.contains(&derc_cancelled.mrid));
            assert!(!mrids.contains(&derc_superseded.mrid));
            assert!(!mrids.contains(&derc_historical.mrid));
        }
    }

    fn setup_model_with_mocks(model: &mut Sep2Model) -> Vec<Event> {
        let lfdi = mock_lfdi();
        let sfdi = mock_sfdi();
        let edevl = mock_edev_list(lfdi, sfdi);
        let derc = mock_control_with_status(EventStatusType::Scheduled);

        model
            .apply_update(edevl.into())
            .into_iter()
            .chain(model.apply_update(mock_fsa_list().into()))
            .chain(model.apply_update(mock_derp_list().into()))
            .chain(model.apply_update(mock_derc_list(vec![derc]).into()))
            .chain(model.apply_update(mock_dderc().into()))
            .collect()
    }

    fn mock_lfdi() -> HexBinary160 {
        HexBinary160::from_str("00112233445566").expect("Invalid LFDI in test")
    }
    fn mock_sfdi() -> SFDIType {
        SFDIType::new(42).expect("Invalid SFDI in test")
    }
    fn mock_edev_list(lfdi: HexBinary160, sfdi: SFDIType) -> EndDeviceList {
        EndDeviceList {
            href: Some(String::from("/edev")),

            end_device: vec![EndDevice {
                lfdi: Some(lfdi),
                sfdi,
                href: Some(String::from("/edev/1")),
                function_set_assignments_list_link: Some(ListLink {
                    href: String::from("/edev/1/fsa"),
                    ..Default::default()
                }),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        }
    }
    fn mock_fsa_list() -> FunctionSetAssignmentsList {
        FunctionSetAssignmentsList {
            href: Some(String::from("/edev/1/fsa")),

            function_set_assignments: vec![FunctionSetAssignments {
                href: Some(String::from("/edev/1/fsa/1")),
                der_program_list_link: Some(ListLink {
                    href: String::from("/edev/1/fsa/1/derp"),
                    ..Default::default()
                }),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        }
    }
    fn mock_derp_list() -> DERProgramList {
        DERProgramList {
            href: Some(String::from("/edev/1/fsa/1/derp")),
            der_program: vec![DERProgram {
                href: Some(String::from("/edev/1/derp/1")),
                default_der_control_link: Some(Link {
                    href: String::from("/edev/1/derp/1/dderc"),
                }),
                der_control_list_link: Some(ListLink {
                    href: String::from("/edev/1/derp/1/derc"),
                    ..Default::default()
                }),

                mrid: MRIDType(123),

                primacy: PrimacyType::NonContractualServiceProvider,
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        }
    }

    fn mock_derc_list(controls: Vec<DERControl>) -> DERControlList {
        let count = controls.len();
        DERControlList {
            href: Some(String::from("/edev/1/derp/1/derc")),
            der_control: controls,

            all: Uint32(count as u32),
            results: Uint32(count as u32),

            ..Default::default()
        }
    }
    fn mock_dderc() -> DefaultDERControl {
        DefaultDERControl {
            href: Some(String::from("/edev/1/derp/1/dderc")),

            set_grad_w: Some(Uint16(42)),
            ..Default::default()
        }
    }

    fn mock_control_with_status(status: EventStatusType) -> DERControl {
        let now = Utc::now().timestamp();
        DERControl {
            mrid: MRIDType(42),
            event_status: EventStatus {
                current_status: status,
                ..Default::default()
            },
            response_required: Some(
                ResponseRequired::MessageReceived | ResponseRequired::SpecificResponse,
            ),
            reply_to: Some(String::from("/edev/1/derp/1/derc/1")),
            interval: DateTimeInterval {
                // Default to starting an hour from now
                start: Int64(now + 3600),
                duration: Uint32(300),
            },
            ..Default::default()
        }
    }
}
