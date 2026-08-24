use std::collections::HashMap;
use std::future::ready;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use sunspec::models::model1::Model1;
use sunspec::models::model103::Model103;
use sunspec::models::model701::{self, Model701};
use sunspec::models::model702::Model702;
use sunspec::models::model703::{self, Model703};
use sunspec::models::model713::Model713;
use sunspec::{Group, Model, Point, Value};

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{self, JoinHandle};
use tokio_modbus::Request;
use tokio_modbus::prelude::*;
use tokio_modbus::server::tcp::{Server, accept_tcp_connection};

/// Represents a mocked sunspec modbus server.
pub struct SunSpecMock {
    pub addr: Option<SocketAddr>,
    service_data: SunSpecService,

    /// Locations for registers that we want to read/write later.
    locations: Locations,

    /// Notify and handle to stop the server.
    stop_notify: Option<(JoinHandle<()>, oneshot::Sender<()>)>,
}

impl SunSpecMock {
    /// Prepares the registers to start a server.
    pub async fn new(
        enabled_models: Option<&[u32]>,
    ) -> Result<SunSpecMock, Box<dyn std::error::Error>> {
        // Initialise the internal state
        let mut registers = vec![0u16; 40500];
        let locations = initialise_registers(&mut registers, enabled_models);

        // Mutex these states for the service.
        let service_data = SunSpecService {
            registers: Arc::new(Mutex::new(registers)),
        };

        Ok(SunSpecMock {
            addr: None,
            service_data,
            locations,
            stop_notify: None,
        })
    }

    /// Starts the modbus server. If addr is None an arbitrary port will be chosen.
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // I would prefer a unix socket here, but tokio-modbus doesn't support it.
        let addr = self.addr.unwrap_or("127.0.0.1:0".parse()?);
        let listener = TcpListener::bind(addr).await?;
        self.addr = Some(listener.local_addr()?);
        eprintln!("SunSpec DER Server running at {}", self.addr.unwrap());
        let server = Server::new(listener);

        // Callbacks for the tokio-modbus serving.
        let new_service = {
            let service_data = self.service_data.clone();
            move |_| {
                eprintln!("Accepting new connection");
                Ok(Some(service_data.clone()))
            }
        };

        let on_connected = move |stream, addr| {
            let new_service = new_service.clone();
            async move { accept_tcp_connection(stream, addr, new_service) }
        };
        let on_process_error = |err| {
            eprintln!("on_process_error: {err}");
        };

        // Prepare a termination signal.
        let (oneshot_tx, oneshot_rx) = oneshot::channel();

        let handle = task::spawn(async move {
            server
                .serve_until(&on_connected, on_process_error, async {
                    oneshot_rx.await.unwrap()
                })
                .await
                .expect("SunSpecMock server died");
        });

        self.stop_notify = Some((handle, oneshot_tx));

        Ok(())
    }

    /// Stops the modbus server but leaves the registers intact.
    pub async fn stop(&mut self) {
        if let Some((handle, ch)) = self.stop_notify.take() {
            let _ = ch.send(());
            let _ = handle.await;
        }
    }

    /// Return a value of a named register. For new registers to be added, their
    /// names need to be specially included when initialising the registers.
    pub fn get_value<T: Value>(&self, name: &str) -> T {
        let (offset, length) = *self
            .locations
            .get(name)
            .unwrap_or_else(|| panic!("Unknown location {}", name));
        let registers = self.service_data.registers.lock().expect("Mutex failure");
        T::decode(&registers[offset..offset + length]).expect("Decode failure")
    }

    /// Sets a value of a named register. For new registers to be added, their
    /// names need to be specially included when initialising the registers.
    pub fn set_value<T: Value>(&self, name: &str, value: T) {
        let (offset, length) = *self
            .locations
            .get(name)
            .unwrap_or_else(|| panic!("Unknown location {}", name));
        let mut registers = self.service_data.registers.lock().expect("Mutex failure");
        let words = value.encode();
        registers[offset..offset + length].copy_from_slice(&words);
    }
}

impl Drop for SunSpecMock {
    fn drop(&mut self) {
        // We can't be async here, so simply send the signal but don't wait for the task.
        if let Some((_, ch)) = self.stop_notify.take() {
            let _ = ch.send(());
        }
    }
}

/// A dedicated struct for the tokio-modbus service. Registers are shared
/// between connections via a mutex. The service is dumb - it reads and writes
/// blindly to the locations requested by the client, with only a buffer overrun
/// sanity check.
#[derive(Clone, Debug)]
struct SunSpecService {
    registers: Arc<Mutex<Vec<u16>>>,
}

