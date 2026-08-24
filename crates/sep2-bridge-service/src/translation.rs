// Translation between SEP2 and Modbus

use chrono::Utc;
use derive_more::Display;
use sep2_common::packages::{
    der::{
        ActivePower, ApparentPower, ConnectStatusType, ConnectStatusValue, DERAlarmStatus,
        DERCapability, DERControlType, DERSettings, DERStatus, OperationalModeStatusType,
        OperationalModeStatusValue, PowerFactor, ReactivePower, ReactiveSusceptance,
        StateOfChargeStatusType, VoltageRMS,
    },
    metering::{Reading, ReadingType},
    metering_mirror::MirrorMeterReading,
    primitives::{Int16, Int48, Int64, String32, Uint16, Uint32},
    types::{
        AccumulationBehaviourType, CommodityType, DateTimeInterval, FlowDirectionType, KindType,
        MRIDType, Percent, PhaseCode, PowerOfTenMultiplierType, UomType,
    },
};
use sunspec::models::{model701, model702::CtrlModes, model703};

use crate::{
    modbus_connection::{
        Capabilities as ModbusCapabilities, Metering as ModbusMetering,
        Parameters as ModbusParameters, PhaseReference, Settings as ModbusSettings,
        Status as ModbusStatus, VoltageWithReference,
    },
    scheduler::ControlAttributes,
};

pub type Result<T> = std::result::Result<T, NamedError>;

#[derive(Clone, Debug)]
pub struct NamedError {
    kind: Error,
    name: &'static str,
}

impl std::error::Error for NamedError {}

impl std::fmt::Display for NamedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} when translating to field {}", self.kind, self.name)
    }
}

#[derive(Clone, Debug, Display)]
pub enum Error {
    UnsignedNegative,
    SignedOverflow,
    MandatoryNone,
    UnmappableInvalid,
    Unknown,
}

impl std::error::Error for Error {}

impl Error {
    fn name(self, name: &'static str) -> NamedError {
        NamedError { kind: self, name }
    }
}

/// The public facing conversion trait.
pub trait TryConvert<T> {
    fn try_convert(self) -> Result<T>;
}

impl TryConvert<DERCapability> for ModbusCapabilities {
    fn try_convert(self: ModbusCapabilities) -> Result<DERCapability> {
        Ok(DERCapability {
            rtg_max_w: self
                .w_max_rtg
                .try_convert_mandatory()
                .map_err(|err| err.name("w_max_rtg"))?,
            rtg_over_excited_w: self
                .w_ovr_ext_rtg
                .try_convert()
                .map_err(|err| err.name("rtg_over_excited_w"))?,
            rtg_over_excited_pf: self.w_ovr_ext_rtg_pf.convert(),
            rtg_under_excited_w: self
                .w_und_ext_rtg
                .try_convert()
                .map_err(|err| err.name("rtg_under_excited_w"))?,
            rtg_under_excited_pf: self.w_und_ext_rtg_pf.convert(),
            rtg_max_va: self.va_max_rtg.convert(),
            rtg_max_var: self
                .var_max_inj_rtg
                .try_convert()
                .map_err(|err| err.name("var_max_inj_rtg"))?,
            rtg_max_var_neg: self
                .var_max_abs_rtg
                .try_convert()
                .map_err(|err| err.name("var_max_abs_rtg"))?,
            rtg_max_charge_rate_w: self
                .w_cha_rte_max_rtg
                .try_convert()
                .map_err(|err| err.name("w_cha_rte_max_rtg"))?,
            rtg_max_charge_rate_va: self.va_cha_rte_max_rtg.convert(),
            rtg_v_nom: self.v_nom_rtg.convert(),
            rtg_max_v: self.v_max_rtg.convert(),
            rtg_min_v: self.v_min_rtg.convert(),
            modes_supported: self
                .ctrl_modes
                .try_convert()
                .map_err(|err| err.name("modes_supported"))?,
            rtg_reactive_susceptance: self.react_suscept_rtg.convert(),
            ..Default::default()
        })
    }
}

impl TryConvert<DERStatus> for ModbusStatus {
    fn try_convert(self: ModbusStatus) -> Result<DERStatus> {
        // TODO: Work out how to translate conn_st, should it be gen_connect_status or stor_connect_status?
        if self.conn_st.is_some() {
            return Err(Error::Unknown.name("conn_st"));
        }
        Ok(DERStatus {
            operational_mode_status: self
                .st
                .try_convert()
                .map_err(|err| err.name("operational_mode_status"))?,
            gen_connect_status: None,
            stor_connect_status: None,
            alarm_status: self
                .alrm
                .try_convert()
                .map_err(|err| err.name("alarm_status"))?,
            state_of_charge_status: self.soc.convert(),
            ..Default::default()
        })
    }
}

