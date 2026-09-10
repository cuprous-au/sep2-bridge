// Tests the full path a control takes from the SEP2 server to the device.
// Assertions are made against the mocked device's registers.

mod modbus_server_mock;

use std::{str::FromStr, time::Duration};

use chrono::Utc;
use modbus_server_mock::SunSpecMock;
use sep2_bridge::{
    Result, deactivated_broadcast, dispatch, modbus_connection, scheduler, sep2_connection,
};
use sep2_client::{client::Client, device::SEDevice};
use sep2_common::{
    packages::{
        der::{
            CurveData, DERControl, DERControlBase, DERControlList, DERCurve, DERCurveList,
            DERProgram, DERProgramList, DERUnitRefType, DefaultDERControl, FixedVar, FreqDroopType,
            PowerFactorWithExcitation,
        },
        fsa::{FunctionSetAssignments, FunctionSetAssignmentsList},
        identification::{Link, ListLink},
        primitives::{HexBinary160, Int16, Int32, Int64, Uint16, Uint32},
        types::{
            DateTimeInterval, DeviceCategoryType, MRIDType, Percent, PowerOfTenMultiplierType,
            PrimacyType, SFDIType, SignedPercent,
        },
    },
    traits::SEType,
};
use sunspec::{
    Group, Value,
    models::{
        model703, model704, model705, model706, model707, model708, model709, model710, model711,
    },
};
use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{self, Instant},
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

// How long a value is given to travel from the SEP2 server to the device.
// Because we setup the mocks before tasks are started, we avoid much of the
// usual wait time so this value can be quite short.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(1);

// How often the device registers are re-read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

const MOCK_POLL_RATE: u32 = 1;

const HREF_EDEVL: &str = "/edev";
const HREF_EDEV: &str = "/edev/1";
const HREF_FSAL: &str = "/edev/1/fsa";
const HREF_FSA: &str = "/edev/1/fsa/1";
const HREF_DERPL: &str = "/edev/1/fsa/1/derp";
const HREF_DERP: &str = "/edev/1/derp/1";
const HREF_DDERC: &str = "/edev/1/derp/1/dderc";
const HREF_DERCL: &str = "/edev/1/derp/1/derc";
const HREF_DERC_1: &str = "/edev/1/derp/1/derc/1";
const HREF_CURVEL: &str = "/edev/1/derp/1/dc";
const HREF_LFRT_MUST_TRIP_CURVE: &str = "/dc/1";
const HREF_HFRT_MUST_TRIP_CURVE: &str = "/dc/2";
const HREF_LVRT_MUST_TRIP_CURVE: &str = "/dc/3";
const HREF_LVRT_MOM_CESS_CURVE: &str = "/dc/4";
const HREF_HVRT_MUST_TRIP_CURVE: &str = "/dc/5";
const HREF_HVRT_MOM_CESS_CURVE: &str = "/dc/6";
const HREF_VOLT_WATT_CURVE: &str = "/dc/7";
const HREF_VOLT_VAR_CURVE: &str = "/dc/8";

// The values to be mocked and verified. Table numbers from AS5438.
//
// SEP2 fixes the scale of each value it carries, while the device advertises
// its own scale factor per group of registers. The mock deliberately uses scale
// factors that differ from SEP2's, so each expectation below is the SEP2 value
// restated at the device's scale.

// Values for all tables
const EXPECTED_ADPT_CRV_REQ: u16 = 2;

// Table 3.
const OP_MOD_FIXED_PF_INJECT_W: u16 = 400;
const OP_MOD_FIXED_PF_INJECT_W_EXT: bool = true;
const OP_MOD_FIXED_PF_ABSORB_W: u16 = 410;
const OP_MOD_FIXED_PF_ABSORB_W_EXT: bool = true;
const EXPECTED_PFW_INJ_PF: u16 = 4000;
const EXPECTED_PFW_INJ_EXT: model704::PfwInjExt = model704::PfwInjExt::UnderExcited;
const EXPECTED_PFW_ABS_PF: u16 = 4100;
const EXPECTED_PFW_ABS_EXT: model704::PfwAbsExt = model704::PfwAbsExt::UnderExcited;

// Table 4.
const OP_MOD_VOLT_VAR_SF_X: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_VOLT_VAR_SF_Y: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_VOLT_VAR_DATA: &[(i32, i32)] = &[(27, 28), (29, 30)];
const OP_MOD_VOLT_VAR_TMS: u16 = 3100;
const OP_MOD_VOLT_VAR_Y_REF: DERUnitRefType = DERUnitRefType::SetMaxVar;
const OP_MOD_VOLT_VAR_V_REF: u16 = 3200;
const OP_MOD_VOLT_VAR_V_REF_AUTO_TMS: u32 = 3300;
const EXPECTED_DER_VOLT_VAR_DATA: &[(u16, i16)] = &[(2700, 280), (2900, 300)];
const EXPECTED_DER_VOLT_VAR_TMS: u32 = 31;
const EXPECTED_DER_VOLT_VAR_Y_REF: model705::CrvDeptRef = model705::CrvDeptRef::VarMaxPct;
const EXPECTED_DER_VOLT_VAR_V_REF: u16 = 3;
const EXPECTED_DER_VOLT_VAR_V_REF_AUTO_TMS: u16 = 33;