impl tokio_modbus::server::Service for SunSpecService {
    type Request = Request<'static>;
    type Response = Option<Response>;
    type Exception = ExceptionCode;
    type Future = std::future::Ready<Result<Self::Response, Self::Exception>>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let mut regs = self.registers.lock().unwrap();

        match req {
            Request::ReadHoldingRegisters(addr, count) => {
                let start = addr as usize;
                let end = start + count as usize;

                let data = if end <= regs.len() {
                    regs[start..end].to_vec()
                } else {
                    vec![0; count as usize]
                };

                ready(Ok(Some(Response::ReadHoldingRegisters(data))))
            }
            Request::ReadInputRegisters(addr, count) => {
                let start = addr as usize;
                let end = start + count as usize;

                let data = if end <= regs.len() {
                    regs[start..end].to_vec()
                } else {
                    vec![0; count as usize]
                };

                ready(Ok(Some(Response::ReadInputRegisters(data))))
            }
            Request::WriteSingleRegister(addr_u16, value) => {
                let addr = usize::from(addr_u16);
                // Minimal checking
                if addr >= regs.len() {
                    ready(Err(ExceptionCode::IllegalDataAddress))
                } else {
                    regs[addr] = value;
                    ready(Ok(Some(Response::WriteSingleRegister(addr_u16, value))))
                }
            }
            Request::WriteMultipleRegisters(addr_u16, values) => {
                let addr = usize::from(addr_u16);
                // Minimal checking
                if addr + values.len() > regs.len() {
                    ready(Err(ExceptionCode::IllegalDataAddress))
                } else {
                    regs[addr..addr + values.len()].copy_from_slice(&values);
                    ready(Ok(Some(Response::WriteMultipleRegisters(
                        addr_u16,
                        values.len() as u16,
                    ))))
                }
            }
            _ => ready(Err(ExceptionCode::IllegalFunction)),
        }
    }
}

/////
// Register initialisation, reading and writing

/// A helper to fill in registers from a Point of the sunspec crate.
trait PointWrite<U> {
    fn fill_registers(&self, registers: &mut [u16], base: usize, value: U) -> usize;
}
impl<T: Model, U: Value> PointWrite<U> for Point<T, U> {
    fn fill_registers(self: &Point<T, U>, registers: &mut [u16], base: usize, value: U) -> usize {
        let start = base + (self.offset as usize);
        let value_end = start + (self.length as usize);
        let words = value.encode();
        let payload_end = start + words.len();
        registers[start..payload_end].copy_from_slice(&words);

        value_end
    }
}

type Locations = HashMap<String, (usize, usize)>;
fn location<T: Model, U: Value>(point: Point<T, U>, base: usize) -> (usize, usize) {
    (base + usize::from(point.offset), usize::from(point.length))
}

/// Fill the registers with some dummy data. Some select locations are saved for
/// later random-access reading/writing.
fn initialise_registers(registers: &mut [u16], enabled_models: Option<&[u32]>) -> Locations {
    let mut locations = HashMap::new();
    // Place 'SunS' magic bytes at base offsets 0 and 40000 for standard discovery
    for base in [0, 40000] {
        registers[base..base + 2].copy_from_slice(&String::from("SunS").encode());

        let mut offset = base + 2;
        if enabled_models.is_none_or(|v| v.contains(&1)) {
            offset = add_model_1(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&103)) {
            offset = add_model_103(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&701)) {
            offset = add_model_701(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&702)) {
            offset = add_model_702(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&703)) {
            offset = add_model_703(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&713)) {
            offset = add_model_713(registers, offset, &mut locations);
        }
        add_end_of_model(registers, offset);
    }

    locations
}
/// Appends Model 1 (Common)
pub fn add_model_1(registers: &mut [u16], base_offset: usize, locations: &mut Locations) -> usize {
    registers[base_offset] = Model1::ID;
    registers[base_offset + 1] = Model1::LEN;

    let offset = base_offset + 2;
    Model1::MN.fill_registers(registers, offset, String::from("RustMockMfg"));
    Model1::MD.fill_registers(registers, offset, String::from("RustDERInverter"));
    Model1::OPT.fill_registers(registers, offset, None);
    Model1::VR.fill_registers(registers, offset, None);
    Model1::SN.fill_registers(registers, offset, String::from("RustMockMfg"));
    Model1::DA.fill_registers(registers, offset, Some(1));

    locations.insert("model1::MN".into(), location(Model1::MN, offset));

    offset + usize::from(Model1::LEN)
}

