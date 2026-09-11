// Translation between SEP2 and Modbus

use chrono::Utc;
use derive_more::Display;
use sep2_common::packages::{
    der::{
        ActivePower, ApparentPower, ConnectStatusType, ConnectStatusValue, DERAlarmStatus,
        DERCapability, DERControlType, DERCurve, DERSettings, DERStatus, DERUnitRefType,
        FreqDroopType, OperationalModeStatusType, OperationalModeStatusValue, PowerFactor,
        ReactivePower, ReactiveSusceptance, StateOfChargeStatusType, VoltageRMS,
    },
    links::DERCurveLink,
    metering::{Reading, ReadingType},
    metering_mirror::MirrorMeterReading,
    primitives::{Int16, Int32, Int48, Int64, String32, Uint16, Uint32},
    types::{
        AccumulationBehaviourType, CommodityType, DateTimeInterval, FlowDirectionType, KindType,
        Percent, PhaseCode, PowerOfTenMultiplierType, SignedPercent, UomType,
    },
};
use std::convert::TryFrom;
use sunspec::models::{model701, model702::CtrlModes, model703, model704, model705, model706};

use crate::{
    ScaledValue,
    modbus_connection::{
        Capabilities as ModbusCapabilities, Curve as ModbusCurve, Metering as ModbusMetering,
        Model711Ctl, Parameters as ModbusParameters, PhaseReference, Settings as ModbusSettings,
        Status as ModbusStatus, VoltageWithReference,
    },
    scheduler::ControlAttributes,
};

pub type Result<T> = std::result::Result<T, NamedError>;

// SEP2 fixes the scale of each of its quantities in the specification, rather
// than carrying a scale factor alongside the value the way sunspec does. SEP2 uses:
//
// - hundredths for: percentages, frequencies and times.
// - thousandths for: frequency droops
const SEP2_HUNDREDTHS_SF: i16 = -2;
const SEP2_THOUSANDTHS_SF: i16 = -3;

// Sunspec does not define a scale factor for time units, but we include this
// const to make the intention clear in the code.
const SUNSPEC_SECONDS_SF: i16 = 0;

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
    OutOfRange,
    Unknown,
}

impl std::error::Error for Error {}

impl Error {
    fn name(self, name: &'static str) -> NamedError {
        NamedError { kind: self, name }
    }
}

// The public facing conversions use TryFrom.
impl TryFrom<ModbusCapabilities> for DERCapability {
    type Error = NamedError;

    fn try_from(caps: ModbusCapabilities) -> Result<Self> {
        Ok(DERCapability {
            rtg_max_w: caps
                .w_max_rtg
                .try_convert_mandatory()
                .map_err(|err| err.name("w_max_rtg"))?,
            rtg_over_excited_w: caps
                .w_ovr_ext_rtg
                .try_convert()
                .map_err(|err| err.name("rtg_over_excited_w"))?,
            rtg_over_excited_pf: caps.w_ovr_ext_rtg_pf.convert(),
            rtg_under_excited_w: caps
                .w_und_ext_rtg
                .try_convert()
                .map_err(|err| err.name("rtg_under_excited_w"))?,
            rtg_under_excited_pf: caps.w_und_ext_rtg_pf.convert(),
            rtg_max_va: caps.va_max_rtg.convert(),
            rtg_max_var: caps
                .var_max_inj_rtg
                .try_convert()
                .map_err(|err| err.name("var_max_inj_rtg"))?,
            rtg_max_var_neg: caps
                .var_max_abs_rtg
                .try_convert()
                .map_err(|err| err.name("var_max_abs_rtg"))?,
            rtg_max_charge_rate_w: caps
                .w_cha_rte_max_rtg
                .try_convert()
                .map_err(|err| err.name("w_cha_rte_max_rtg"))?,
            rtg_max_charge_rate_va: caps.va_cha_rte_max_rtg.convert(),
            rtg_v_nom: caps.v_nom_rtg.convert(),
            rtg_max_v: caps.v_max_rtg.convert(),
            rtg_min_v: caps.v_min_rtg.convert(),
            modes_supported: caps
                .ctrl_modes
                .try_convert()
                .map_err(|err| err.name("modes_supported"))?,
            rtg_reactive_susceptance: caps.react_suscept_rtg.convert(),
            ..Default::default()
        })
    }
}

impl TryFrom<ModbusStatus> for DERStatus {
    type Error = NamedError;

    fn try_from(status: ModbusStatus) -> Result<DERStatus> {
        let connect_status = status
            .conn_st
            .try_convert()
            .map_err(|err| err.name("conn_st"))?;

        Ok(DERStatus {
            operational_mode_status: status
                .st
                .try_convert()
                .map_err(|err| err.name("operational_mode_status"))?,
            gen_connect_status: connect_status.clone(),
            stor_connect_status: connect_status,
            alarm_status: status
                .alrm
                .try_convert()
                .map_err(|err| err.name("alarm_status"))?,
            state_of_charge_status: status.soc.convert(),
            reading_time: Int64(Utc::now().timestamp()),
            ..Default::default()
        })
    }
}

impl TryFrom<ModbusSettings> for DERSettings {
    type Error = NamedError;