// Table 5.
const OP_MOD_FIXED_VAR: i16 = 42;
const OP_MOD_FIXED_VAR_MOD: DERUnitRefType = DERUnitRefType::SetMaxVar;
const EXPECTED_VAR_SET_PCT: i16 = 4;
const EXPECTED_VAR_SET_MOD: model704::VarSetMod = model704::VarSetMod::VarMaxPct;

// Table 6.
const OP_MOD_VOLT_WATT_SF_X: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_VOLT_WATT_SF_Y: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_VOLT_WATT_DATA: &[(i32, i32)] = &[(23, 24), (25, 26)];
const OP_MOD_VOLT_WATT_TMS: u16 = 2700;
const OP_MOD_VOLT_WATT_Y_REF: DERUnitRefType = DERUnitRefType::SetMaxW;
const EXPECTED_DER_VOLT_WATT_DATA: &[(u16, i16)] = &[(2300, 240), (2500, 260)];
const EXPECTED_DER_VOLT_WATT_TMS: u32 = 27;
const EXPECTED_DER_VOLT_WATT_Y_REF: model706::CrvDeptRef = model706::CrvDeptRef::WMaxPct;

// Table 7. Note that SEP2 and Sunspec choose the opposite ordering for the x/y
// axes in these cases.
const OP_MOD_LVRT_SF_X: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_LVRT_SF_Y: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_LVRT_MUST_DATA: &[(i32, i32)] = &[(7, 8), (9, 10)];
const EXPECTED_DER_TRIP_LV_MUST_DATA: &[(u16, u32)] = &[(800, 70), (1000, 90)];
const OP_MOD_LVRT_MOM_CESS_DATA: &[(i32, i32)] = &[(11, 12), (13, 14)];
const EXPECTED_DER_TRIP_LV_MOM_CESS_DATA: &[(u16, u32)] = &[(1200, 110), (1400, 130)];

const OP_MOD_HVRT_SF_X: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_HVRT_SF_Y: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_HVRT_MUST_DATA: &[(i32, i32)] = &[(15, 16), (17, 18)];
const EXPECTED_DER_TRIP_HV_MUST_DATA: &[(u16, u32)] = &[(1600, 150), (1800, 170)];
const OP_MOD_HVRT_MOM_CESS_DATA: &[(i32, i32)] = &[(19, 20), (21, 22)];
const EXPECTED_DER_TRIP_HV_MOM_CESS_DATA: &[(u16, u32)] = &[(2000, 190), (2200, 210)];

// Table 8. Note that SEP2 and Sunspec choose the opposite ordering for the x/y
// axes in these cases.
const OP_MOD_LFRT_DATA: &[(i32, i32)] = &[(1, 1), (2, 3)];
const OP_MOD_LFRT_SF_X: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const OP_MOD_LFRT_SF_Y: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Kilo;
const EXPECTED_DER_TRIP_LF_DATA: &[(u32, u32)] = &[(100, 10), (300, 20)];

const OP_MOD_HFRT_DATA: &[(i32, i32)] = &[(40, 400), (50, 600)];
const OP_MOD_HFRT_SF_X: PowerOfTenMultiplierType = PowerOfTenMultiplierType::None;
const OP_MOD_HFRT_SF_Y: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Deci;
const EXPECTED_DER_TRIP_HF_DATA: &[(u32, u32)] = &[(4, 0), (6, 1)];

// Table 9. frequency droop values are thousandths in SEP2, except for the time
// which is hundredths of a second.
const DROOP_DB_OF: u32 = 360; // 0.360 Hz
const DROOP_DB_UF: u32 = 350; // 0.350 Hz
const DROOP_K_OF: u16 = 50; // 0.050 per unit
const DROOP_K_UF: u16 = 40; // 0.040 per unit
const DROOP_OPEN_LOOP_TMS: u16 = 500; // 5.00 s
// At the mock's DB_SF of -2, K_SF of -4 and RSP_TMS_SF of 0.
const EXPECTED_DB_OF: u32 = 36;
const EXPECTED_DB_UF: u32 = 35;
const EXPECTED_K_OF: u16 = 500;
const EXPECTED_K_UF: u16 = 400;
const EXPECTED_RSP_TMS: u32 = 5;

// Table 10. All SEP2 SFs are hundredths.
const SET_ES_CONNECT: bool = true;
const SET_ES_HIGH_VOLT: i16 = 2450; // 24.50%
const SET_ES_LOW_VOLT: i16 = 2000; // 20.00%
const SET_ES_HIGH_FREQ: u16 = 5100; // 51.00 Hz
const SET_ES_LOW_FREQ: u16 = 4900; // 49.00 Hz
const SET_ES_DELAY: u32 = 30_000; // 300 s
const SET_ES_RANDOM_DELAY: u32 = 6_000; // 60 s
const SET_ES_RAMP_TMS: u32 = 12_000; // 120 s
// At the mock's V_SF of -1 and HZ_SF of -3.
const EXPECTED_ESV_HI: u16 = 245;
const EXPECTED_ESV_LO: u16 = 200;
const EXPECTED_ES_HZ_HI: u32 = 51_000;
const EXPECTED_ES_HZ_LO: u32 = 49_000;
// Model 703 has an implicit scale factor of 0 for the times
const EXPECTED_ES_DLY_TMS: u32 = 300;
const EXPECTED_ES_RND_TMS: u32 = 60;
const EXPECTED_ES_RMP_TMS: u32 = 120;

