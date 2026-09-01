use derive_more::Display;

pub mod dispatch;
pub mod modbus_connection;
mod scaled_value;
pub mod scheduler;
pub mod sep2_connection;
mod translation;

pub use scaled_value::ScaledValue;
pub(crate) use scaled_value::ScaledValueInner;

#[derive(Debug, Display)]
pub enum Error {
    #[display("Device returned from server was not known to us")]
    UnexpectedDevice,
    #[display("Server rejected our update")]
    ServerRejected,
    #[display("Invalid input: {_0}")]
    InvalidInput(String),
    #[display("Item known by its href or MRID but can't be found in the model")]
    ItemDetailsUnknown,
    #[display("Channel was closed when trying to send.")]
    ChannelClosed,
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Allows referencing types of sep2_common resources at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display)]
pub enum ResourceKind {
    Time,
    EndDeviceList,
    EndDevice,
    FunctionSetAssignmentsList,
    FunctionSetAssignments,
    DERProgramList,
    DERProgram,
    DefaultDERControl,
    DERControlList,
    DERControl,
    DERCurveList,
    DERCurve,
}

pub fn deactivated_broadcast<T>(
    cap: usize,
) -> (
    async_broadcast::Sender<T>,
    async_broadcast::InactiveReceiver<T>,
) {
    let (mut tx, rx) = async_broadcast::broadcast(cap);
    let rx = rx.deactivate();
    tx.set_await_active(false);

    (tx, rx)
}
