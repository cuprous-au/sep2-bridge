// The internal representation caching the upstream state communicated over SEP2.

use chrono::Utc;
use rand::{RngExt, rngs::ThreadRng};
use sep2_common::{
    packages::{
        der::{
            DERControl, DERControlList, DERCurve, DERCurveList, DERProgram, DERProgramList,
            DefaultDERControl,
        },
        edev::{EndDevice, EndDeviceList},
        fsa::{FunctionSetAssignments, FunctionSetAssignmentsList},
        identification::{ResponseRequired, ResponseStatus},
        objects::EventStatusType,
        primitives::{HexBinary160, Int64, Uint32},
        time::Time,
        types::{MRIDType, PrimacyType},
    },
    traits::{SEList, SEResource},
};
use std::{
    collections::{BTreeMap, HashMap, hash_map::Entry},
    hash::Hash,
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
#[derive(Debug)]
struct IDList<T> {
    pub href: String,
    pub items: Vec<T>,
    pub poll_rate: Option<Uint32>,
}
type MRIDList = IDList<MRIDType>;
type LFDIList = IDList<HexBinary160>;

/// A resource the model needs to be kept up to date with, and the poll rate
/// that applies to it.
#[derive(Debug, PartialEq, Eq)]
struct ResourceLink {
    kind: ResourceKind,
    poll_rate: Option<Uint32>,
}
type ResourceLinks = BTreeMap<String, ResourceLink>;

/// An entry for `ResourceLinks`: a resource's href, its kind and the poll rate
/// that applies to it.
fn resource_link(
    href: impl Into<String>,
    kind: ResourceKind,
    poll_rate: Option<Uint32>,
) -> (String, ResourceLink) {
    (href.into(), ResourceLink { kind, poll_rate })
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
    // The end device list is special as it is unique and contains LFDIs
    // instead of MRIDs. It is stored along with its href.
    end_device_list: Option<LFDIList>,

    // The individual resource definitions.
    // End devices are keyed by LFDI, as an href may refer to a different
    // device over time.
    end_devices: HashMap<HexBinary160, EndDevice>,
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

        // We build the entire list of resource links before and after applying
        // the update and diff them to identify any changes. While this is a
        // little wasteful in calculation, it pays off in the non-local effects
        // each resource can have on others (parent resources control their
        // children's pollrates sometimes 3 levels deep, items can disappear
        // from lists). For the number of resources expected in the model, this
        // cost is expected to be small.
        let resource_links_before = self.resource_links();

        // Collect up any events that require responses to the SEP2 server.
        let control_events = match update {
            Sep2ResourceEvent::Time(time) => self.set_time(&time),
            Sep2ResourceEvent::EndDeviceList(edl) => {
                generic_log_list(&edl);
                self.set_end_device_list(&edl)
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
        };

        let resource_links_after = self.resource_links();

        // TODO: Go through all of our resources and any that don't exist in the
        // resource_links_after are orphaned and should be removed from the
        // model. This is GH issue #20.

        // Announce changes to the set of polled resources first, followed by
        // any control responses.
        let mut events = Self::resource_link_events(resource_links_before, resource_links_after);
        events.extend(control_events);
        events
    }

    /// Upsert an EndDeviceList. Will upsert EndDevices too.
    pub fn set_end_device_list(self: &mut Sep2Model, incoming: &EndDeviceList) -> Vec<Event> {
        // We should always have a href, but for type safety let's abort early if we don't.
        let Some(href) = require_href(incoming) else {
            return Vec::new();
        };

        // Create a simplified list
        let list = LFDIList {
            href,
            items: incoming
                .end_device
                .iter()
                // Flat map will throw away any device without an LFDI. All
                // devices should have an LFDI in any case.
                .flat_map(|edev| edev.lfdi)
                .collect(),
            poll_rate: incoming.poll_rate,
        };
        self.end_device_list = Some(list);

        incoming
            .end_device
            .iter()
            .for_each(|edev| self.set_end_device(edev));

        // No responses required.
        Vec::new()
    }

    /// Insert/replace information about a single EndDevice in the model.
    pub fn set_end_device(self: &mut Sep2Model, incoming: &EndDevice) {
        let Some(lfdi) = incoming.lfdi else {
            log::warn!(
                "Ignoring EndDevice ({}) without an LFDI.",
                incoming.href.as_deref().unwrap_or("missing href")
            );
            return;
        };
        self.end_devices.insert(lfdi, incoming.clone());
    }

    /// Upsert a FunctionSetAssignmentsList. Will upsert FunctionSetAssignments too.
    pub fn set_function_set_assignments_list(
        self: &mut Sep2Model,
        incoming: &FunctionSetAssignmentsList,
    ) -> Vec<Event> {
        // We should always have a href, but for type safety let's abort early if we don't.
        let Some(href) = require_href(incoming) else {
            return Vec::new();
        };

        // Create a simplified list
        let list = MRIDList {
            href: href.clone(),
            items: incoming
                .function_set_assignments
                .iter()
                .map(|fsa| fsa.mrid)
                .collect(),
            poll_rate: incoming.poll_rate,
        };
        self.function_set_assignments_lists.insert(href, list);

        incoming
            .function_set_assignments
            .iter()
            .for_each(|fsa| self.set_function_set_assignments(fsa));

        // No responses required.
        Vec::new()
    }

    /// Upsert a FunctionSetAssignments.
    pub fn set_function_set_assignments(self: &mut Sep2Model, incoming: &FunctionSetAssignments) {
        self.function_set_assignments
            .insert(incoming.mrid, incoming.clone());
    }

    /// Upsert a DERProgramList. Will upsert DERPrograms too.
    pub fn set_der_program_list(self: &mut Sep2Model, incoming: &DERProgramList) -> Vec<Event> {
        // We should always have a href, but for type safety let's abort early if we don't.
        let Some(href) = require_href(incoming) else {
            return Vec::new();
        };

        let list = MRIDList {
            href: href.clone(),
            items: incoming.der_program.iter().map(|derp| derp.mrid).collect(),
            poll_rate: incoming.poll_rate,
        };
        self.program_lists.insert(href, list);

        incoming
            .der_program
            .iter()
            .for_each(|derp| self.set_der_program(derp));

        // No responses required.
        Vec::new()
    }

    /// Upsert a DERProgram.
    pub fn set_der_program(self: &mut Sep2Model, incoming: &DERProgram) {
        self.programs.insert(incoming.mrid, incoming.clone());
    }

    /// Upsert a DERControlList. Will upsert DERControls too.
    pub fn set_der_control_list(
        self: &mut Sep2Model,
        incoming: &DERControlList,
        rng: &mut ThreadRng,
    ) -> Vec<Event> {
        // We should always have a href, but for type safety let's abort early if we don't.
        let Some(href) = require_href(incoming) else {
            return Vec::new();
        };

        let list = MRIDList {
            href: href.clone(),
            items: incoming.der_control.iter().map(|derp| derp.mrid).collect(),
            // The poll rate of a DERControlList is decided by the parent
            // DERProgramList. We can safely set it to None here.
            poll_rate: None,
        };
        self.control_lists.insert(href, list);

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

                // Event for acknowledging this message has been received.
                reply_to_if_required(incoming, ResponseRequired::MessageReceived)
                    .map(|reply_to| Event::DERControlStatusChanged {
                        subject: mrid,
                        status: ResponseStatus::EventReceived,
                        reply_to: reply_to.clone(),
                    })
                    .into_iter()
                    // Event mentioning the control has already started.
                    .chain(
                        already_started
                            .then(|| {
                                reply_to_if_required(incoming, ResponseRequired::SpecificResponse)
                                    .map(|reply_to| Event::DERControlStatusChanged {
                                        subject: mrid,
                                        status: ResponseStatus::EventStarted,
                                        reply_to: reply_to.clone(),
                                    })
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
        // We should always have a href, but for type safety let's abort early if we don't.
        let Some(href) = require_href(incoming) else {
            return Vec::new();
        };
        self.default_controls.insert(href, incoming.clone());

        // No responses required.
        Vec::new()
    }

    /// Upsert a DERCurveList. Will upsert DERCurves too.
    pub fn set_der_curve_list(self: &mut Sep2Model, incoming: &DERCurveList) -> Vec<Event> {
        // We should always have a href, but for type safety let's abort early if we don't.
        let Some(href) = require_href(incoming) else {
            return Vec::new();
        };

        let list = MRIDList {
            href: href.clone(),
            items: incoming.der_curve.iter().map(|curve| curve.mrid).collect(),
            // The poll rate of a DERCurveList is decided by the parent
            // DERProgramList. We can safely set it to None here.
            poll_rate: None,
        };
        self.curve_lists.insert(href, list);

        incoming
            .der_curve
            .iter()
            .for_each(|curve| self.set_der_curve(curve));

        // No responses required.
        Vec::new()
    }

    /// Upsert a DERCurve.
    pub fn set_der_curve(self: &mut Sep2Model, incoming: &DERCurve) {
        self.curves.insert(incoming.mrid, incoming.clone());
    }

    /// Upsert the Time
    pub fn set_time(self: &mut Sep2Model, time: &Time) -> Vec<Event> {
        self.time = time.clone();

        // No responses required.
        Vec::new()
    }

    /// Compares the resources needed by the model against a prior set,
    /// returning events for any that were added, changed or removed.
    fn resource_link_events(prior: ResourceLinks, post: ResourceLinks) -> Vec<Event> {
        let removed = prior
            .iter()
            .filter(|(href, _)| !post.contains_key(*href))
            .map(|(href, resource)| Event::LinkRemoved {
                href: href.clone(),
                kind: resource.kind,
            });
        let added_or_updated = post
            .iter()
            .filter(|(href, resource)| prior.get(*href) != Some(*resource))
            .map(|(href, resource)| Event::LinkAddedOrUpdated {
                href: href.clone(),
                kind: resource.kind,
                poll_rate: resource.poll_rate,
            });
        removed.chain(added_or_updated).collect()
    }

    /// Identify all resources which the model needs to be kept up to date with,
    /// including resources that the model currently has and resources the model
    /// is yet to receive. Apart from Time, these are found by following links
    /// from the EndDeviceList. Most lists use their own poll rate, and all
    /// other resources use the poll rate of their closest parent list.
    fn resource_links(self: &Sep2Model) -> ResourceLinks {
        // The Time href is only known once it has been received.
        let time = self
            .time
            .href
            .iter()
            .map(|href| resource_link(href, ResourceKind::Time, self.time.poll_rate));

        let end_devices = self.end_device_list.iter().flat_map(|edl| {
            list_links(
                &edl.href,
                ResourceKind::EndDeviceList,
                ResourceKind::EndDevice,
                edl.poll_rate,
                list_items(Some(edl), &self.end_devices),
                |edev| {
                    edev.function_set_assignments_list_link
                        .iter()
                        .flat_map(|link| self.function_set_assignments_list_links(&link.href))
                        .collect()
                },
            )
        });

        time.chain(end_devices).collect()
    }

    /// The links for a FunctionSetAssignmentsList and everything below it.
    fn function_set_assignments_list_links(self: &Sep2Model, href: &str) -> ResourceLinks {
        let fsal = self.function_set_assignments_lists.get(href);

        list_links(
            href,
            ResourceKind::FunctionSetAssignmentsList,
            ResourceKind::FunctionSetAssignments,
            // The poll rate is unknown until the list has been received.
            fsal.and_then(|list| list.poll_rate),
            list_items(fsal, &self.function_set_assignments),
            |fsa| {
                fsa.der_program_list_link
                    .iter()
                    .flat_map(|link| self.der_program_list_links(&link.href))
                    .collect()
            },
        )
    }

    /// The links for a DERProgramList and everything below it.
    fn der_program_list_links(self: &Sep2Model, href: &str) -> ResourceLinks {
        let derpl = self.program_lists.get(href);
        // The poll rate is unknown until the list has been received. Everything
        // below the DERProgramList uses its poll rate.
        let poll_rate = derpl.and_then(|list| list.poll_rate);

        list_links(
            href,
            ResourceKind::DERProgramList,
            ResourceKind::DERProgram,
            poll_rate,
            list_items(derpl, &self.programs),
            |derp| {
                let default_control = derp.default_der_control_link.iter().map(|link| {
                    resource_link(&link.href, ResourceKind::DefaultDERControl, poll_rate)
                });
                let controls = derp.der_control_list_link.iter().flat_map(|link| {
                    list_links(
                        &link.href,
                        ResourceKind::DERControlList,
                        ResourceKind::DERControl,
                        poll_rate,
                        list_items(self.control_lists.get(&link.href), &self.controls)
                            .map(|control| &control.der_control),
                        |_| ResourceLinks::new(),
                    )
                });
                let curves = derp.der_curve_list_link.iter().flat_map(|link| {
                    list_links(
                        &link.href,
                        ResourceKind::DERCurveList,
                        ResourceKind::DERCurve,
                        poll_rate,
                        list_items(self.curve_lists.get(&link.href), &self.curves),
                        |_| ResourceLinks::new(),
                    )
                });

                default_control.chain(controls).chain(curves).collect()
            },
        )
    }

    /// Find an EndDevice. This is the entrypoint to the model state, from
    /// which we follow links around.
    pub fn get_end_device(self: &Sep2Model, lfdi: HexBinary160) -> Option<&EndDevice> {
        self.end_devices.get(&lfdi)
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

/// A convenience function to extract the href as a String and log an error if
/// it is not present.
///
/// A SEP2 server must provide HREFs for its resources so we never expect to hit
/// this edge case and it is included for typesafety only.
fn require_href<T: SEResource>(resource: &T) -> Option<String> {
    match resource.href() {
        None => {
            log::error!(
                "Ignoring resource {} without a href",
                std::any::type_name::<T>()
            );
            None
        }
        Some(href) => Some(String::from(href)),
    }
}

/// The links for a list and each of its items, all using the given poll rate.
/// The links below each item are provided by `item_links`.
fn list_links<'a, T: SEResource + 'a>(
    href: &str,
    kind: ResourceKind,
    item_kind: ResourceKind,
    poll_rate: Option<Uint32>,
    items: impl Iterator<Item = &'a T>,
    item_links: impl Fn(&T) -> ResourceLinks,
) -> ResourceLinks {
    iter::once(resource_link(href, kind, poll_rate))
        .chain(items.flat_map(|item| {
            item.href()
                .map(|href| resource_link(href, item_kind, poll_rate))
                .into_iter()
                .chain(item_links(item))
        }))
        .collect()
}

/// Looks up each item of a list in the model, skipping any not yet received.
fn list_items<'a, K: Eq + Hash, V>(
    list: Option<&'a IDList<K>>,
    items: &'a HashMap<K, V>,
) -> impl Iterator<Item = &'a V> {
    list.into_iter()
        .flat_map(|list| &list.items)
        .filter_map(|id| items.get(id))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sep2_common::packages::{
        edev::EndDeviceList,
        identification::{Link, ListLink},
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
    fn program_list_changes_reach_its_children() {
        let mut model = Sep2Model::default();
        setup_model_with_mocks(&mut model);

        let children = [
            ("/edev/1/derp/1", ResourceKind::DERProgram),
            ("/edev/1/derp/1/dderc", ResourceKind::DefaultDERControl),
            ("/edev/1/derp/1/derc", ResourceKind::DERControlList),
            ("/edev/1/derp/1/derc/1", ResourceKind::DERControl),
        ];
        let program_list = ("/edev/1/fsa/1/derp", ResourceKind::DERProgramList);

        // Changing the list's poll rate announces it and everything below it
        // with the new rate.
        let events = model.apply_update(
            DERProgramList {
                poll_rate: Some(Uint32(60)),
                ..mock_derp_list()
            }
            .into(),
        );
        let expected: Vec<_> = children
            .into_iter()
            .chain([program_list])
            .map(|(href, kind)| Event::LinkAddedOrUpdated {
                href: String::from(href),
                kind,
                poll_rate: Some(Uint32(60)),
            })
            .collect();
        assert_eq!(events, expected);

        // Removing the program from the list means everything below it is no
        // longer needed.
        let events = model.apply_update(
            DERProgramList {
                poll_rate: Some(Uint32(60)),
                der_program: Vec::new(),
                all: Uint32(0),
                results: Uint32(0),
                ..mock_derp_list()
            }
            .into(),
        );
        let expected: Vec<_> = children
            .into_iter()
            .map(|(href, kind)| Event::LinkRemoved {
                href: String::from(href),
                kind,
            })
            .collect();
        assert_eq!(events, expected);
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
            href: Some(String::from("/edev/1/derp/1/derc/1")),
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
