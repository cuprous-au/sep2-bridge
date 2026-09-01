// This is a helper module shared by multiple tests
#![allow(dead_code)]

use std::collections::HashMap;
use std::future::ready;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use sunspec::models::model1::Model1;
use sunspec::models::model103::Model103;
use sunspec::models::model701::{self, Model701};
use sunspec::models::model702::Model702;
use sunspec::models::model703::{self, Model703};
use sunspec::models::model704::{self, Model704};
use sunspec::models::model706::{self, Model706};
use sunspec::models::model707::{self, Model707};
use sunspec::models::model708::{self, Model708};
use sunspec::models::model709::{self, Model709};
use sunspec::models::model710::{self, Model710};
use sunspec::models::model711::{self, Model711};
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
        let mut registers = vec![0u16; 42000];
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

    /// Allows the ability to reconfigure the mock with a new model list.
    pub fn reinit(&mut self, enabled_models: Option<&[u32]>) {
        let locations = initialise_registers(
            &mut self
                .service_data
                .registers
                .lock()
                .expect("Unable to lock registers for reinit"),
            enabled_models,
        );
        self.locations = locations;
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
        T::decode(&registers[offset..offset + length])
            .unwrap_or_else(|err| panic!("Decode failure at {offset}+{length}: {err}"))
    }

    pub fn get_value_at_addr<T: Value>(&self, addr: usize, length: usize) -> T {
        let registers = self.service_data.registers.lock().expect("Mutex failure");
        T::decode(&registers[addr..addr + length])
            .unwrap_or_else(|err| panic!("Decode failure at {addr}+{length}: {err}"))
    }

    pub fn get_name_addr(&self, name: &str) -> usize {
        let (offset, _) = *self
            .locations
            .get(name)
            .unwrap_or_else(|| panic!("Unknown location {}", name));
        offset
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
impl<G: Group, U: Value> PointWrite<U> for Point<G, U> {
    fn fill_registers(self: &Point<G, U>, registers: &mut [u16], base: usize, value: U) -> usize {
        let start = base + (self.offset as usize);
        let value_end = start + (self.length as usize);
        let words = value.encode();
        let payload_end = start + words.len();
        registers[start..payload_end].copy_from_slice(&words);

        value_end
    }
}

type Locations = HashMap<String, (usize, usize)>;
fn location<G: Group, U: Value>(point: Point<G, U>, base: usize) -> (usize, usize) {
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
        if enabled_models.is_none_or(|v| v.contains(&704)) {
            offset = add_model_704(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&706)) {
            offset = add_model_706(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&707)) {
            offset = add_model_707(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&708)) {
            offset = add_model_708(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&709)) {
            offset = add_model_709(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&710)) {
            offset = add_model_710(registers, offset, &mut locations);
        }
        if enabled_models.is_none_or(|v| v.contains(&711)) {
            offset = add_model_711(registers, offset, &mut locations);
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
    Model703::ESV_LO.fill_registers(registers, offset, Some(43));
    locations.insert(
        "model703::ESV_LO".into(),
        location(Model703::ESV_LO, offset),
    );
    Model703::ES_HZ_HI.fill_registers(registers, offset, Some(44));
    locations.insert(
        "model703::ES_HZ_HI".into(),
        location(Model703::ES_HZ_HI, offset),
    );
    Model703::ES_HZ_LO.fill_registers(registers, offset, Some(45));
    locations.insert(
        "model703::ES_HZ_LO".into(),
        location(Model703::ES_HZ_LO, offset),
    );
    Model703::ES_DLY_TMS.fill_registers(registers, offset, Some(46));
    locations.insert(
        "model703::ES_DLY_TMS".into(),
        location(Model703::ES_DLY_TMS, offset),
    );
    Model703::ES_RND_TMS.fill_registers(registers, offset, Some(47));
    locations.insert(
        "model703::ES_RND_TMS".into(),
        location(Model703::ES_RND_TMS, offset),
    );
    Model703::ES_RMP_TMS.fill_registers(registers, offset, Some(48));
    locations.insert(
        "model703::ES_RMP_TMS".into(),
        location(Model703::ES_RMP_TMS, offset),
    );

    // Deliberately not the scale factors SEP2 fixes its own values at, so that
    // anything written here has to be rescaled to land correctly.
    Model703::V_SF.fill_registers(registers, offset, Some(-1));
    locations.insert("model703::V_SF".into(), location(Model703::V_SF, offset));
    Model703::HZ_SF.fill_registers(registers, offset, Some(-3));
    locations.insert("model703::HZ_SF".into(), location(Model703::HZ_SF, offset));

    offset + usize::from(Model703::LEN)
}

/// Appends Model 704 (DER AC Controls)
///
/// The model contains four nested (non-repeating) groups after its fixed part,
/// so the declared length covers those too.
pub fn add_model_704(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    let length = Model704::LEN
        + model704::PfwInj::LEN
        + model704::PfwInjRvrt::LEN
        + model704::PfwAbs::LEN
        + model704::PfwAbsRvrt::LEN;

    registers[base_offset] = Model704::ID;
    registers[base_offset + 1] = length;

    let offset = base_offset + 2;

    // AS5438 - Table 11
    Model704::W_MAX_LIM_PCT_ENA.fill_registers(
        registers,
        offset,
        Some(model704::WMaxLimPctEna::Disabled),
    );
    locations.insert(
        "model704::W_MAX_LIM_PCT_ENA".into(),
        location(Model704::W_MAX_LIM_PCT_ENA, offset),
    );
    Model704::W_MAX_LIM_PCT.fill_registers(registers, offset, Some(0));
    locations.insert(
        "model704::W_MAX_LIM_PCT".into(),
        location(Model704::W_MAX_LIM_PCT, offset),
    );
    // Deliberately not the -2 SEP2 fixes its percentages at, so that a value
    // written here has to be rescaled to land correctly.
    Model704::W_MAX_LIM_PCT_SF.fill_registers(registers, offset, Some(0));
    locations.insert(
        "model704::W_MAX_LIM_PCT_SF".into(),
        location(Model704::W_MAX_LIM_PCT_SF, offset),
    );

    // AS5438 - Table 12
    Model704::W_SET_ENA.fill_registers(registers, offset, Some(model704::WSetEna::Disabled));
    locations.insert(
        "model704::W_SET_ENA".into(),
        location(Model704::W_SET_ENA, offset),
    );
    Model704::W_SET_MOD.fill_registers(registers, offset, None);
    locations.insert(
        "model704::W_SET_MOD".into(),
        location(Model704::W_SET_MOD, offset),
    );
    Model704::W_SET_PCT.fill_registers(registers, offset, Some(0));
    locations.insert(
        "model704::W_SET_PCT".into(),
        location(Model704::W_SET_PCT, offset),
    );
    Model704::W_SET_PCT_SF.fill_registers(registers, offset, Some(-1));
    locations.insert(
        "model704::W_SET_PCT_SF".into(),
        location(Model704::W_SET_PCT_SF, offset),
    );
    Model704::W_SET.fill_registers(registers, offset, None);
    locations.insert("model704::W_SET".into(), location(Model704::W_SET, offset));
    Model704::W_SET_SF.fill_registers(registers, offset, Some(-2));
    locations.insert(
        "model704::W_SET_SF".into(),
        location(Model704::W_SET_SF, offset),
    );

    offset + usize::from(length)
}

/// Appends Model 706 (DER Trip low voltage)
///
/// This model contains curves which are a repeating group, and these themselves
/// contain points which are repeating groups.
pub fn add_model_706(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    let n_crv = 2;
    let n_pt = 4;

    // Assuming MustTrip, MayTrip, MomCess are all the same layout.
    let crv_len = model706::Crv::LEN + model706::Pt::LEN * n_pt;
    let length = Model706::LEN + crv_len * n_crv;

    registers[base_offset] = Model706::ID;
    registers[base_offset + 1] = length;

    let offset = base_offset + 2;

    Model706::ENA.fill_registers(registers, offset, model706::Ena::Disabled);
    locations.insert("model706::ENA".into(), location(Model706::ENA, offset));
    Model706::ADPT_CRV_REQ.fill_registers(registers, offset, 1);
    locations.insert(
        "model706::ADPT_CRV_REQ".into(),
        location(Model706::ADPT_CRV_REQ, offset),
    );
    Model706::N_PT.fill_registers(registers, offset, n_pt);
    locations.insert("model706::N_PT".into(), location(Model706::N_PT, offset));
    Model706::N_CRV.fill_registers(registers, offset, n_crv);
    locations.insert("model706::N_CRV".into(), location(Model706::N_CRV, offset));
    Model706::V_SF.fill_registers(registers, offset, 1);
    locations.insert("model706::V_SF".into(), location(Model706::V_SF, offset));
    Model706::DEPT_REF_SF.fill_registers(registers, offset, 2);
    locations.insert(
        "model706::DEPT_REF_SF".into(),
        location(Model706::DEPT_REF_SF, offset),
    );

    // Ensure the 1st curve is readonly
    model706::Crv::READ_ONLY.fill_registers(
        registers,
        offset + usize::from(Model706::LEN),
        model706::CrvReadOnly::R,
    );

    // Skip to the 2nd curve, skipping past the rw point.
    let crv_offset = offset + usize::from(Model706::LEN + crv_len);
    // Record the curve location and the Tms location
    locations.insert(
        "model706::Crv_1_ActPt".into(),
        location(model706::Crv::ACT_PT, crv_offset),
    );
    locations.insert(
        "model706::Crv_1_RspTms".into(),
        location(model706::Crv::RSP_TMS, crv_offset),
    );
    // The location of the data will have to be worked out by the user.

    offset + usize::from(length)
}

/// Appends Model 707 (DER Trip low voltage)
///
/// This model contains curves which are a repeating group, and these themselves
/// contain points which are repeating groups.
pub fn add_model_707(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    let n_crv_set = 2;
    let n_pt = 4;

    // Assuming MustTrip, MayTrip, MomCess are all the same layout.
    let curve_len = model707::MustTrip::LEN + model707::Pt::LEN * n_pt;
    let crv_set_len = model707::Crv::LEN + 3 * curve_len;
    let length = Model707::LEN + crv_set_len * n_crv_set;

    registers[base_offset] = Model707::ID;
    registers[base_offset + 1] = length;

    let offset = base_offset + 2;

    Model707::ENA.fill_registers(registers, offset, model707::Ena::Disabled);
    locations.insert("model707::ENA".into(), location(Model707::ENA, offset));
    Model707::ADPT_CRV_REQ.fill_registers(registers, offset, 1);
    locations.insert(
        "model707::ADPT_CRV_REQ".into(),
        location(Model707::ADPT_CRV_REQ, offset),
    );
    Model707::N_PT.fill_registers(registers, offset, n_pt);
    locations.insert("model707::N_PT".into(), location(Model707::N_PT, offset));
    Model707::N_CRV_SET.fill_registers(registers, offset, n_crv_set);
    locations.insert(
        "model707::N_CRV_SET".into(),
        location(Model707::N_CRV_SET, offset),
    );
    Model707::V_SF.fill_registers(registers, offset, 1);
    locations.insert("model707::V_SF".into(), location(Model707::V_SF, offset));
    Model707::TMS_SF.fill_registers(registers, offset, 2);
    locations.insert(
        "model707::TMS_SF".into(),
        location(Model707::TMS_SF, offset),
    );

    // Ensure the 1st curve is readonly
    model707::Crv::READ_ONLY.fill_registers(
        registers,
        offset + usize::from(Model707::LEN),
        model707::CrvReadOnly::R,
    );

    // Skip to the 2nd curve, skipping past the rw point.
    let crv_offset = offset + usize::from(Model707::LEN + crv_set_len);
    let must_trip_offset = crv_offset + usize::from(model707::Crv::LEN);
    let may_trip_offset = must_trip_offset + usize::from(curve_len);
    let mom_cess_offset = may_trip_offset + usize::from(curve_len);
    // Don't fill any curve data but just record the MustTrip and MomCess locations so we can look up values later.
    locations.insert(
        "model707::Crv_1_MustTrip".into(),
        location(model707::MustTrip::ACT_PT, must_trip_offset),
    );
    locations.insert(
        "model707::Crv_1_MayTrip".into(),
        location(model709::MayTrip::ACT_PT, may_trip_offset),
    );
    locations.insert(
        "model707::Crv_1_MomCess".into(),
        location(model707::MomCess::ACT_PT, mom_cess_offset),
    );

    offset + usize::from(length)
}

/// Appends Model 708 (DER Trip high voltage)
///
/// This model contains curves which are a repeating group, and these themselves
/// contain points which are repeating groups.
pub fn add_model_708(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    let n_crv_set = 2;
    let n_pt = 4;

    // Assuming MustTrip, MayTrip, MomCess are all the same layout.
    let curve_len = model708::MustTrip::LEN + model708::Pt::LEN * n_pt;
    let crv_set_len = model708::Crv::LEN + 3 * curve_len;
    let length = Model708::LEN + crv_set_len * n_crv_set;

    registers[base_offset] = Model708::ID;
    registers[base_offset + 1] = length;

    let offset = base_offset + 2;

    Model708::ENA.fill_registers(registers, offset, model708::Ena::Disabled);
    locations.insert("model708::ENA".into(), location(Model708::ENA, offset));
    Model708::ADPT_CRV_REQ.fill_registers(registers, offset, 1);
    locations.insert(
        "model708::ADPT_CRV_REQ".into(),
        location(Model708::ADPT_CRV_REQ, offset),
    );
    Model708::N_PT.fill_registers(registers, offset, n_pt);
    locations.insert("model708::N_PT".into(), location(Model708::N_PT, offset));
    Model708::N_CRV_SET.fill_registers(registers, offset, n_crv_set);
    locations.insert(
        "model708::N_CRV_SET".into(),
        location(Model708::N_CRV_SET, offset),
    );
    Model708::V_SF.fill_registers(registers, offset, 1);
    locations.insert("model708::V_SF".into(), location(Model708::V_SF, offset));
    Model708::TMS_SF.fill_registers(registers, offset, 2);
    locations.insert(
        "model708::TMS_SF".into(),
        location(Model708::TMS_SF, offset),
    );

    // Ensure the 1st curve is readonly
    model708::Crv::READ_ONLY.fill_registers(
        registers,
        offset + usize::from(Model708::LEN),
        model708::CrvReadOnly::R,
    );

    // Skip to the 2nd curve, skipping past the rw point.
    let crv_offset = offset + usize::from(Model708::LEN + crv_set_len);
    let must_trip_offset = crv_offset + usize::from(model708::Crv::LEN);
    let may_trip_offset = must_trip_offset + usize::from(curve_len);
    let mom_cess_offset = may_trip_offset + usize::from(curve_len);
    // Don't fill any curve data but just record the MustTrip and MomCess locations so we can look up values later.
    locations.insert(
        "model708::Crv_1_MustTrip".into(),
        location(model708::MustTrip::ACT_PT, must_trip_offset),
    );
    locations.insert(
        "model708::Crv_1_MayTrip".into(),
        location(model709::MayTrip::ACT_PT, may_trip_offset),
    );
    locations.insert(
        "model708::Crv_1_MomCess".into(),
        location(model708::MomCess::ACT_PT, mom_cess_offset),
    );

    offset + usize::from(length)
}

/// Appends Model 709 (DER Trip low frequency)
///
/// This model contains curves which are a repeating group, and these themselves
/// contain points which are repeating groups.
pub fn add_model_709(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    let n_crv_set = 2;
    let n_pt = 4;

    // Assuming MustTrip, MayTrip, MomCess are all the same layout.
    let crv_len = model709::Crv::LEN + 3 * (model709::MustTrip::LEN + model709::Pt::LEN * n_pt);
    let length = Model709::LEN + crv_len * n_crv_set;

    registers[base_offset] = Model709::ID;
    registers[base_offset + 1] = length;

    let offset = base_offset + 2;

    Model709::ENA.fill_registers(registers, offset, model709::Ena::Disabled);
    locations.insert("model709::ENA".into(), location(Model709::ENA, offset));
    Model709::ADPT_CRV_REQ.fill_registers(registers, offset, 1);
    locations.insert(
        "model709::ADPT_CRV_REQ".into(),
        location(Model709::ADPT_CRV_REQ, offset),
    );
    Model709::N_PT.fill_registers(registers, offset, n_pt);
    locations.insert("model709::N_PT".into(), location(Model709::N_PT, offset));
    Model709::N_CRV_SET.fill_registers(registers, offset, n_crv_set);
    locations.insert(
        "model709::N_CRV_SET".into(),
        location(Model709::N_CRV_SET, offset),
    );
    Model709::HZ_SF.fill_registers(registers, offset, 1);
    locations.insert("model709::HZ_SF".into(), location(Model709::HZ_SF, offset));
    Model709::TMS_SF.fill_registers(registers, offset, 2);
    locations.insert(
        "model709::TMS_SF".into(),
        location(Model709::TMS_SF, offset),
    );

    // Ensure the 1st curve is readonly
    model709::Crv::READ_ONLY.fill_registers(
        registers,
        offset + usize::from(Model709::LEN),
        model709::CrvReadOnly::R,
    );

    // Skip to the 2nd curve, skipping past the rw point.
    let curve_offset = offset + usize::from(Model709::LEN + crv_len + model709::Crv::LEN);
    // Don't fill any curve data but just record this location so we can look up values later.
    locations.insert(
        "model709::Crv_1_ActPt".into(),
        location(model709::MustTrip::ACT_PT, curve_offset),
    );

    offset + usize::from(length)
}

/// Appends Model 710 (DER Trip high frequency)
///
/// This model contains curves which are a repeating group, and these themselves
/// contain points which are repeating groups.
pub fn add_model_710(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    let n_crv_set = 3;
    let n_pt = 6;

    // Assuming MustTrip, MayTrip, MomCess are all the same layout.
    let crv_len = model710::Crv::LEN + 3 * (model710::MustTrip::LEN + model710::Pt::LEN * n_pt);
    let length = Model710::LEN + crv_len * n_crv_set;

    registers[base_offset] = Model710::ID;
    registers[base_offset + 1] = length;

    let offset = base_offset + 2;

    Model710::ENA.fill_registers(registers, offset, model710::Ena::Disabled);
    locations.insert("model710::ENA".into(), location(Model710::ENA, offset));
    Model710::ADPT_CRV_REQ.fill_registers(registers, offset, 1);
    locations.insert(
        "model710::ADPT_CRV_REQ".into(),
        location(Model710::ADPT_CRV_REQ, offset),
    );
    Model710::N_PT.fill_registers(registers, offset, n_pt);
    locations.insert("model710::N_PT".into(), location(Model710::N_PT, offset));
    Model710::N_CRV_SET.fill_registers(registers, offset, n_crv_set);
    locations.insert(
        "model710::N_CRV_SET".into(),
        location(Model710::N_CRV_SET, offset),
    );
    Model710::HZ_SF.fill_registers(registers, offset, 1);
    locations.insert("model710::HZ_SF".into(), location(Model710::HZ_SF, offset));
    Model710::TMS_SF.fill_registers(registers, offset, 2);
    locations.insert(
        "model710::TMS_SF".into(),
        location(Model710::TMS_SF, offset),
    );

    // Ensure the 1st curve is readonly
    model710::Crv::READ_ONLY.fill_registers(
        registers,
        offset + usize::from(Model710::LEN),
        model710::CrvReadOnly::R,
    );

    // Skip to the 2nd curve, skipping past the rw point.
    let curve_offset = offset + usize::from(Model710::LEN + crv_len + model710::Crv::LEN);
    // Don't fill any curve data but just record this location so we can look up values later.
    locations.insert(
        "model710::Crv_1_ActPt".into(),
        location(model710::MustTrip::ACT_PT, curve_offset),
    );

    offset + usize::from(length)
}

/// Appends Model 711 (DER Frequency Droop)
///
/// The model has a repeating `Ctl` group; we provide two of them as the first
/// is read-only by the SunSpec spec and represents the current settings. The
/// second group's points are exposed as locations, as that is where the bridge
/// writes, along with one point of the first group so tests can check it is
/// left alone.
pub fn add_model_711(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    const N_CTL: u16 = 2;
    let length = Model711::LEN + N_CTL * model711::Ctl::LEN;

    registers[base_offset] = Model711::ID;
    registers[base_offset + 1] = length;

    let offset = base_offset + 2;
    Model711::ENA.fill_registers(registers, offset, model711::Ena::Disabled);
    locations.insert("model711::ENA".into(), location(Model711::ENA, offset));
    Model711::ADPT_CTL_REQ.fill_registers(registers, offset, 1);
    locations.insert(
        "model711::ADPT_CTL_REQ".into(),
        location(Model711::ADPT_CTL_REQ, offset),
    );
    Model711::N_CTL.fill_registers(registers, offset, N_CTL);
    // Deliberately not the scale factors SEP2 fixes its droop values at, so
    // that anything written here has to be rescaled to land correctly. One of
    // each direction, so a rescale the wrong way cannot pass.
    Model711::DB_SF.fill_registers(registers, offset, -2);
    locations.insert("model711::DB_SF".into(), location(Model711::DB_SF, offset));
    Model711::K_SF.fill_registers(registers, offset, -4);
    locations.insert("model711::K_SF".into(), location(Model711::K_SF, offset));
    Model711::RSP_TMS_SF.fill_registers(registers, offset, 0);
    locations.insert(
        "model711::RSP_TMS_SF".into(),
        location(Model711::RSP_TMS_SF, offset),
    );

    // The first control group is read-only and reports the current settings.
    let ctl_0 = offset + usize::from(Model711::LEN);
    fill_ctl_group(registers, ctl_0, model711::CtlReadOnly::R);

    // The second control group is the writable one.
    let ctl_1 = ctl_0 + usize::from(model711::Ctl::LEN);
    fill_ctl_group(registers, ctl_1, model711::CtlReadOnly::Rw);
    for (name, point_location) in [
        (
            "model711::CTL_1::DB_OF",
            location(model711::Ctl::DB_OF, ctl_1),
        ),
        (
            "model711::CTL_1::DB_UF",
            location(model711::Ctl::DB_UF, ctl_1),
        ),
        (
            "model711::CTL_1::K_OF",
            location(model711::Ctl::K_OF, ctl_1),
        ),
        (
            "model711::CTL_1::K_UF",
            location(model711::Ctl::K_UF, ctl_1),
        ),
        (
            "model711::CTL_1::RSP_TMS",
            location(model711::Ctl::RSP_TMS, ctl_1),
        ),
        (
            "model711::CTL_1::READ_ONLY",
            location(model711::Ctl::READ_ONLY, ctl_1),
        ),
        (
            "model711::CTL_0::DB_OF",
            location(model711::Ctl::DB_OF, ctl_0),
        ),
    ] {
        locations.insert(name.into(), point_location);
    }

    offset + usize::from(length)
}

/// Fills a single model 711 `Ctl` group with zeroed control values.
fn fill_ctl_group(registers: &mut [u16], base: usize, read_only: model711::CtlReadOnly) {
    model711::Ctl::DB_OF.fill_registers(registers, base, 0);
    model711::Ctl::DB_UF.fill_registers(registers, base, 0);
    model711::Ctl::K_OF.fill_registers(registers, base, 0);
    model711::Ctl::K_UF.fill_registers(registers, base, 0);
    model711::Ctl::RSP_TMS.fill_registers(registers, base, 0);
    model711::Ctl::P_MIN.fill_registers(registers, base, None);
    model711::Ctl::READ_ONLY.fill_registers(registers, base, read_only);
}

pub fn add_model_713(
    registers: &mut [u16],
    base_offset: usize,
    locations: &mut Locations,
) -> usize {
    registers[base_offset] = Model713::ID;
    registers[base_offset + 1] = Model713::LEN;

    let offset = base_offset + 2;
    Model713::SOC.fill_registers(registers, offset, Some(10));
    locations.insert("model713::SOC".into(), location(Model713::SOC, offset));

    // Deliberately not the -2 SEP2 fixes its percentages at, so that the state
    // of charge has to be rescaled on the way out.
    Model713::PCT_SF.fill_registers(registers, offset, Some(-1));
    locations.insert(
        "model713::PCT_SF".into(),
        location(Model713::PCT_SF, offset),
    );

    offset + usize::from(Model713::LEN)
}

/// Appends the End-of-Model Marker (0xFFFF)
pub fn add_end_of_model(registers: &mut [u16], base_offset: usize) {
    registers[base_offset] = 0xFFFF;
    registers[base_offset + 1] = 0;
}