// Table 11. All SEP2 SFs are hundredths.
const OP_MOD_MAX_LIM_W: u16 = 8000; // 80.00%
// At the mock's W_MAX_LIM_PCT_SF of 0.
const EXPECTED_W_MAX_LIM_PCT: u16 = 80;

// Table 12. All SEP2 SFs are hundredths.
const OP_MOD_FIXED_W: i16 = -2500; // -25.00%
// At the mock's W_SET_PCT_SF of -1.
const EXPECTED_W_SET_PCT: i16 = -250;

/// Tests that the DefaultDERControl parameters reach model 703.
/// (AS5438 - Table 10)
#[tokio::test]
async fn applies_as5438_table_10() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    assert_register(&mock, "model703::ESV_HI", Some(EXPECTED_ESV_HI)).await;
    assert_register(&mock, "model703::ESV_LO", Some(EXPECTED_ESV_LO)).await;
    assert_register(&mock, "model703::ES_HZ_HI", Some(EXPECTED_ES_HZ_HI)).await;
    assert_register(&mock, "model703::ES_HZ_LO", Some(EXPECTED_ES_HZ_LO)).await;
    assert_register(&mock, "model703::ES_DLY_TMS", Some(EXPECTED_ES_DLY_TMS)).await;
    assert_register(&mock, "model703::ES_RND_TMS", Some(EXPECTED_ES_RND_TMS)).await;
    assert_register(&mock, "model703::ES_RMP_TMS", Some(EXPECTED_ES_RMP_TMS)).await;

    // The mock seeds this as Disabled, so this cannot pass vacuously.
    // We manually are doing the conversion from true to the enum, so assert the
    // const is what we expect.
    const {
        assert!(SET_ES_CONNECT);
    }
    assert_register(&mock, "model703::ES", Some(model703::Es::Enabled)).await;
}

/// Tests that an active DERControl's power limits reach model 704.
/// (AS5438 - Tables 11 and 12)
#[tokio::test]
async fn applies_as5438_tables_11_12() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    assert_register(
        &mock,
        "model704::W_MAX_LIM_PCT_ENA",
        Some(model704::WMaxLimPctEna::Enabled),
    )
    .await;
    assert_register(
        &mock,
        "model704::W_MAX_LIM_PCT",
        Some(EXPECTED_W_MAX_LIM_PCT),
    )
    .await;

    assert_register(
        &mock,
        "model704::W_SET_ENA",
        Some(model704::WSetEna::Enabled),
    )
    .await;
    assert_register(&mock, "model704::W_SET_PCT", Some(EXPECTED_W_SET_PCT)).await;
    assert_register(
        &mock,
        "model704::W_SET_MOD",
        Some(model704::WSetMod::WMaxPct),
    )
    .await;
}

/// Tests that an active DERControl's opModFreqDroop reaches model 711. The
/// values must land in the second (writable) control group, leaving the first
/// read-only group alone.
/// (AS5438 - Table 9)
#[tokio::test]
async fn applies_as5438_table_9() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    assert_register(&mock, "model711::CTL_2::DB_OF", EXPECTED_DB_OF).await;
    assert_register(&mock, "model711::CTL_2::DB_UF", EXPECTED_DB_UF).await;
    assert_register(&mock, "model711::CTL_2::K_OF", EXPECTED_K_OF).await;
    assert_register(&mock, "model711::CTL_2::K_UF", EXPECTED_K_UF).await;
    assert_register(&mock, "model711::CTL_2::RSP_TMS", EXPECTED_RSP_TMS).await;
    assert_register(&mock, "model711::ENA", model711::Ena::Enabled).await;
    assert_register(&mock, "model711::ADPT_CTL_REQ", 2u16).await;

    // The read-only group reporting the current settings must not be touched,
    // and the writable group must stay marked writable.
    assert_register(&mock, "model711::CTL_1::DB_OF", 0u32).await;
    assert_register(
        &mock,
        "model711::CTL_2::READ_ONLY",
        model711::CtlReadOnly::Rw,
    )
    .await;
}

/// Tests that an active DERControl's opModLFRTMustTrip and opModHFRTMustTrip
/// reach models 709 and 710.
/// (AS5438 - Table 8)
#[tokio::test]
async fn applies_as5438_table_8() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    // LFRT
    // The requested curve should have been updated.
    assert_register(&mock, "model709::ADPT_CRV_REQ", EXPECTED_ADPT_CRV_REQ).await;
    assert_register(&mock, "model709::ENA", model709::Ena::Enabled).await;
    // And the curve data filled in.
    assert_curve_data(
        &mock,
        "model709::CRV_2_ACT_PT",
        EXPECTED_DER_TRIP_LF_DATA,
        model709::MustTrip::LEN,
    )
    .await;

    // HFRT
    // The requested curve should have been updated.
    assert_register(&mock, "model710::ADPT_CRV_REQ", EXPECTED_ADPT_CRV_REQ).await;
    assert_register(&mock, "model710::ENA", model710::Ena::Enabled).await;
    // And the curve data filled in.
    assert_curve_data(
        &mock,
        "model710::CRV_2_ACT_PT",
        EXPECTED_DER_TRIP_HF_DATA,
        model710::MustTrip::LEN,
    )
    .await;
}