    fn try_from(settings: ModbusSettings) -> Result<DERSettings> {
        Ok(DERSettings {
            set_es_high_volt: settings
                .esv_hi
                .try_convert()
                .map_err(|err| err.name("set_es_high_volt"))?,
            updated_time: Int64(Utc::now().timestamp()),
            ..Default::default()
        })
    }
}

impl TryFrom<ModbusMetering> for Vec<MirrorMeterReading> {
    type Error = NamedError;

    fn try_from(metering: ModbusMetering) -> Result<Vec<MirrorMeterReading>> {
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

        let power_template = |name: &str, value: Option<i16>, phase| {
            value
                .map(|value| {
                    Ok(MirrorMeterReading {
                        description: Some(String32(name.into())),
                        reading_type: Some(ReadingType {
                            flow_direction: Some(FlowDirectionType::Reverse),
                            kind: Some(KindType::Power),
                            uom: Some(UomType::W),
                            power_of_ten_multiplier: metering
                                .w_sf
                                .try_convert()
                                .map_err(|err| err.name("w_sf"))?,
                            phase,
                            ..template_reading_type.clone()
                        }),
                        reading: Some(Reading {
                            value: Some(Int48(i64::from(value))),
                            ..template_reading.clone()
                        }),
                        ..Default::default()
                    })
                })
                .transpose()
        };

        let w = power_template("w", metering.w, None)?;
        let wl1 = power_template("wl1", metering.wl1, Some(PhaseCode::PhaseA))?;
        let wl2 = power_template("wl2", metering.wl2, Some(PhaseCode::PhaseB))?;
        let wl3 = power_template("wl3", metering.wl3, Some(PhaseCode::PhaseC))?;

        let var = metering
            .var
            .map(|value| {
                Ok(MirrorMeterReading {
                    description: Some(String32("reactive_power".into())),
                    reading_type: Some(ReadingType {
                        flow_direction: Some(FlowDirectionType::Reverse),
                        kind: Some(KindType::Power),
                        uom: Some(UomType::VAr),
                        power_of_ten_multiplier: metering
                            .var_sf
                            .try_convert()
                            .map_err(|err| err.name("var_sf"))?,
                        ..template_reading_type.clone()
                    }),
                    reading: Some(Reading {
                        value: Some(Int48(i64::from(value))),
                        ..template_reading.clone()
                    }),
                    ..Default::default()
                })
            })
            .transpose()?;
        let voltages = metering
            .voltages
            .iter()
            .map(|VoltageWithReference(v, phase)| {
                let name = format!("voltage_{}", phase);
                Ok(MirrorMeterReading {
                    description: Some(String32(name.clone())),
                    reading_type: Some(ReadingType {
                        flow_direction: Some(FlowDirectionType::Forward),
                        phase: Some(phase.try_convert().map_err(|err| err.name("voltages"))?),
                        power_of_ten_multiplier: metering
                            .v_sf
                            .try_convert()
                            .map_err(|err| err.name("v_sf"))?,
                        uom: Some(UomType::Voltage),
                        ..template_reading_type.clone()
                    }),
                    reading: Some(Reading {
                        value: Some(Int48(i64::from(*v))),
                        ..template_reading.clone()
                    }),
                    ..Default::default()
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let hz = metering
            .hz
            .map(|value| {
                Ok(MirrorMeterReading {
                    description: Some(String32("frequency".into())),
                    reading_type: Some(ReadingType {
                        flow_direction: Some(FlowDirectionType::Reverse),
                        uom: Some(UomType::Hz),
                        power_of_ten_multiplier: metering
                            .hz_sf
                            .try_convert()
                            .map_err(|err| err.name("hz_sf"))?,
                        ..template_reading_type.clone()
                    }),
                    reading: Some(Reading {
                        value: Some(Int48(i64::from(value))),
                        ..template_reading.clone()
                    }),
                    ..Default::default()
                })
            })
            .transpose()?;

        Ok(vec![w, wl1, wl2, wl3, var, hz]
            .into_iter()
            .flatten()
            .chain(voltages)
            .collect())
    }
}

impl TryFrom<ControlAttributes> for ModbusParameters {
    type Error = NamedError;

    fn try_from(attrs: ControlAttributes) -> Result<ModbusParameters> {
        let get_curve_data = |link: DERCurveLink| attrs.curves.get(&link.href).cloned();

        let der_volt_var_curve = attrs
            .inner
            .der_control_base
            .op_mod_volt_var
            .clone()
            .and_then(get_curve_data);
        let der_volt_watt_curve = attrs
            .inner
            .der_control_base
            .op_mod_volt_watt
            .and_then(get_curve_data);

        Ok(ModbusParameters {
            // AS5438 - Table F.3 to E.3
            pfw_inj_ena: attrs
                .inner
                .der_control_base
                .op_mod_fixed_pf_inject_w
                .is_some()
                .convert(),
            pfw_inj_pf: attrs
                .inner
                .der_control_base
                .op_mod_fixed_pf_inject_w
                .as_ref()
                .map(|pfw| ScaledValue::new(pfw.displacement.convert(), pfw.multiplier.convert())),
            pfw_inj_ext: attrs
                .inner
                .der_control_base
                .op_mod_fixed_pf_inject_w
                .map(|pfw| pfw.excitation.convert()),
            pfw_abs_ena: attrs
                .inner
                .der_control_base
                .op_mod_fixed_pf_absorb_w
                .is_some()
                .convert(),
            pfw_abs_pf: attrs
                .inner
                .der_control_base
                .op_mod_fixed_pf_absorb_w
                .as_ref()
                .map(|pfw| ScaledValue::new(pfw.displacement.convert(), pfw.multiplier.convert())),
            pfw_abs_ext: attrs
                .inner
                .der_control_base
                .op_mod_fixed_pf_absorb_w
                .map(|pfw| pfw.excitation.convert()),

            // AS5438 - Table F.4 to E.4
            der_volt_var: der_volt_var_curve
                .clone()
                .map(|c| convert_curve(c, AxisOrder::Same))
                .transpose()
                .map_err(|err| err.name("der_volt_var"))?,

            der_volt_var_tms: der_volt_var_curve.as_ref().and_then(|c| {
                c.open_loop_tms
                    .convert()
                    .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF))
            }),
            der_volt_var_dept_ref: der_volt_var_curve
                .as_ref()
                .map(|c| c.y_ref_type.try_convert())
                .transpose()
                .map_err(|err| err.name("der_volt_var_dept_ref"))?,

            vref: der_volt_var_curve.as_ref().and_then(|c| {
                c.v_ref
                    .convert()
                    .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF))
            }),
            vref_auto_ena: der_volt_var_curve
                .as_ref()
                .map(|c| c.autonomous_v_ref_enable.unwrap_or(false).convert()),
            vref_auto_tms: der_volt_var_curve
                .as_ref()
                .and_then(|c| {
                    c.autonomous_v_ref_time_constant
                        .map(|val| Uint32(hundredths_to_seconds(val.0)).try_convert())
                })
                .transpose()
                .map_err(|err| err.name("vref_auto_tms"))?,

            // AS5438 - Table F.5 to E.5
            var_set_ena: attrs
                .inner
                .der_control_base
                .op_mod_fixed_var
                .is_some()
                .convert(),
            var_set_pct: attrs
                .inner
                .der_control_base
                .op_mod_fixed_var
                .as_ref()
                .map(|var| ScaledValue::new(var.value.convert(), SEP2_HUNDREDTHS_SF)),
            var_set_mod: attrs
                .inner
                .der_control_base
                .op_mod_fixed_var
                .map(|var| var.ref_type.try_convert())
                .transpose()
                .map_err(|err| err.name("var_set_mod"))?,

            // AS5438 - Table F.6 to E.6
            der_volt_watt: der_volt_watt_curve
                .clone()
                .map(|c| convert_curve(c, AxisOrder::Same))
                .transpose()
                .map_err(|err| err.name("der_volt_watt"))?,
            der_volt_watt_tms: der_volt_watt_curve
                .as_ref()
                .and_then(|curve_data| curve_data.open_loop_tms.convert())
                .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF)),
            der_volt_watt_dept_ref: der_volt_watt_curve
                .as_ref()
                .map(|c| c.y_ref_type.try_convert())
                .transpose()
                .map_err(|err| err.name("der_volt_watt_dept_ref"))?,

            // AS5438 - Table F.7 to E.7
            der_trip_lv_must: attrs
                .inner
                .der_control_base
                .op_mod_lvrt_must_trip
                .and_then(get_curve_data)
                .map(|c| convert_curve(c, AxisOrder::Flipped))
                .transpose()
                .map_err(|err| err.name("der_trip_lv_must"))?,
            der_trip_lv_mom_cess: attrs
                .inner
                .der_control_base
                .op_mod_lvrt_momentary_cessation
                .and_then(get_curve_data)
                .map(|c| convert_curve(c, AxisOrder::Flipped))
                .transpose()
                .map_err(|err| err.name("der_trip_lv_mom_cess"))?,
            der_trip_hv_must: attrs
                .inner
                .der_control_base
                .op_mod_hvrt_must_trip
                .and_then(get_curve_data)
                .map(|c| convert_curve(c, AxisOrder::Flipped))
                .transpose()
                .map_err(|err| err.name("der_trip_hv_must"))?,
            der_trip_hv_mom_cess: attrs
                .inner
                .der_control_base
                .op_mod_hvrt_momentary_cessation
                .and_then(get_curve_data)
                .map(|c| convert_curve(c, AxisOrder::Flipped))
                .transpose()
                .map_err(|err| err.name("der_trip_hv_mom_cess"))?,

            // AS5438 - Table F.8 to E.8
            der_trip_lf: attrs
                .inner
                .der_control_base
                .op_mod_lfrt_must_trip
                .and_then(get_curve_data)
                .map(|c| convert_curve(c, AxisOrder::Flipped))
                .transpose()
                .map_err(|err| err.name("der_trip_lf"))?,
            der_trip_hf: attrs
                .inner
                .der_control_base
                .op_mod_hfrt_must_trip
                .and_then(get_curve_data)
                .map(|c| convert_curve(c, AxisOrder::Flipped))
                .transpose()
                .map_err(|err| err.name("der_trip_hf"))?,

            // AS5438 - Table F.9 to E.9
            droop_ctl: attrs.inner.der_control_base.op_mod_freq_droop.convert(),

            // AS5438 - Table F.10 to E.10
            es: attrs.inner.der_control_base.op_mod_connect.convert(),
            esv_hi: attrs
                .inner
                .set_es_high_volt
                .try_convert()
                .map_err(|err| err.name("esv_hi"))?
                .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF)),
            esv_lo: attrs
                .inner
                .set_es_low_volt
                .try_convert()
                .map_err(|err| err.name("esv_lo"))?
                .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF)),
            es_hz_hi: attrs
                .inner
                .set_es_high_freq
                .convert()
                .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF)),
            es_hz_lo: attrs
                .inner
                .set_es_low_freq
                .convert()
                .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF)),
            es_dly_tms: attrs
                .inner
                .set_es_delay
                .convert()
                .map(hundredths_to_seconds),
            es_rnd_tms: attrs
                .inner
                .set_es_random_delay
                .convert()
                .map(hundredths_to_seconds),
            es_rmp_tms: attrs
                .inner
                .set_es_ramp_tms
                .convert()
                .map(hundredths_to_seconds),

            // AS5438 - Table F.11 to E.11
            w_max_lim_pct_ena: attrs
                .inner
                .der_control_base
                .op_mod_max_lim_w
                .is_some()
                .convert(),
            w_max_lim_pct: attrs
                .inner
                .der_control_base
                .op_mod_max_lim_w
                .convert()
                .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF)),

            // AS5438 - Table F.12 to E.12
            w_set_ena: (attrs.inner.der_control_base.op_mod_fixed_w.is_some()
                || attrs.inner.der_control_base.op_mod_target_w.is_some())
            .convert(),
            w_set_pct: attrs
                .inner
                .der_control_base
                .op_mod_fixed_w
                .convert()
                .map(|val| ScaledValue::new(val, SEP2_HUNDREDTHS_SF)),
            w_set: attrs
                .inner
                .der_control_base
                .op_mod_target_w
                .clone()
                .convert(),
            // Also set WSetMod conditionally. If both WSet and WSetPct are
            // available this is likely a mistake from upstream, however default
            // to WSetPct as that is the specified in the AS5438 spec.
            w_set_mod: match (
                attrs.inner.der_control_base.op_mod_fixed_w,
                attrs.inner.der_control_base.op_mod_target_w,
            ) {
                (None, None) => None,
                (Some(_), None) => Some(model704::WSetMod::WMaxPct),
                (None, Some(_)) => Some(model704::WSetMod::Watts),
                (Some(_), Some(_)) => Some(model704::WSetMod::WMaxPct),
            },
        })
    }
}