impl TryConvert<DERSettings> for ModbusSettings {
    fn try_convert(self: ModbusSettings) -> Result<DERSettings> {
        Ok(DERSettings {
            set_es_high_volt: self
                .esv_hi
                .try_convert()
                .map_err(|err| err.name("set_es_high_volt"))?,
            ..Default::default()
        })
    }
}

impl TryConvert<Vec<MirrorMeterReading>> for ModbusMetering {
    fn try_convert(self: ModbusMetering) -> Result<Vec<MirrorMeterReading>> {
        let now = Int64(Utc::now().timestamp());

        let template_reading_type = ReadingType {
            accumulation_behaviour: Some(AccumulationBehaviourType::Instantaneous),
            commodity: Some(CommodityType::ElectricitySecondaryMetered),
            ..Default::default()
        };
        let template_reading = Reading {
            time_period: Some(DateTimeInterval {
                start: now,
                duration: Uint32(0),
            }),
            ..Default::default()
        };
        let template = MirrorMeterReading {
            // We leave the MRID to be assigned by the sep2_connection task.
            // However, it is mandatory in the struct so we assign a default.
            mrid: MRIDType(0),
            ..Default::default()
        };

        let active_power = self.active_power.map(|value| MirrorMeterReading {
            description: Some(String32("active_power".into())),
            reading_type: Some(ReadingType {
                flow_direction: Some(FlowDirectionType::Reverse),
                kind: Some(KindType::Power),
                uom: Some(UomType::W),
                ..template_reading_type.clone()
            }),
            reading: Some(Reading {
                value: Some(Int48(i64::from(value))),
                ..template_reading.clone()
            }),
            ..template.clone()
        });
        let reactive_power = self.reactive_power.map(|value| MirrorMeterReading {
            description: Some(String32("reactive_power".into())),
            reading_type: Some(ReadingType {
                flow_direction: Some(FlowDirectionType::Reverse),
                kind: Some(KindType::Power),
                uom: Some(UomType::VAr),
                ..template_reading_type.clone()
            }),
            reading: Some(Reading {
                value: Some(Int48(i64::from(value))),
                ..template_reading.clone()
            }),
            ..template.clone()
        });
        let voltages = self
            .voltages
            .iter()
            .map(|VoltageWithReference(v, phase)| {
                let name = format!("voltage_{}", phase);
                Ok(MirrorMeterReading {
                    description: Some(String32(name.clone())),
                    reading_type: Some(ReadingType {
                        flow_direction: Some(FlowDirectionType::Forward),
                        phase: Some(phase.try_convert().map_err(|err| err.name("voltages"))?),
                        uom: Some(UomType::Voltage),
                        ..template_reading_type.clone()
                    }),
                    reading: Some(Reading {
                        value: Some(Int48(i64::from(*v))),
                        ..template_reading.clone()
                    }),
                    ..template.clone()
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let frequency = self.frequency.map(|value| MirrorMeterReading {
            description: Some(String32("frequency".into())),
            reading_type: Some(ReadingType {
                flow_direction: Some(FlowDirectionType::Reverse),
                uom: Some(UomType::Hz),
                ..template_reading_type.clone()
            }),
            reading: Some(Reading {
                value: Some(Int48(i64::from(value))),
                ..template_reading.clone()
            }),
            ..template.clone()
        });

        Ok(vec![active_power, reactive_power, frequency]
            .into_iter()
            .flatten()
            .chain(voltages)
            .collect())
    }
}

impl TryConvert<ModbusParameters> for ControlAttributes {
    fn try_convert(self: ControlAttributes) -> Result<ModbusParameters> {
        let es = self.base.op_mod_connect.map(|connect| match connect {
            false => model703::Es::Disabled,
            true => model703::Es::Enabled,
        });
        Ok(ModbusParameters {
            es,
            esvhi: self
                .set_es_high_volt
                .try_convert()
                .map_err(|err| err.name("esvhi"))?,
        })
    }
}

//////
// Internals

// Internal conversions care only about the types U->T not the names of the
// fields. Hence they return an unnamed error type. They can:
//
// a) have no errors: .convert()
// b) potentially error: .try_convert()
// c) be an Option<T>->U conversion: .try_convert_mandatory()

type ResultUnnamed<T> = std::result::Result<T, Error>;

// A local trait to make writing out the conversions easier.
trait Convert<T> {
    fn convert(self) -> T;
}

trait TryConvertUnnamed<T> {
    fn try_convert(self) -> ResultUnnamed<T>;
}

impl<T, U: Convert<T>> TryConvertUnnamed<T> for U {
    fn try_convert(self) -> ResultUnnamed<T> {
        Ok(self.convert())
    }
}

// Specialised trait for options to avoid getting tied in knots with nested traits.
trait OptionConvert<T, U: TryConvertUnnamed<T>> {
    fn try_convert(self) -> ResultUnnamed<Option<T>>;
    // Conversions to error on missing values.
    fn try_convert_mandatory(self) -> ResultUnnamed<T>;
    // Conversions without errors
    fn convert(self) -> Option<T>
    where
        U: Convert<T>;
}
impl<T, U> OptionConvert<T, U> for Option<U>
where
    U: TryConvertUnnamed<T>,
{
    fn try_convert_mandatory(self: Option<U>) -> ResultUnnamed<T> {
        match self {
            None => Err(Error::MandatoryNone),
            Some(val) => val.try_convert(),
        }
    }

    fn try_convert(self: Option<U>) -> ResultUnnamed<Option<T>> {
        self.map(|inner| inner.try_convert()).transpose()
    }

    fn convert(self: Option<U>) -> Option<T>
    where
        U: Convert<T>,
    {
        self.map(|inner| inner.convert())
    }
}

//////
// Internals for converting to SEP2.

impl TryConvertUnnamed<Int16> for u16 {
    fn try_convert(self: u16) -> ResultUnnamed<Int16> {
        Ok(Int16(
            i16::try_from(self).map_err(|_| Error::SignedOverflow)?,
        ))
    }
}

impl TryConvertUnnamed<ActivePower> for u16 {
    fn try_convert(self: u16) -> ResultUnnamed<ActivePower> {
        Ok(ActivePower {
            value: self.try_convert()?,
            multiplier: PowerOfTenMultiplierType::None,
        })
    }
}

impl Convert<PowerFactor> for u16 {
    fn convert(self: u16) -> PowerFactor {
        PowerFactor {
            displacement: Uint16(self),
            multiplier: PowerOfTenMultiplierType::None,
        }
    }
}

impl Convert<ApparentPower> for u16 {
    fn convert(self: u16) -> ApparentPower {
        ApparentPower {
            value: Uint16(self),
            multiplier: PowerOfTenMultiplierType::None,
        }
    }
}

impl TryConvertUnnamed<ReactivePower> for u16 {
    fn try_convert(self: u16) -> ResultUnnamed<ReactivePower> {
        Ok(ReactivePower {
            value: Int16(i16::try_from(self).map_err(|_| Error::SignedOverflow)?),
            multiplier: PowerOfTenMultiplierType::None,
        })
    }
}

impl Convert<VoltageRMS> for u16 {
    fn convert(self: u16) -> VoltageRMS {
        VoltageRMS {
            value: Uint16(self),
            multiplier: PowerOfTenMultiplierType::None,
        }
    }
}

impl TryConvertUnnamed<DERControlType> for Option<CtrlModes> {
    fn try_convert(self: Option<CtrlModes>) -> ResultUnnamed<DERControlType> {
        match self {
            None => Ok(DERControlType::empty()),
            Some(_todo) => Err(Error::Unknown),
        }
    }
}

impl Convert<ReactiveSusceptance> for u16 {
    fn convert(self: u16) -> ReactiveSusceptance {
        ReactiveSusceptance {
            value: Uint16(self),
            multiplier: PowerOfTenMultiplierType::None,
        }
    }
}

impl TryConvertUnnamed<OperationalModeStatusType> for model701::St {
    fn try_convert(self: model701::St) -> ResultUnnamed<OperationalModeStatusType> {
        Ok(OperationalModeStatusType {
            date_time: Int64(Utc::now().timestamp()),
            value: match self {
                model701::St::Off => OperationalModeStatusValue::Off,
                model701::St::On => OperationalModeStatusValue::Operational,
                model701::St::Invalid(_) => Err(Error::UnmappableInvalid)?,
            },
        })
    }
}

impl TryConvertUnnamed<ConnectStatusType> for model701::ConnSt {
    fn try_convert(self: model701::ConnSt) -> ResultUnnamed<ConnectStatusType> {
        Ok(ConnectStatusType {
            date_time: Int64(Utc::now().timestamp()),
            value: match self {
                model701::ConnSt::Disconnected => ConnectStatusValue::empty(),
                model701::ConnSt::Connected => {
                    // TODO: Figure out what we say exactly here
                    Err(Error::Unknown)?
                }
                model701::ConnSt::Invalid(_) => Err(Error::UnmappableInvalid)?,
            },
        })
    }
}

impl TryConvertUnnamed<DERAlarmStatus> for model701::Alrm {
    fn try_convert(self: model701::Alrm) -> ResultUnnamed<DERAlarmStatus> {
        Ok(self
            .iter()
            .map(|flag| {
                match flag {
                    model701::Alrm::DcOverVolt => Ok(DERAlarmStatus::DER_FAULT_OVER_VOLTAGE),
                    model701::Alrm::ManualShutdown => Ok(DERAlarmStatus::DER_FAULT_EMERGENCY_LOCAL),
                    model701::Alrm::OverFrequency => Ok(DERAlarmStatus::DER_FAULT_OVER_FREQUENCY),
                    model701::Alrm::UnderFrequency => Ok(DERAlarmStatus::DER_FAULT_UNDER_FREQUENCY),
                    model701::Alrm::AcOverVolt => Ok(DERAlarmStatus::DER_FAULT_OVER_VOLTAGE),
                    model701::Alrm::AcUnderVolt => Ok(DERAlarmStatus::DER_FAULT_UNDER_VOLTAGE),
                    model701::Alrm::OverTemp
                    | model701::Alrm::AcDisconnect
                    | model701::Alrm::DcDisconnect
                    | model701::Alrm::GridDisconnect
                    | model701::Alrm::CabinetOpen
                    | model701::Alrm::GroundFault
                    | model701::Alrm::BlownStringFuse
                    | model701::Alrm::UnderTemp
                    | model701::Alrm::MemoryLoss
                    | model701::Alrm::HwTestFailure
                    | model701::Alrm::ManufacturerAlrm => {
                        // TODO how to translate these?
                        Err(Error::Unknown)
                    }
                    // FIXME: why is the enum not exhausted? what are the other flags?
                    // There are potentially other bits which are unknown to us and need to be handled.
                    _ => Err(Error::UnmappableInvalid),
                }
            })
            .collect::<ResultUnnamed<Vec<_>>>()?
            .into_iter()
            .fold(DERAlarmStatus::empty(), |x, y| x | y))
    }
}

impl Convert<StateOfChargeStatusType> for u16 {
    fn convert(self: u16) -> StateOfChargeStatusType {
        StateOfChargeStatusType {
            date_time: Int64(Utc::now().timestamp()),
            value: Percent::new(self).unwrap_or_default(),
        }
    }
}

impl TryConvertUnnamed<PhaseCode> for PhaseReference {
    fn try_convert(self: PhaseReference) -> ResultUnnamed<PhaseCode> {
        Ok(match self {
            PhaseReference::LLV => PhaseCode::PhaseABC,
            PhaseReference::LNV => PhaseCode::PhaseAN,
            PhaseReference::VL1 => PhaseCode::PhaseA,
            PhaseReference::VL2 => PhaseCode::PhaseB,
            PhaseReference::VL3 => PhaseCode::PhaseC,
            PhaseReference::VL1L2 => PhaseCode::PhaseAB,
            PhaseReference::VL2L3 => PhaseCode::PhaseBC,
            PhaseReference::VL3L1 => PhaseCode::PhaseCA,
        })
    }
}

//////
// Internals for converting to modbus.
impl TryConvertUnnamed<u16> for Int16 {
    fn try_convert(self: Int16) -> ResultUnnamed<u16> {
        // Raise errors on negative values.
        u16::try_from(self.0).map_err(|_| Error::UnsignedNegative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use sep2_common::packages::{der::DERControlBase, primitives::Uint32};

    proptest! {
        #[test]
        fn negative_to_unsigned_fails(x in i16::MIN..0) {
            let result: ResultUnnamed<u16> = Int16(x).try_convert();
            assert!(matches!(result, Err(Error::UnsignedNegative)));
        }

        #[test]
        fn positive_to_unsigned(x in 0..i16::MAX) {
            let result: ResultUnnamed<u16> = Int16(x).try_convert();
            assert!(result.is_ok());
        }

        #[test]
        fn too_large_to_signed_fails(x in ((i16::MAX as u16)+1u16)..u16::MAX) {
            let result: ResultUnnamed<Int16> = x.try_convert();
            assert!(matches!(result, Err(Error::SignedOverflow)));
        }

        #[test]
        fn small_signed_to_unsigned(x in 0u16..(i16::MAX as u16)) {
            let result: ResultUnnamed<Int16> = x.try_convert();
            assert!(result.is_ok());
        }
    }

    #[test]
    fn missing_mandatory_fails() {
        let result: ResultUnnamed<ActivePower> = None::<u16>.try_convert_mandatory();
        assert!(matches!(result, Err(Error::MandatoryNone)));
    }

    #[test]
    fn mandatory_conversion() {
        let result: ResultUnnamed<ActivePower> = Some(0u16).try_convert_mandatory();
        assert!(result.is_ok());
    }

    #[test]
    fn options_none() {
        let result: ResultUnnamed<Option<ActivePower>> = None::<u16>.try_convert();
        assert!(matches!(result, Ok(None)));

        let result: Option<PowerFactor> = None::<u16>.convert();
        assert!(result.is_none());
    }

    #[test]
    fn options_some() {
        let result: ResultUnnamed<Option<ActivePower>> = Some(0u16).try_convert();
        assert!(matches!(result, Ok(Some(_))));

        let result: Option<PowerFactor> = Some(0u16).convert();
        assert!(result.is_some());
    }

    #[test]
    fn invalid_value_fails() {
        let result: ResultUnnamed<OperationalModeStatusType> =
            model701::St::Invalid(0).try_convert();
        assert!(matches!(result, Err(Error::UnmappableInvalid)));
    }

    #[test]
    fn errors_should_include_name() {
        let status = ModbusStatus {
            st: Some(model701::St::Invalid(0)),
            conn_st: None,
            alrm: None,
            soc: None,
        };

        let result: Result<DERStatus> = status.try_convert();
        assert!(
            matches!(result, Err(NamedError { kind: Error::UnmappableInvalid, name }) if name == "operational_mode_status")
        );

        let err = result.unwrap_err();
        assert!(err.to_string().contains("operational_mode_status"));
    }

    #[test]
    fn settings() {
        let settings = ModbusSettings { esv_hi: Some(42) };

        let result: Result<DERSettings> = settings.try_convert();
        assert!(result.is_ok());
    }

    #[test]
    fn status() {
        let status = ModbusStatus {
            st: Some(model701::St::On),
            // TODO: once this is translatable, add a value back in.
            conn_st: None,
            alrm: Some(model701::Alrm::AcOverVolt),
            soc: Some(42),
        };

        let result: Result<DERStatus> = status.try_convert();
        assert!(result.is_ok());
    }

    #[test]
    fn capabilities() {
        let capabilities = ModbusCapabilities {
            w_max_rtg: Some(42),
            w_ovr_ext_rtg: Some(43),
            w_ovr_ext_rtg_pf: Some(1),
            w_und_ext_rtg: Some(44),
            w_und_ext_rtg_pf: Some(2),
            va_max_rtg: Some(45),
            var_max_inj_rtg: Some(46),
            var_max_abs_rtg: Some(47),
            w_cha_rte_max_rtg: Some(48),
            va_cha_rte_max_rtg: Some(49),
            v_nom_rtg: Some(50),
            v_max_rtg: Some(51),
            v_min_rtg: Some(52),
            // TODO: once this is translatable, add a value back in.
            ctrl_modes: None,
            react_suscept_rtg: Some(53),
        };

        let result: Result<DERCapability> = capabilities.try_convert();
        assert!(result.is_ok());
    }

    #[test]
    fn metering() {
        let metering = ModbusMetering {
            active_power: Some(42),
            reactive_power: Some(45),
            voltages: vec![VoltageWithReference(10000, PhaseReference::VL1L2)],
            frequency: Some(60),
        };

        let result: Result<Vec<MirrorMeterReading>> = metering.try_convert();
        assert!(result.is_ok());
    }

    #[test]
    fn parameters() {
        let parameters = ControlAttributes {
            base: DERControlBase {
                op_mod_connect: Some(true),
                ..Default::default()
            },
            set_es_delay: Some(Uint32(42)),
            ..Default::default()
        };

        let result: Result<ModbusParameters> = parameters.try_convert();
        assert!(result.is_ok());
    }
}