/// Tests that an active DERControl's opModLVRTMustTrip, opModLVRTMomCess,
/// opModHVRTMustTrip and opModHVRTMomCess reach models 707 and 708.
/// (AS5438 - Table 7)
#[tokio::test]
async fn applies_as5438_table_7() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    // LVRT
    // The requested curve should have been updated.
    assert_register(&mock, "model707::ADPT_CRV_REQ", EXPECTED_ADPT_CRV_REQ).await;
    assert_register(&mock, "model707::ENA", model707::Ena::Enabled).await;
    // And the MustTrip curve data filled in.
    assert_curve_data(
        &mock,
        "model707::CRV_2_MUST_TRIP",
        EXPECTED_DER_TRIP_LV_MUST_DATA,
        model707::MustTrip::LEN,
    )
    .await;
    // And the MomCess curve data filled in.
    assert_curve_data(
        &mock,
        "model707::CRV_2_MOM_CESS",
        EXPECTED_DER_TRIP_LV_MOM_CESS_DATA,
        model707::MomCess::LEN,
    )
    .await;

    // HVRT
    // The requested curve should have been updated.
    assert_register(&mock, "model708::ADPT_CRV_REQ", EXPECTED_ADPT_CRV_REQ).await;
    assert_register(&mock, "model708::ENA", model708::Ena::Enabled).await;
    // And the MustTrip curve data filled in.
    assert_curve_data(
        &mock,
        "model708::CRV_2_MUST_TRIP",
        EXPECTED_DER_TRIP_HV_MUST_DATA,
        model708::MustTrip::LEN,
    )
    .await;
    // And the MomCess curve data filled in.
    assert_curve_data(
        &mock,
        "model708::CRV_2_MOM_CESS",
        EXPECTED_DER_TRIP_HV_MOM_CESS_DATA,
        model708::MomCess::LEN,
    )
    .await;
}

/// Tests that an active DERControl's opModVoltWatt reaches model 706.
/// (AS5438 - Table 6)
#[tokio::test]
async fn applies_as5438_table_6() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    // The function is enabled
    assert_register(&mock, "model706::ENA", model706::Ena::Enabled).await;
    // And the requested curve should have been updated.
    assert_register(&mock, "model706::ADPT_CRV_REQ", EXPECTED_ADPT_CRV_REQ).await;
    // And the TMS is filled
    assert_register(&mock, "model706::CRV_2_RSP_TMS", EXPECTED_DER_VOLT_WATT_TMS).await;
    // And the dependent reference for the y axis.
    assert_register(
        &mock,
        "model706::CRV_2_DEPT_REF",
        EXPECTED_DER_VOLT_WATT_Y_REF,
    )
    .await;
    // And the curve data filled in.
    assert_curve_data(
        &mock,
        "model706::CRV_2_ACT_PT",
        EXPECTED_DER_VOLT_WATT_DATA,
        model706::Crv::LEN,
    )
    .await;
}

/// Tests that an active DERControl's constant reactive power mode values reach model 704.
/// (AS5438 - Table 5)
#[tokio::test]
async fn applies_as5438_table_5() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    assert_register(
        &mock,
        "model704::VAR_SET_ENA",
        Some(model704::VarSetEna::Enabled),
    )
    .await;
    assert_register(&mock, "model704::VAR_SET_PCT", Some(EXPECTED_VAR_SET_PCT)).await;
    assert_register(&mock, "model704::VAR_SET_MOD", Some(EXPECTED_VAR_SET_MOD)).await;
}

/// Tests that an active DERControl's opModVoltVar reaches model 705.
/// (AS5438 - Table 4)
#[tokio::test]
async fn applies_as5438_table_4() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    // The function is enabled
    assert_register(&mock, "model705::ENA", model705::Ena::Enabled).await;
    // And the requested curve should have been updated.
    assert_register(&mock, "model705::ADPT_CRV_REQ", EXPECTED_ADPT_CRV_REQ).await;
    // And the other parameters are filled
    assert_register(&mock, "model705::CRV_2_RSP_TMS", EXPECTED_DER_VOLT_VAR_TMS).await;
    assert_register(
        &mock,
        "model705::CRV_2_DEPT_REF",
        EXPECTED_DER_VOLT_VAR_Y_REF,
    )
    .await;
    assert_register(&mock, "model705::CRV_2_V_REF", EXPECTED_DER_VOLT_VAR_V_REF).await;
    assert_register(
        &mock,
        "model705::CRV_2_V_REF_AUTO_ENA",
        model705::CrvVRefAutoEna::Enabled,
    )
    .await;
    assert_register(
        &mock,
        "model705::CRV_2_V_REF_AUTO_TMS",
        EXPECTED_DER_VOLT_VAR_V_REF_AUTO_TMS,
    )
    .await;
    // And the curve data filled in.
    assert_curve_data(
        &mock,
        "model705::CRV_2_ACT_PT",
        EXPECTED_DER_VOLT_VAR_DATA,
        model705::Crv::LEN,
    )
    .await;
}

