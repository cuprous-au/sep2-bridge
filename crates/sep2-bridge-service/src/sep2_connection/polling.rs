use async_broadcast::Sender as BroadcastSender;
use sep2_client::client::{Client, PollCallback};
use sep2_common::{
    packages::{
        der::{DERControlList, DERProgramList, DefaultDERControl},
        edev::EndDeviceList,
        fsa::FunctionSetAssignmentsList,
        primitives::Uint32,
        time::Time,
    },
    traits::SEResource,
};

use crate::{ResourceKind, sep2_connection::Sep2ResourceEvent};

/// Make a callback function to feed a resource into the broadcast channel.
pub fn make_poll_callback<T>(
    broadcast: BroadcastSender<super::Sep2ResourceEvent>,
) -> impl PollCallback<T>
where
    T: Into<Sep2ResourceEvent> + SEResource,
{
    move |data: T| {
        let broadcast = broadcast.clone();
        async move {
            let _ = broadcast.broadcast(data.into()).await;
        }
    }
}

/// Retrieves a resource from the SEP2 server and sets up a poll for continued
/// updates on the resource. Information is returned via the broadcast channel.
pub async fn start_poll_for(
    kind: ResourceKind,
    client: Client,
    href: &str,
    poll_rate: u32,
    max_list_size: u32,
    broadcast: BroadcastSender<super::Sep2ResourceEvent>,
) {
    log::trace!("Setting up poll for {href}");
    let kind_href = match kind {
        // Singular items use the href as is.
        ResourceKind::Time
        | ResourceKind::DefaultDERControl
        | ResourceKind::DERProgram
        | ResourceKind::DERControl
        | ResourceKind::EndDevice
        | ResourceKind::FunctionSetAssignments => href,
        // Lists add pagination options.
        ResourceKind::EndDeviceList
        | ResourceKind::FunctionSetAssignmentsList
        | ResourceKind::DERProgramList
        | ResourceKind::DERControlList => &paginated_uri(href, max_list_size),
    };
    match kind {
        ResourceKind::Time => {
            get_then_poll(
                client,
                kind_href,
                poll_rate,
                make_poll_callback::<Time>(broadcast),
            )
            .await
        }
        ResourceKind::EndDeviceList => {
            get_then_poll(
                client,
                kind_href,
                poll_rate,
                make_poll_callback::<EndDeviceList>(broadcast),
            )
            .await
        }
        ResourceKind::FunctionSetAssignmentsList => {
            get_then_poll(
                client,
                kind_href,
                poll_rate,
                make_poll_callback::<FunctionSetAssignmentsList>(broadcast),
            )
            .await
        }
        ResourceKind::DERProgramList => {
            get_then_poll(
                client,
                kind_href,
                poll_rate,
                make_poll_callback::<DERProgramList>(broadcast),
            )
            .await
        }
        ResourceKind::DERControlList => {
            get_then_poll(
                client,
                kind_href,
                poll_rate,
                make_poll_callback::<DERControlList>(broadcast),
            )
            .await
        }
        ResourceKind::DefaultDERControl => {
            get_then_poll(
                client,
                kind_href,
                poll_rate,
                make_poll_callback::<DefaultDERControl>(broadcast),
            )
            .await
        }
        // These are no-ops: we don't want to subscribe to them individually as they will be obtained by a list subscription instead.
        ResourceKind::EndDevice
        | ResourceKind::FunctionSetAssignments
        | ResourceKind::DERProgram
        | ResourceKind::DERControl => {}
    };
}

/// Convenience function to get the resource immediately instead of waiting for
/// the first poll event.
pub async fn get_then_poll<T, U>(client: Client, href: &str, poll_rate: u32, callback: T)
where
    T: PollCallback<U>,
    U: SEResource,
{
    match client.get(href).await {
        Ok(output) => {
            callback.callback(output).await;
        }
        Err(err) => {
            log::warn!("Error while attempting initial GET when setting up poll on {href}: {err}")
        }
    }

    client
        .start_poll(href, Some(Uint32(poll_rate)), callback)
        .await;
}

pub fn paginated_uri(uri: &str, max_list_size: u32) -> String {
    format!("{uri}?l={}", max_list_size)
}
