use async_broadcast::Receiver as BroadcastReceiver;
use chrono::Utc;
use sep2_common::packages::primitives::Int64;
use tokio::sync::mpsc::Sender as MpscSender;

use crate::translation::TryConvert;
use crate::{
    Error, Result, modbus_connection, scheduler,
    sep2_connection::{self, ControlResponse, Sep2ResourceEvent},
};

/// Reacts to updates for SEP2 resources and forwards those to the scheduler to
/// update its internal model.
pub async fn resource_update_dispatcher(
    mut sep2_conn_output: BroadcastReceiver<Sep2ResourceEvent>,
    scheduler_input: MpscSender<scheduler::Command>,
) -> Result<()> {
    while let Ok(event) = sep2_conn_output.recv().await {
        scheduler_input
            .send(scheduler::Command::ResourceUpdated(event))
            .await
            .map_err(|_| Error::ChannelClosed)?;
    }
    Err(Error::ChannelClosed)
}

/// Reacts to new polls required and old polls to be removed as well as
/// notifications on control status changes.
pub async fn sep2_subscription_and_notification_dispatcher(
    mut scheduler_output: BroadcastReceiver<scheduler::Event>,
    sep2_conn_input: MpscSender<sep2_connection::Command>,
) -> Result<()> {
    while let Ok(event) = scheduler_output.recv().await {
        match event {
            scheduler::Event::LinkAdded { href, kind } => {
                sep2_conn_input
                    .send(sep2_connection::Command::SubscribeToResource { href, kind })
                    .await
                    .map_err(|_| Error::ChannelClosed)?;
            }
            scheduler::Event::LinkRemoved { href, kind: _ } => {
                sep2_conn_input
                    .send(sep2_connection::Command::UnsubscribeFromResource { href })
                    .await
                    .map_err(|_| Error::ChannelClosed)?;
            }
            scheduler::Event::DERControlStatusChanged {
                subject,
                status,
                reply_to,
            } => {
                let now = Int64(Utc::now().timestamp());
                sep2_conn_input
                    .send(sep2_connection::Command::SendControlResponse(
                        ControlResponse::new(subject, status, reply_to, now),
                    ))
                    .await
                    .map_err(|_| Error::ChannelClosed)?;
            }
            scheduler::Event::ParametersChanged(_) => {
                // Ignore changed parameters
            }
        }
    }

    Err(Error::ChannelClosed)
}

/// Reacts to changes in the currently applied controls from the scheduler and
/// sends these as commands to the modbus task.
pub async fn control_change_dispatcher(
    mut scheduler_output: BroadcastReceiver<scheduler::Event>,
    modbus_input: MpscSender<modbus_connection::Command>,
) -> Result<()> {
    while let Ok(event) = scheduler_output.recv().await {
        match event {
            scheduler::Event::ParametersChanged(control_attributes) => {
                match (*control_attributes).clone().try_convert() {
                    Ok(modbus_parameters) => {
                        modbus_input
                            .send(modbus_connection::Command::UpdateParameters(
                                modbus_parameters,
                            ))
                            .await
                            .map_err(|_| Error::ChannelClosed)?;
                    }
                    Err(err) => {
                        log::warn!("Failed to translate SEP2 controls to modbus parameters: {err}");
                    }
                }
            }
            scheduler::Event::LinkAdded { .. }
            | scheduler::Event::LinkRemoved { .. }
            | scheduler::Event::DERControlStatusChanged { .. } => {
                // Ignore these events
            }
        }
    }

    Err(Error::ChannelClosed)
}

/// Reacts to events from the modbus task indicating a change in the device
/// state and send those as commands to the SEP2 task.
pub async fn sep2_device_state_dispatcher(
    mut modbus_output: BroadcastReceiver<modbus_connection::Event>,
    sep2_conn_input: MpscSender<sep2_connection::Command>,
) -> Result<()> {
    while let Ok(event) = modbus_output.recv().await {
        match event {
            // Note: for each of these, we can potentially fail conversion from
            // modbus to SEP2 translation. In that case, we log a warning and do
            // not pass the message along but continue otherwise.
            modbus_connection::Event::CapabilitiesPolled(cap) => match cap.try_convert() {
                Ok(der_capability) => {
                    sep2_conn_input
                        .send(sep2_connection::Command::SendDeviceCapability(
                            der_capability,
                        ))
                        .await
                        .map_err(|_| Error::ChannelClosed)?;
                }
                Err(err) => {
                    log::warn!(
                        "Failed to translate modbus device capabilities to SEP2 DERCapability: {err}"
                    );
                }
            },
            modbus_connection::Event::StatePolled(status, settings) => {
                match status.try_convert() {
                    Ok(der_status) => {
                        sep2_conn_input
                            .send(sep2_connection::Command::SendDeviceStatus(der_status))
                            .await
                            .map_err(|_| Error::ChannelClosed)?;
                    }
                    Err(err) => {
                        log::warn!(
                            "Failed to translate modbus device status to SEP2 DERStatus: {err}"
                        );
                    }
                };
                match settings.try_convert() {
                    Ok(der_settings) => {
                        sep2_conn_input
                            .send(sep2_connection::Command::SendDeviceSettings(der_settings))
                            .await
                            .map_err(|_| Error::ChannelClosed)?;
                    }
                    Err(err) => {
                        log::warn!(
                            "Failed to translate modbus device status to SEP2 DERStatus: {err}"
                        );
                    }
                };
            }
        }
    }

    Err(Error::ChannelClosed)
}