fn hundredths_to_seconds(hundredths_of_a_second: u32) -> u32 {
    ScaledValue::new(hundredths_of_a_second, SEP2_HUNDREDTHS_SF)
        .rescale(SUNSPEC_SECONDS_SF)
        .value
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

// Local traits to make writing out the conversions easier. These are not
// exposed outside of this module to avoid leaking new methods onto the commonly
// used data types.
trait Convert<T> {
    fn convert(self) -> T;
}

trait TryConvert<T> {
    fn try_convert(self) -> ResultUnnamed<T>;
}

// Specialised trait for options to avoid getting tied in knots with nested traits.
trait OptionTryConvert<T, U: TryConvert<T>> {
    fn try_convert(self) -> ResultUnnamed<Option<T>>;
    // Conversions to error on missing values.
    fn try_convert_mandatory(self) -> ResultUnnamed<T>;
}
impl<T, U> OptionTryConvert<T, U> for Option<U>
where
    U: TryConvert<T>,
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
}
trait OptionConvert<T, U: Convert<T>> {
    // Conversions without errors
    fn convert(self) -> Option<T>;
}
impl<T, U> OptionConvert<T, U> for Option<U>
where
    U: Convert<T>,
{
    fn convert(self: Option<U>) -> Option<T>
    where
        U: Convert<T>,
    {
        self.map(|inner| inner.convert())
    }
}