/// Tests that an active DERControl's constant power factor mode values reach model 704.
/// (AS5438 - Table 3)
#[tokio::test]
async fn applies_as5438_table_3() {
    let (mock, _sep2_mock, _tasks, _modbus_events) = setup().await;

    // Inject power factors
    assert_register(
        &mock,
        "model704::PFW_INJ_ENA",
        Some(model704::PfwInjEna::Enabled),
    )
    .await;
    assert_register(&mock, "model704::PFW_INJ_PF", Some(EXPECTED_PFW_INJ_PF)).await;
    assert_register(&mock, "model704::PFW_INJ_EXT", Some(EXPECTED_PFW_INJ_EXT)).await;

    // Absorb power factors
    assert_register(
        &mock,
        "model704::PFW_ABS_ENA",
        Some(model704::PfwAbsEna::Enabled),
    )
    .await;
    assert_register(&mock, "model704::PFW_ABS_PF", Some(EXPECTED_PFW_ABS_PF)).await;
    assert_register(&mock, "model704::PFW_ABS_EXT", Some(EXPECTED_PFW_ABS_EXT)).await;
}

/////
// Helpers

/// Waits for a named register on the device mock to reach `expected` and then
/// asserts on it, so a failure reports the last value actually seen.
///
/// Note that a failure to translate the SEP2 attributes is only logged as a
/// warning by `dispatch::control_change_dispatcher`, so it surfaces here as a
/// register that never changes.
async fn assert_register<T>(mock: &SunSpecMock, name: &str, expected: T)
where
    T: Value + PartialEq + std::fmt::Debug,
{
    let deadline = Instant::now() + SETTLE_TIMEOUT;

    let mut actual = mock.get_value::<T>(name);
    while actual != expected && Instant::now() < deadline {
        time::sleep(POLL_INTERVAL).await;
        actual = mock.get_value::<T>(name);
    }

    assert_eq!(
        actual, expected,
        "register {name} never reached the expected value"
    );
}

/// Checks a series of registers to validate the assignment of curve data.
///
/// Note that this doesn't keep polling the registers unlike assert_register, as it
/// assumes all data has been set prior. This is reasonable as a curve will
/// require an AdptCrvReq to be set last by the client and this should be polled
/// before querying for the curve data, much like the behaviour of a real
/// device.
async fn assert_curve_data<TX, TY>(
    mock: &SunSpecMock,
    name: &str,
    expected: &[(TX, TY)],
    curve_static_len: u16,
) where
    TX: Value + PartialEq + std::fmt::Debug,
    TY: Value + PartialEq + std::fmt::Debug,
{
    let mut addr = mock.get_name_addr(name);

    // First the active point count
    let actual = mock.get_value_at_addr::<u16>(addr, 1);
    assert_eq!(usize::from(actual), expected.len());
    addr += usize::from(curve_static_len);

    // Then each pair of data points.
    // Note: TX and TY should always be word-aligned. Just in case of something
    // unusual, we will take the ceil of the divide so that we cannot end up
    // with a zero-length size.
    let x_size = std::mem::size_of::<TX>().div_ceil(2);
    let y_size = std::mem::size_of::<TY>().div_ceil(2);
    for (x, y) in expected.iter() {
        let actual_x = mock.get_value_at_addr::<TX>(addr, x_size);
        assert_eq!(&actual_x, x);
        addr += x_size;
        let actual_y = mock.get_value_at_addr::<TY>(addr, y_size);
        assert_eq!(&actual_y, y);
        addr += y_size;
    }
}

fn mock_lfdi() -> HexBinary160 {
    HexBinary160::from_str("00112233").expect("Invalid LFDI")
}

/// Starts a mocked sunspec device, a mocked SEP2 server exposing one active
/// control, and the full set of bridge tasks wiring the two together.
async fn setup() -> (
    SunSpecMock,
    MockServer,
    JoinSet<Result<()>>,
    async_broadcast::Receiver<modbus_connection::Event>,
) {
    let lfdi = mock_lfdi();
    let sfdi = SFDIType::new(42).expect("Invalid SFDI");
    let device = SEDevice::new(lfdi, sfdi, DeviceCategoryType::empty());

    // The mocked modbus device.
    let mut sunspec_mock = SunSpecMock::new(None)
        .await
        .expect("Couldn't create mock modbus server");
    sunspec_mock
        .start()
        .await
        .expect("Couldn't start mock modbus server");

    // The mocked SEP2 server. All endpoints are mounted before the tasks start
    // so that no polling cycle has to elapse before they are seen.
    let sep2_mock = MockServer::start().await;
    setup_base_mocks(&sep2_mock, lfdi, sfdi).await;
    setup_control_mocks(&sep2_mock).await;

    let client = Client::new(
        &format!("http://{}", sep2_mock.address()),
        None,
        Some(Duration::from_secs(1)),
    )
    .expect("Unable to create client");

    let mut join_set = JoinSet::new();

    // Start the SEP2 connection management task.
    let (sep2_conn_input_tx, sep2_conn_input_rx) = mpsc::channel(10);
    let (sep2_conn_output_tx, sep2_conn_output_rx) = deactivated_broadcast(10);
    join_set.spawn({
        let sep2_conn_input_tx = sep2_conn_input_tx.clone();
        async move {
            sep2_connection::task(
                sep2_conn_output_tx,
                sep2_conn_input_rx,
                sep2_conn_input_tx,
                sep2_connection::Sep2ConnectionArgs {
                    client,
                    dcap_uri: String::from("/dcap"),
                    max_list_size: 30,
                    default_poll_rate: 1,
                    device_to_register: device,
                    expected_pin: None,
                    pen: 42,
                },
            )
            .await
        }
    });

    // Start the scheduler task.
    let (scheduler_input_tx, scheduler_input_rx) = mpsc::channel(10);
    let (scheduler_output_tx, scheduler_output_rx) = deactivated_broadcast(10);
    join_set.spawn(scheduler::task(
        scheduler_output_tx,
        scheduler_input_rx,
        scheduler_input_tx.clone(),
        lfdi,
    ));

    // Start the modbus task.
    let (modbus_input_tx, modbus_input_rx) = mpsc::channel(10);
    let (mut modbus_output_tx, modbus_output_rx) = async_broadcast::broadcast(10);
    // Nothing is consuming from the output channel in these tests, so allow it to overflow.
    modbus_output_tx.set_overflow(true);
    join_set.spawn(modbus_connection::task(
        modbus_output_tx,
        modbus_input_rx,
        modbus_connection::Transport::Tcp(sunspec_mock.addr.unwrap()),
        1,
    ));

    // Dispatch sep2_conn events to the scheduler.
    join_set.spawn(dispatch::resource_update_dispatcher(
        sep2_conn_output_rx.activate_cloned(),
        scheduler_input_tx.clone(),
    ));

    // Dispatch scheduler events.
    join_set.spawn(dispatch::sep2_subscription_and_notification_dispatcher(
        scheduler_output_rx.activate_cloned(),
        sep2_conn_input_tx.clone(),
    ));
    join_set.spawn(dispatch::control_change_dispatcher(
        scheduler_output_rx.activate_cloned(),
        modbus_input_tx.clone(),
    ));

    // Wake up the sep2_connection task to begin its work. Because all mocks are
    // in place already, this should speed through.
    sep2_conn_input_tx
        .send(sep2_connection::Command::Wake)
        .await
        .expect("Send error");

    (sunspec_mock, sep2_mock, join_set, modbus_output_rx)
}