pub fn add_model_103(
    registers: &mut [u16],
    base_offset: usize,
    _locations: &mut Locations,
) -> usize {
    registers[base_offset] = Model103::ID;
    registers[base_offset + 1] = Model103::LEN;

    let offset = base_offset + 2;
    Model103::W.fill_registers(registers, offset, 15000);
    Model103::W_SF.fill_registers(registers, offset, -1);
    Model103::ST.fill_registers(registers, offset, sunspec::models::model103::St::Mppt);

    offset + usize::from(Model103::LEN)
}

/// Appends Model 701 (DER AC Measurement - IEEE 1547 / DER)
pub fn add_model_701(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    registers[base_offset] = Model701::ID;
    registers[base_offset + 1] = Model701::LEN;

    let offset = base_offset + 2;
    Model701::W.fill_registers(registers, offset, Some(12500));
    locations.insert("model701::W".into(), location(Model701::W, offset));
    Model701::W_SF.fill_registers(registers, offset, Some(-1));

    Model701::VAR.fill_registers(registers, offset, Some(500));
    Model701::VAR_SF.fill_registers(registers, offset, Some(-1));

    Model701::HZ.fill_registers(registers, offset, Some(6000));
    Model701::HZ_SF.fill_registers(registers, offset, Some(-2));

    Model701::PF.fill_registers(registers, offset, Some(995));
    Model701::PF_SF.fill_registers(registers, offset, Some(-3));

    Model701::ST.fill_registers(registers, offset, Some(model701::St::On));
    locations.insert("model701::ST".into(), location(Model701::ST, offset));
    // Model701::CONN_ST.fill_registers(registers, offset, Some(model701::ConnSt::Disconnected));
    Model701::CONN_ST.fill_registers(registers, offset, None);
    locations.insert(
        "model701::CONN_ST".into(),
        location(Model701::CONN_ST, offset),
    );

    offset + usize::from(Model701::LEN)
}

pub fn add_model_702(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    registers[base_offset] = Model702::ID;
    registers[base_offset + 1] = Model702::LEN;

    let offset = base_offset + 2;
    Model702::W_MAX_RTG.fill_registers(registers, offset, Some(1000));
    locations.insert(
        "model702::W_MAX_RTG".into(),
        location(Model702::W_MAX_RTG, offset),
    );
    Model702::W_OVR_EXT_RTG.fill_registers(registers, offset, Some(1000));
    Model702::W_OVR_EXT_RTG_PF.fill_registers(registers, offset, Some(1));
    Model702::W_UND_EXT_RTG.fill_registers(registers, offset, Some(200));
    Model702::W_UND_EXT_RTG_PF.fill_registers(registers, offset, Some(0));
    Model702::VA_MAX_RTG.fill_registers(registers, offset, Some(5000));
    Model702::VAR_MAX_INJ_RTG.fill_registers(registers, offset, Some(400));
    Model702::VAR_MAX_ABS_RTG.fill_registers(registers, offset, Some(420));
    Model702::W_CHA_RTE_MAX_RTG.fill_registers(registers, offset, Some(630));
    Model702::VA_CHA_RTE_MAX_RTG.fill_registers(registers, offset, Some(650));
    Model702::V_NOM_RTG.fill_registers(registers, offset, Some(9000));
    Model702::V_MAX_RTG.fill_registers(registers, offset, Some(9100));
    Model702::V_MIN_RTG.fill_registers(registers, offset, Some(8900));
    Model702::CTRL_MODES.fill_registers(registers, offset, None);
    Model702::REACT_SUSCEPT_RTG.fill_registers(registers, offset, Some(1234));

    offset + usize::from(Model702::LEN)
}

pub fn add_model_703(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    registers[base_offset] = Model703::ID;
    registers[base_offset + 1] = Model703::LEN;

    let offset = base_offset + 2;
    Model703::ES.fill_registers(registers, offset, Some(model703::Es::Disabled));
    locations.insert("model703::ES".into(), location(Model703::ES, offset));
    Model703::ESV_HI.fill_registers(registers, offset, Some(42));
    locations.insert(
        "model703::ESV_HI".into(),
        location(Model703::ESV_HI, offset),
    );
    offset + usize::from(Model703::LEN)
}

pub fn add_model_713(
    registers: &mut [u16],
    base_offset: usize,
    _locations: &mut Locations,
) -> usize {
    registers[base_offset] = Model713::ID;
    registers[base_offset + 1] = Model713::LEN;

    let offset = base_offset + 2;
    Model713::SOC.fill_registers(registers, offset, Some(10));

    offset + usize::from(Model713::LEN)
}

/// Appends the End-of-Model Marker (0xFFFF)
pub fn add_end_of_model(registers: &mut [u16], base_offset: usize) {
    registers[base_offset] = 0xFFFF;
    registers[base_offset + 1] = 0;
}