//////
// Internals for converting to SEP2.

impl TryConvert<Int16> for u16 {
    fn try_convert(self: u16) -> ResultUnnamed<Int16> {
        Ok(Int16(
            i16::try_from(self).map_err(|_| Error::SignedOverflow)?,
        ))
    }
}

// Int16s appear in percentages, which have a granularity of hundredths in SEP2.
impl TryConvert<Int16> for ScaledValue<u16> {
    fn try_convert(self: ScaledValue<u16>) -> ResultUnnamed<Int16> {
        self.rescale(SEP2_HUNDREDTHS_SF).value.try_convert()
    }
}

impl TryConvert<ActivePower> for u16 {
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

impl TryConvert<ReactivePower> for u16 {
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

impl TryConvert<DERControlType> for Option<CtrlModes> {
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

impl TryConvert<OperationalModeStatusType> for model701::St {
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

impl TryConvert<ConnectStatusType> for model701::ConnSt {
    fn try_convert(self: model701::ConnSt) -> ResultUnnamed<ConnectStatusType> {
        Ok(ConnectStatusType {
            date_time: Int64(Utc::now().timestamp()),
            value: match self {
                model701::ConnSt::Disconnected => ConnectStatusValue::empty(),
                model701::ConnSt::Connected => ConnectStatusValue::Connected,
                model701::ConnSt::Invalid(_) => Err(Error::UnmappableInvalid)?,
            },
        })
    }
}

impl TryConvert<DERAlarmStatus> for model701::Alrm {
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

impl Convert<StateOfChargeStatusType> for ScaledValue<u16> {
    fn convert(self: ScaledValue<u16>) -> StateOfChargeStatusType {
        // SEP2 fixes the scale factor at -2 (hundredths of a percent)
        let hundredths = self.rescale(SEP2_HUNDREDTHS_SF).value;
        StateOfChargeStatusType {
            date_time: Int64(Utc::now().timestamp()),
            // Percent::new returns None for values > 100%. Replace these with 100% instead.
            value: Percent::new(hundredths).unwrap_or_else(|| {
                Percent::new(10_000).expect("Percent::new(10_000) should always be Some")
            }),
        }
    }
}

impl TryConvert<PhaseCode> for PhaseReference {
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

impl TryConvert<PowerOfTenMultiplierType> for i16 {
    fn try_convert(self) -> ResultUnnamed<PowerOfTenMultiplierType> {
        Ok(match self {
            -9 => PowerOfTenMultiplierType::Nano,
            -8 => PowerOfTenMultiplierType::NegativeEight,
            -7 => PowerOfTenMultiplierType::NegativeSeven,
            -6 => PowerOfTenMultiplierType::Micro,
            -5 => PowerOfTenMultiplierType::NegativeFive,
            -4 => PowerOfTenMultiplierType::NegativeFour,
            -3 => PowerOfTenMultiplierType::Milli,
            -2 => PowerOfTenMultiplierType::Centi,
            -1 => PowerOfTenMultiplierType::Deci,
            0 => PowerOfTenMultiplierType::None,
            1 => PowerOfTenMultiplierType::Deca,
            2 => PowerOfTenMultiplierType::Hecto,
            3 => PowerOfTenMultiplierType::Kilo,
            4 => PowerOfTenMultiplierType::Four,
            5 => PowerOfTenMultiplierType::Five,
            6 => PowerOfTenMultiplierType::Mega,
            7 => PowerOfTenMultiplierType::Seven,
            8 => PowerOfTenMultiplierType::Eight,
            9 => PowerOfTenMultiplierType::Giga,
            _ => Err(Error::OutOfRange)?,
        })
    }
}

//////
// Internals for converting to modbus.
impl TryConvert<u16> for Int16 {
    fn try_convert(self: Int16) -> ResultUnnamed<u16> {
        // Raise errors on negative values.
        u16::try_from(self.0).map_err(|_| Error::UnsignedNegative)
    }
}

impl TryConvert<u32> for Int32 {
    fn try_convert(self: Int32) -> ResultUnnamed<u32> {
        // Raise errors on negative values.
        u32::try_from(self.0).map_err(|_| Error::UnsignedNegative)
    }
}

impl TryConvert<u16> for Int32 {
    fn try_convert(self: Int32) -> ResultUnnamed<u16> {
        if self.0 < 0 {
            return Err(Error::UnsignedNegative);
        }

        u16::try_from(self.0).map_err(|_| Error::SignedOverflow)
    }
}

impl TryConvert<i16> for Int32 {
    fn try_convert(self: Int32) -> ResultUnnamed<i16> {
        i16::try_from(self.0).map_err(|_| Error::SignedOverflow)
    }
}

impl TryConvert<u16> for Uint32 {
    fn try_convert(self: Uint32) -> ResultUnnamed<u16> {
        u16::try_from(self.0).map_err(|_| Error::SignedOverflow)
    }
}

impl Convert<u32> for Uint16 {
    fn convert(self: Uint16) -> u32 {
        u32::from(self.0)
    }
}

impl Convert<u16> for Uint16 {
    fn convert(self: Uint16) -> u16 {
        self.0
    }
}

impl Convert<u32> for Uint32 {
    fn convert(self: Uint32) -> u32 {
        self.0
    }
}

impl Convert<u16> for Percent {
    fn convert(self) -> u16 {
        self.get()
    }
}

impl Convert<i16> for SignedPercent {
    fn convert(self) -> i16 {
        self.get()
    }
}

impl Convert<Option<model704::WMaxLimPctEna>> for bool {
    fn convert(self: bool) -> Option<model704::WMaxLimPctEna> {
        match self {
            false => Some(model704::WMaxLimPctEna::Disabled),
            true => Some(model704::WMaxLimPctEna::Enabled),
        }
    }
}

impl Convert<Option<model704::WSetEna>> for bool {
    fn convert(self: bool) -> Option<model704::WSetEna> {
        match self {
            false => Some(model704::WSetEna::Disabled),
            true => Some(model704::WSetEna::Enabled),
        }
    }
}

impl Convert<Option<model704::VarSetEna>> for bool {
    fn convert(self: bool) -> Option<model704::VarSetEna> {
        match self {
            false => Some(model704::VarSetEna::Disabled),
            true => Some(model704::VarSetEna::Enabled),
        }
    }
}

impl Convert<Option<model704::PfwInjEna>> for bool {
    fn convert(self: bool) -> Option<model704::PfwInjEna> {
        match self {
            false => Some(model704::PfwInjEna::Disabled),
            true => Some(model704::PfwInjEna::Enabled),
        }
    }
}

impl Convert<Option<model704::PfwAbsEna>> for bool {
    fn convert(self: bool) -> Option<model704::PfwAbsEna> {
        match self {
            false => Some(model704::PfwAbsEna::Disabled),
            true => Some(model704::PfwAbsEna::Enabled),
        }
    }
}

impl Convert<model704::PfwInjExt> for bool {
    fn convert(self: bool) -> model704::PfwInjExt {
        match self {
            false => model704::PfwInjExt::OverExcited,
            true => model704::PfwInjExt::UnderExcited,
        }
    }
}

impl Convert<model704::PfwAbsExt> for bool {
    fn convert(self: bool) -> model704::PfwAbsExt {
        match self {
            false => model704::PfwAbsExt::OverExcited,
            true => model704::PfwAbsExt::UnderExcited,
        }
    }
}

impl TryConvert<model704::VarSetMod> for DERUnitRefType {
    fn try_convert(self: DERUnitRefType) -> ResultUnnamed<model704::VarSetMod> {
        Ok(match self {
            DERUnitRefType::SetMaxW => model704::VarSetMod::WMaxPct,
            DERUnitRefType::SetMaxVar => model704::VarSetMod::VarMaxPct,
            DERUnitRefType::StatVarAvail => model704::VarSetMod::VarAvailPct,
            DERUnitRefType::StatWAvail
            | DERUnitRefType::SetEffectiveV
            | DERUnitRefType::SetMaxChargeRateW
            | DERUnitRefType::SetMaxDischargeRateW
            | DERUnitRefType::NotApplicable => Err(Error::UnmappableInvalid)?,
        })
    }
}

impl Convert<model703::Es> for bool {
    fn convert(self: bool) -> model703::Es {
        match self {
            false => model703::Es::Disabled,
            true => model703::Es::Enabled,
        }
    }
}

impl Convert<model705::CrvVRefAutoEna> for bool {
    fn convert(self: bool) -> model705::CrvVRefAutoEna {
        match self {
            false => model705::CrvVRefAutoEna::Disabled,
            true => model705::CrvVRefAutoEna::Enabled,
        }
    }
}

impl TryConvert<model705::CrvDeptRef> for DERUnitRefType {
    fn try_convert(self: DERUnitRefType) -> ResultUnnamed<model705::CrvDeptRef> {
        Ok(match self {
            DERUnitRefType::SetMaxW => model705::CrvDeptRef::WMaxPct,
            DERUnitRefType::SetMaxVar => model705::CrvDeptRef::VarMaxPct,
            DERUnitRefType::StatVarAvail => model705::CrvDeptRef::VarAvalPct,
            DERUnitRefType::StatWAvail
            | DERUnitRefType::SetEffectiveV
            | DERUnitRefType::SetMaxChargeRateW
            | DERUnitRefType::SetMaxDischargeRateW
            | DERUnitRefType::NotApplicable => Err(Error::UnmappableInvalid)?,
        })
    }
}

impl TryConvert<model706::CrvDeptRef> for DERUnitRefType {
    fn try_convert(self: DERUnitRefType) -> ResultUnnamed<model706::CrvDeptRef> {
        Ok(match self {
            DERUnitRefType::SetMaxW => model706::CrvDeptRef::WMaxPct,
            DERUnitRefType::StatWAvail => model706::CrvDeptRef::WAvalPct,
            DERUnitRefType::SetMaxVar
            | DERUnitRefType::StatVarAvail
            | DERUnitRefType::SetEffectiveV
            | DERUnitRefType::SetMaxChargeRateW
            | DERUnitRefType::SetMaxDischargeRateW
            | DERUnitRefType::NotApplicable => Err(Error::UnmappableInvalid)?,
        })
    }
}

impl Convert<Model711Ctl> for FreqDroopType {
    fn convert(self: FreqDroopType) -> Model711Ctl {
        Model711Ctl {
            db_of: ScaledValue::new(self.d_bof.0, SEP2_THOUSANDTHS_SF),
            db_uf: ScaledValue::new(self.d_buf.0, SEP2_THOUSANDTHS_SF),
            k_of: ScaledValue::new(self.k_of.0, SEP2_THOUSANDTHS_SF),
            k_uf: ScaledValue::new(self.k_uf.0, SEP2_THOUSANDTHS_SF),
            rsp_tms: ScaledValue::new(self.open_loop_tms.convert(), SEP2_HUNDREDTHS_SF),
        }
    }
}

impl Convert<ScaledValue<i32>> for ActivePower {
    fn convert(self: ActivePower) -> ScaledValue<i32> {
        ScaledValue {
            value: i32::from(self.value.0),
            sf: self.multiplier.convert(),
        }
    }
}

impl Convert<i16> for PowerOfTenMultiplierType {
    fn convert(self: PowerOfTenMultiplierType) -> i16 {
        match self {
            PowerOfTenMultiplierType::Nano => -9,
            PowerOfTenMultiplierType::NegativeEight => -8,
            PowerOfTenMultiplierType::NegativeSeven => -7,
            PowerOfTenMultiplierType::Micro => -6,
            PowerOfTenMultiplierType::NegativeFive => -5,
            PowerOfTenMultiplierType::NegativeFour => -4,
            PowerOfTenMultiplierType::Milli => -3,
            PowerOfTenMultiplierType::Centi => -2,
            PowerOfTenMultiplierType::Deci => -1,
            PowerOfTenMultiplierType::None => 0,
            PowerOfTenMultiplierType::Deca => 1,
            PowerOfTenMultiplierType::Hecto => 2,
            PowerOfTenMultiplierType::Kilo => 3,
            PowerOfTenMultiplierType::Four => 4,
            PowerOfTenMultiplierType::Five => 5,
            PowerOfTenMultiplierType::Mega => 6,
            PowerOfTenMultiplierType::Seven => 7,
            PowerOfTenMultiplierType::Eight => 8,
            PowerOfTenMultiplierType::Giga => 9,
        }
    }
}

enum AxisOrder {
    Same,
    Flipped,
}

/// Convert a DER Curve to the sunspec format. For some curves, the x/y axes are
/// in agreement between the two protocols. For the trip curves in particular
/// they are reversed. Hence, we don't provide a TryConvert handler but a
/// dedicated function for the conversion.
fn convert_curve<TX, TY>(
    input: DERCurve,
    axis_order: AxisOrder,
) -> ResultUnnamed<ModbusCurve<TX, TY>>
where
    Int32: TryConvert<TX>,
    Int32: TryConvert<TY>,
{
    match axis_order {
        AxisOrder::Same => Ok(ModbusCurve {
            points: input
                .curve_data
                .iter()
                .map(|point| Ok((point.xvalue.try_convert()?, point.yvalue.try_convert()?)))
                .collect::<ResultUnnamed<Vec<_>>>()?,
            sf_x: input.x_multiplier.convert(),
            sf_y: input.y_multiplier.convert(),
        }),
        AxisOrder::Flipped => Ok(ModbusCurve {
            points: input
                .curve_data
                .iter()
                .map(|point| Ok((point.yvalue.try_convert()?, point.xvalue.try_convert()?)))
                .collect::<ResultUnnamed<Vec<_>>>()?,
            sf_x: input.y_multiplier.convert(),
            sf_y: input.x_multiplier.convert(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use sep2_common::packages::{
        der::{DERControlBase, DefaultDERControl},
        primitives::Uint32,
    };

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

        let result: Result<DERStatus> = status.try_into();
        assert!(
            matches!(result, Err(NamedError { kind: Error::UnmappableInvalid, name }) if name == "operational_mode_status")
        );

        let err = result.unwrap_err();
        assert!(err.to_string().contains("operational_mode_status"));
    }

    #[test]
    fn settings() {
        let settings = ModbusSettings {
            esv_hi: Some(ScaledValue::new(42, SEP2_HUNDREDTHS_SF)),
        };

        let result: Result<DERSettings> = settings.try_into();
        assert!(result.is_ok());
    }

    /// Test translation of scale factor to SEP2.
    #[test]
    fn settings_rescaled_to_sep2() {
        // A value of 24.5% translated.
        let settings = ModbusSettings {
            esv_hi: Some(ScaledValue::new(245, -1)),
        };

        let result: DERSettings = settings.try_into().expect("Translation failed");
        assert_eq!(result.set_es_high_volt, Some(Int16(2450)));
    }

    #[test]
    fn status() {
        let status = ModbusStatus {
            st: Some(model701::St::On),
            conn_st: Some(model701::ConnSt::Connected),
            alrm: Some(model701::Alrm::AcOverVolt),
            soc: Some(ScaledValue::new(42, SEP2_HUNDREDTHS_SF)),
        };

        let result: Result<DERStatus> = status.try_into();
        assert!(result.is_ok());
    }

    /// SEP2 fixes the state of charge at hundredths of a percent, so whatever
    /// scale the device reports it at has to be converted, not passed through.
    #[test]
    fn soc_rescaled_to_sep2() {
        // A device reporting whole percent: 10% is 1000 hundredths.
        let soc: StateOfChargeStatusType = ScaledValue::new(10u16, 0).convert();
        assert_eq!(soc.value.get(), 1000);

        // A device already reporting hundredths passes through unchanged.
        let soc: StateOfChargeStatusType = ScaledValue::new(5000u16, -2).convert();
        assert_eq!(soc.value.get(), 5000);

        // And one reporting tenths of a percent.
        let soc: StateOfChargeStatusType = ScaledValue::new(505u16, -1).convert();
        assert_eq!(soc.value.get(), 5050);
    }

    /// An out of range state of charge should clamp to 100% rather than being None.
    #[test]
    fn soc_out_of_range_clamps_to_full() {
        let soc: StateOfChargeStatusType = ScaledValue::new(700u16, 0).convert();
        assert_eq!(soc.value.get(), 10_000);
        let soc: StateOfChargeStatusType = ScaledValue::new(101u16, 0).convert();
        assert_eq!(soc.value.get(), 10_000);
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

        let result: Result<DERCapability> = capabilities.try_into();
        assert!(result.is_ok());
    }

    #[test]
    fn metering() {
        let metering = ModbusMetering {
            w: Some(42),
            w_sf: Some(-1),
            var: Some(45),
            var_sf: Some(1),
            voltages: vec![VoltageWithReference(10000, PhaseReference::VL1L2)],
            v_sf: Some(0),
            hz: Some(60),
            hz_sf: None,
            ..Default::default()
        };

        let result: Result<Vec<MirrorMeterReading>> = metering.try_into();
        assert!(result.is_ok());
    }

    #[test]
    fn parameters() {
        let parameters = ControlAttributes {
            inner: DefaultDERControl {
                der_control_base: DERControlBase {
                    op_mod_connect: Some(true),
                    ..Default::default()
                },
                set_es_delay: Some(Uint32(42)),
                ..Default::default()
            },
            ..Default::default()
        };

        let result: Result<ModbusParameters> = parameters.try_into();
        assert!(result.is_ok());
    }

    /// Every value handed to the modbus side must carry the scale factor SEP2
    /// defines it at, so that it can be rescaled to whatever the device wants.
    #[test]
    fn parameters_carry_sep2_scale_factors() {
        let parameters = ControlAttributes {
            inner: DefaultDERControl {
                der_control_base: DERControlBase {
                    op_mod_max_lim_w: Some(Percent::new(8000).expect("Invalid percent")),
                    op_mod_fixed_w: Some(SignedPercent::new(-2500).expect("Invalid percent")),
                    ..Default::default()
                },
                // 24.50% and 20.00% of nominal voltage.
                set_es_high_volt: Some(Int16(2450)),
                set_es_low_volt: Some(Int16(2000)),
                // 51.00 Hz and 49.00 Hz.
                set_es_high_freq: Some(Uint16(5100)),
                set_es_low_freq: Some(Uint16(4900)),
                ..Default::default()
            },
            ..Default::default()
        };

        let result: ModbusParameters = parameters.try_into().expect("Translation failed");

        assert_eq!(result.esv_hi, Some(ScaledValue::new(2450, -2)));
        assert_eq!(result.esv_lo, Some(ScaledValue::new(2000, -2)));
        assert_eq!(result.es_hz_hi, Some(ScaledValue::new(5100, -2)));
        assert_eq!(result.es_hz_lo, Some(ScaledValue::new(4900, -2)));
        assert_eq!(result.w_max_lim_pct, Some(ScaledValue::new(8000, -2)));
        assert_eq!(result.w_set_pct, Some(ScaledValue::new(-2500, -2)));
    }

    /// Times are the one group with no scale factor on the sunspec side, so
    /// they must be converted directly.
    #[test]
    fn times_converted_to_whole_seconds() {
        let parameters = ControlAttributes {
            inner: DefaultDERControl {
                set_es_delay: Some(Uint32(30_000)),
                set_es_random_delay: Some(Uint32(6_000)),
                set_es_ramp_tms: Some(Uint32(12_000)),
                ..Default::default()
            },
            ..Default::default()
        };

        let result: ModbusParameters = parameters.try_into().expect("Translation failed");

        assert_eq!(result.es_dly_tms, Some(300));
        assert_eq!(result.es_rnd_tms, Some(60));
        assert_eq!(result.es_rmp_tms, Some(120));
    }

    /// Sub-second times cannot be represented by model 703 at all, so they
    /// round to the nearest second rather than being scaled the wrong way.
    #[test]
    fn sub_second_enter_service_times_round() {
        let parameters = ControlAttributes {
            inner: DefaultDERControl {
                // 0.4s, 0.6s and 1.2s.
                set_es_delay: Some(Uint32(40)),
                set_es_random_delay: Some(Uint32(60)),
                set_es_ramp_tms: Some(Uint32(120)),
                ..Default::default()
            },
            ..Default::default()
        };

        let result: ModbusParameters = parameters.try_into().expect("Translation failed");

        assert_eq!(result.es_dly_tms, Some(0));
        assert_eq!(result.es_rnd_tms, Some(1));
        assert_eq!(result.es_rmp_tms, Some(1));
    }

    /// Most of the frequency droop values are in thousandths, but the time is in hundredths.
    #[test]
    fn droop_carries_sep2_scale_factors() {
        let droop = FreqDroopType {
            // 0.360 Hz and 0.350 Hz.
            d_bof: Uint32(360),
            d_buf: Uint32(350),
            // 0.050 and 0.040 per unit.
            k_of: Uint16(50),
            k_uf: Uint16(40),
            // 5.00 seconds.
            open_loop_tms: Uint16(500),
        };

        let result: Model711Ctl = droop.convert();

        assert_eq!(result.db_of, ScaledValue::new(360, -3));
        assert_eq!(result.db_uf, ScaledValue::new(350, -3));
        assert_eq!(result.k_of, ScaledValue::new(50, -3));
        assert_eq!(result.k_uf, ScaledValue::new(40, -3));
        assert_eq!(result.rsp_tms, ScaledValue::new(500, -2));
    }
}