async fn mock_get(mock: &MockServer, path: String, body: String) {
    Mock::given(matchers::method("GET"))
        .and(matchers::path(path.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/sep+xml"))
        .expect(1..)
        .named(path)
        .mount(mock)
        .await;
}

/// Serialises a SEP2 resource and mounts it at the given path.
async fn mock_resource<R: SEType>(mock: &MockServer, path: &str, resource: &R) {
    let body = sep2_common::serialize(resource).expect("Unable to serialize resource");
    mock_get(mock, String::from(path), body).await;
}

/// Mounts the endpoints the sep2_connection task always queries.
async fn setup_base_mocks(mock: &MockServer, lfdi: HexBinary160, sfdi: SFDIType) {
    let now = Utc::now().timestamp();

    mock_get(mock, String::from("/dcap"),
        format!(r#"<DeviceCapability xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/dcap" pollRate="{MOCK_POLL_RATE}">
  <TimeLink href="/tm"/>
  <EndDeviceListLink href="{HREF_EDEVL}" all="1"/>
</DeviceCapability>"#))
        .await;

    mock_get(mock, String::from(HREF_EDEVL),
        format!(r#"<EndDeviceList xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="{HREF_EDEVL}" subscribable="1" all="1" results="1" pollRate="{MOCK_POLL_RATE}">
  <EndDevice href="{HREF_EDEV}" subscribable="1">
    <DERListLink href="/edev/1/der" all="1"/>
    <deviceCategory>00</deviceCategory>
    <lFDI>{lfdi}</lFDI>
    <LogEventListLink href="/edev/1/lel"/>
    <sFDI>{sfdi}</sFDI>
    <changedTime>{now}</changedTime>
    <enabled>true</enabled>
    <FunctionSetAssignmentsListLink href="{HREF_FSAL}" all="1"/>
    <RegistrationLink href="/edev/1/rg"/><csipaus:ConnectionPointLink href="/edev/1/cp"/>
  </EndDevice>
</EndDeviceList>"#))
        .await;

    mock_get(mock, String::from("/tm"),
        format!(r#"<Time xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/tm">
  <currentTime>{now}</currentTime>
  <dstEndTime>0</dstEndTime>
  <dstOffset>0</dstOffset>
  <dstStartTime>0</dstStartTime>
  <quality>4</quality>
  <tzOffset>0</tzOffset>
</Time>"#))
        .await;

    mock_get(mock, String::from("/edev/1/der"),
        format!(r#"<DERList xmlns="urn:ieee:std:2030.5:ns" xmlns:csipaus="https://csipaus.org/ns" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" href="/edev/1/der" all="1" results="1" pollRate="{MOCK_POLL_RATE}">
  <DER href="/edev/1/der/1">
    <AssociatedDERProgramListLink href="/edev/1/der/1/derp"/>
    <DERAvailabilityLink href="/edev/1/der/1/dera"/>
    <DERCapabilityLink href="/edev/1/der/1/dercap"/>
    <DERSettingsLink href="/edev/1/der/1/derg"/>
    <DERStatusLink href="/edev/1/der/1/ders"/>
  </DER>
</DERList>"#))
        .await;
}

/// Mounts the chain of resources leading from the EndDevice to a single active
/// DERControl and the program's DefaultDERControl.
async fn setup_control_mocks(mock: &MockServer) {
    mock_resource(
        mock,
        HREF_FSAL,
        &FunctionSetAssignmentsList {
            href: Some(HREF_FSAL.into()),

            function_set_assignments: vec![FunctionSetAssignments {
                href: Some(HREF_FSA.into()),
                der_program_list_link: Some(ListLink {
                    href: HREF_DERPL.into(),
                    ..Default::default()
                }),
                mrid: MRIDType(1),
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        },
    )
    .await;

    mock_resource(
        mock,
        HREF_DERPL,
        &DERProgramList {
            href: Some(HREF_DERPL.into()),

            der_program: vec![DERProgram {
                href: Some(HREF_DERP.into()),
                default_der_control_link: Some(Link {
                    href: HREF_DDERC.into(),
                }),
                der_control_list_link: Some(ListLink {
                    href: HREF_DERCL.into(),
                    ..Default::default()
                }),
                der_curve_list_link: Some(ListLink {
                    href: HREF_CURVEL.into(),
                    ..Default::default()
                }),

                mrid: MRIDType(123),

                primacy: PrimacyType::NonContractualServiceProvider,
                ..Default::default()
            }],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        },
    )
    .await;

    mock_resource(
        mock,
        HREF_DDERC,
        &DefaultDERControl {
            href: Some(HREF_DDERC.into()),

            set_es_high_volt: Some(Int16(SET_ES_HIGH_VOLT)),
            set_es_low_volt: Some(Int16(SET_ES_LOW_VOLT)),
            set_es_high_freq: Some(Uint16(SET_ES_HIGH_FREQ)),
            set_es_low_freq: Some(Uint16(SET_ES_LOW_FREQ)),
            set_es_delay: Some(Uint32(SET_ES_DELAY)),
            set_es_random_delay: Some(Uint32(SET_ES_RANDOM_DELAY)),
            set_es_ramp_tms: Some(Uint32(SET_ES_RAMP_TMS)),

            ..Default::default()
        },
    )
    .await;

    // A control that has already started and stays active well past the end of
    // the test.
    let der_control = DERControl {
        href: Some(HREF_DERC_1.into()),
        mrid: MRIDType(42),

        der_control_base: DERControlBase {
            op_mod_connect: Some(SET_ES_CONNECT),
            op_mod_max_lim_w: Some(Percent::new(OP_MOD_MAX_LIM_W).expect("Invalid percent")),
            op_mod_fixed_w: Some(SignedPercent::new(OP_MOD_FIXED_W).expect("Invalid percent")),
            op_mod_freq_droop: Some(FreqDroopType {
                d_bof: Uint32(DROOP_DB_OF),
                d_buf: Uint32(DROOP_DB_UF),
                k_of: Uint16(DROOP_K_OF),
                k_uf: Uint16(DROOP_K_UF),
                open_loop_tms: Uint16(DROOP_OPEN_LOOP_TMS),
            }),
            op_mod_lfrt_must_trip: Some(Link {
                href: HREF_LFRT_MUST_TRIP_CURVE.into(),
            }),
            op_mod_hfrt_must_trip: Some(Link {
                href: HREF_HFRT_MUST_TRIP_CURVE.into(),
            }),
            op_mod_lvrt_must_trip: Some(Link {
                href: HREF_LVRT_MUST_TRIP_CURVE.into(),
            }),
            op_mod_lvrt_momentary_cessation: Some(Link {
                href: HREF_LVRT_MOM_CESS_CURVE.into(),
            }),
            op_mod_hvrt_must_trip: Some(Link {
                href: HREF_HVRT_MUST_TRIP_CURVE.into(),
            }),
            op_mod_hvrt_momentary_cessation: Some(Link {
                href: HREF_HVRT_MOM_CESS_CURVE.into(),
            }),
            op_mod_volt_watt: Some(Link {
                href: HREF_VOLT_WATT_CURVE.into(),
            }),
            op_mod_volt_var: Some(Link {
                href: HREF_VOLT_VAR_CURVE.into(),
            }),
            op_mod_fixed_pf_inject_w: Some(PowerFactorWithExcitation {
                displacement: Uint16(OP_MOD_FIXED_PF_INJECT_W),
                excitation: OP_MOD_FIXED_PF_INJECT_W_EXT,
                multiplier: PowerOfTenMultiplierType::None,
            }),
            op_mod_fixed_pf_absorb_w: Some(PowerFactorWithExcitation {
                displacement: Uint16(OP_MOD_FIXED_PF_ABSORB_W),
                excitation: OP_MOD_FIXED_PF_ABSORB_W_EXT,
                multiplier: PowerOfTenMultiplierType::None,
            }),
            op_mod_fixed_var: Some(FixedVar {
                value: SignedPercent::new(OP_MOD_FIXED_VAR).expect("Invalid percent"),
                ref_type: OP_MOD_FIXED_VAR_MOD,
            }),
            ..Default::default()
        },

        interval: DateTimeInterval {
            start: Int64(Utc::now().timestamp()),
            duration: Uint32(3600),
        },
        ..Default::default()
    };

    mock_resource(
        mock,
        HREF_DERCL,
        &DERControlList {
            href: Some(HREF_DERCL.into()),
            der_control: vec![der_control],

            all: Uint32(1),
            results: Uint32(1),

            ..Default::default()
        },
    )
    .await;

    let lfrt_must_trip_curve = DERCurve {
        href: Some(HREF_LFRT_MUST_TRIP_CURVE.into()),
        mrid: MRIDType(1001),

        curve_data: OP_MOD_LFRT_DATA
            .iter()
            .map(|(x, y)| CurveData {
                xvalue: Int32(*x),
                yvalue: Int32(*y),
                ..Default::default()
            })
            .collect(),
        x_multiplier: OP_MOD_LFRT_SF_X,
        y_multiplier: OP_MOD_LFRT_SF_Y,
        ..Default::default()
    };
    let hfrt_must_trip_curve = DERCurve {
        href: Some(HREF_HFRT_MUST_TRIP_CURVE.into()),
        mrid: MRIDType(1002),

        curve_data: OP_MOD_HFRT_DATA
            .iter()
            .map(|(x, y)| CurveData {
                xvalue: Int32(*x),
                yvalue: Int32(*y),
                ..Default::default()
            })
            .collect(),
        x_multiplier: OP_MOD_HFRT_SF_X,
        y_multiplier: OP_MOD_HFRT_SF_Y,
        ..Default::default()
    };

    let lvrt_must_trip_curve = DERCurve {
        href: Some(HREF_LVRT_MUST_TRIP_CURVE.into()),
        mrid: MRIDType(1003),

        curve_data: OP_MOD_LVRT_MUST_DATA
            .iter()
            .map(|(x, y)| CurveData {
                xvalue: Int32(*x),
                yvalue: Int32(*y),
                ..Default::default()
            })
            .collect(),
        x_multiplier: OP_MOD_LVRT_SF_X,
        y_multiplier: OP_MOD_LVRT_SF_Y,
        ..Default::default()
    };

    let lvrt_mom_cess_curve = DERCurve {
        href: Some(HREF_LVRT_MOM_CESS_CURVE.into()),
        mrid: MRIDType(1004),

        curve_data: OP_MOD_LVRT_MOM_CESS_DATA
            .iter()
            .map(|(x, y)| CurveData {
                xvalue: Int32(*x),
                yvalue: Int32(*y),
                ..Default::default()
            })
            .collect(),
        x_multiplier: OP_MOD_LVRT_SF_X,
        y_multiplier: OP_MOD_LVRT_SF_Y,
        ..Default::default()
    };

    let hvrt_must_trip_curve = DERCurve {
        href: Some(HREF_HVRT_MUST_TRIP_CURVE.into()),
        mrid: MRIDType(1005),

        curve_data: OP_MOD_HVRT_MUST_DATA
            .iter()
            .map(|(x, y)| CurveData {
                xvalue: Int32(*x),
                yvalue: Int32(*y),
                ..Default::default()
            })
            .collect(),
        x_multiplier: OP_MOD_HVRT_SF_X,
        y_multiplier: OP_MOD_HVRT_SF_Y,
        ..Default::default()
    };

    let hvrt_mom_cess_curve = DERCurve {
        href: Some(HREF_HVRT_MOM_CESS_CURVE.into()),
        mrid: MRIDType(1006),

        curve_data: OP_MOD_HVRT_MOM_CESS_DATA
            .iter()
            .map(|(x, y)| CurveData {
                xvalue: Int32(*x),
                yvalue: Int32(*y),
                ..Default::default()
            })
            .collect(),
        x_multiplier: OP_MOD_HVRT_SF_X,
        y_multiplier: OP_MOD_HVRT_SF_Y,
        ..Default::default()
    };

    let volt_watt_curve = DERCurve {
        href: Some(HREF_VOLT_WATT_CURVE.into()),
        mrid: MRIDType(1007),

        curve_data: OP_MOD_VOLT_WATT_DATA
            .iter()
            .map(|(x, y)| CurveData {
                xvalue: Int32(*x),
                yvalue: Int32(*y),
                ..Default::default()
            })
            .collect(),
        x_multiplier: OP_MOD_VOLT_WATT_SF_X,
        y_multiplier: OP_MOD_VOLT_WATT_SF_Y,
        y_ref_type: OP_MOD_VOLT_WATT_Y_REF,
        open_loop_tms: Some(Uint16(OP_MOD_VOLT_WATT_TMS)),
        ..Default::default()
    };

    let volt_var_curve = DERCurve {
        href: Some(HREF_VOLT_VAR_CURVE.into()),
        mrid: MRIDType(1008),

        curve_data: OP_MOD_VOLT_VAR_DATA
            .iter()
            .map(|(x, y)| CurveData {
                xvalue: Int32(*x),
                yvalue: Int32(*y),
                ..Default::default()
            })
            .collect(),
        x_multiplier: OP_MOD_VOLT_VAR_SF_X,
        y_multiplier: OP_MOD_VOLT_VAR_SF_Y,
        y_ref_type: OP_MOD_VOLT_VAR_Y_REF,
        open_loop_tms: Some(Uint16(OP_MOD_VOLT_VAR_TMS)),
        v_ref: Percent::new(OP_MOD_VOLT_VAR_V_REF),
        autonomous_v_ref_enable: Some(true),
        autonomous_v_ref_time_constant: Some(Uint32(OP_MOD_VOLT_VAR_V_REF_AUTO_TMS)),
        ..Default::default()
    };

    mock_resource(
        mock,
        HREF_CURVEL,
        &DERCurveList {
            href: Some(HREF_CURVEL.into()),
            der_curve: vec![
                lfrt_must_trip_curve,
                hfrt_must_trip_curve,
                lvrt_must_trip_curve,
                lvrt_mom_cess_curve,
                hvrt_must_trip_curve,
                hvrt_mom_cess_curve,
                volt_watt_curve,
                volt_var_curve,
            ],

            all: Uint32(8),
            results: Uint32(8),
        },
    )
    .await;
}
