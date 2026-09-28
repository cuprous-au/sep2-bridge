// Tests the full path device state takes from the device to the SEP2 server.

mod bridge_harness;
mod modbus_server_mock;

use std::time::Duration;

use bridge_harness::{HREF_DERCAP, HREF_DERG, HREF_DERS, setup_base_mocks, start_bridge};
use modbus_server_mock::SunSpecMock;
use sep2_bridge::Result;
use sep2_common::{
    packages::{
        der::{
            ActivePower, ApparentPower, ConnectStatusValue, DERCapability, DERControlType,
            DERSettings, DERStatus, OperationalModeStatusValue, PowerFactor, ReactivePower,
            ReactiveSusceptance, VoltageRMS,
        },
        primitives::{Int16, Uint16, Uint32},
        types::{Percent, PowerOfTenMultiplierType},
    },
    traits::SEType,
};
use sunspec::models::{model701, model702::CtrlModes};
use tokio::{
    task::JoinSet,
    time::{self, Instant},
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

// The maximum time a value is expected to take to travel from the device to the SEP2 server.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(3);

// How often the requests received by the SEP2 server are re-checked while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

// The values to be mocked and verified.
//
// SEP2 either fixes the scale of each value it carries, or carries a
// multiplier alongside it, while the device advertises its own scale factor per
// group of registers. The mock deliberately uses scale factors that differ from
// SEP2's fixed scales, so each expectation below is the device value restated
// at SEP2's scale.

// DERCapability:
const DEVICE_W_SF: i16 = 1;
const DEVICE_PF_SF: i16 = -3;
const DEVICE_VA_SF: i16 = 1;
const DEVICE_VAR_SF: i16 = 1;
const DEVICE_V_SF: i16 = -1;
const DEVICE_S_SF: i16 = -2;
const EXPECTED_W_MULTIPLIER: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Deca;
const EXPECTED_PF_MULTIPLIER: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Milli;
const EXPECTED_VA_MULTIPLIER: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Deca;
const EXPECTED_VAR_MULTIPLIER: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Deca;
const EXPECTED_V_MULTIPLIER: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Deci;
const EXPECTED_S_MULTIPLIER: PowerOfTenMultiplierType = PowerOfTenMultiplierType::Centi;

const DEVICE_W_MAX_RTG: u16 = 500;
const DEVICE_W_OVR_EXT_RTG: u16 = 480;
const DEVICE_W_OVR_EXT_RTG_PF: u16 = 900;
const DEVICE_W_UND_EXT_RTG: u16 = 470;
const DEVICE_W_UND_EXT_RTG_PF: u16 = 850;
const DEVICE_VA_MAX_RTG: u16 = 520;
const DEVICE_VAR_MAX_INJ_RTG: u16 = 300;
const DEVICE_VAR_MAX_ABS_RTG: u16 = 310;
// Sunspec gives the absorbed rating as a magnitude, SEP2 expects it negative.
const EXPECTED_RTG_MAX_VAR_NEG: i16 = -310;
const DEVICE_W_CHA_RTE_MAX_RTG: u16 = 450;
const DEVICE_VA_CHA_RTE_MAX_RTG: u16 = 510;
const DEVICE_V_NOM_RTG: u16 = 2300;
const DEVICE_V_MAX_RTG: u16 = 2530;
const DEVICE_V_MIN_RTG: u16 = 2070;
const DEVICE_REACT_SUSCEPT_RTG: u16 = 1234;

const DEVICE_CTRL_MODES: CtrlModes = CtrlModes::MaxW
    .union(CtrlModes::FixedPf)
    .union(CtrlModes::VoltVar);
const EXPECTED_MODES_SUPPORTED: DERControlType = DERControlType::OpModMaxLimW
    // The FixedPf maps to both the Inject and Absorb options.
    .union(DERControlType::OpModFixedPFInjectW)
    .union(DERControlType::OpModFixedPFAbsorbW)
    .union(DERControlType::OpModVoltVar);

// DERSettings. All SEP2 SFs are hundredths.
// The device mock sues V_SF=-1 and HZ_SF=-3.
const DEVICE_ESV_HI: u16 = 1100; // 110.0%
const DEVICE_ESV_LO: u16 = 880; // 88.0%
const DEVICE_ES_HZ_HI: u32 = 50_150; // 50.150 Hz
const DEVICE_ES_HZ_LO: u32 = 47_500; // 47.500 Hz
// Sunspec's times are seconds so a scale factor of 0.
const DEVICE_ES_DLY_TMS: u32 = 60;
const DEVICE_ES_RND_TMS: u32 = 30;
const DEVICE_ES_RMP_TMS: u32 = 300;
const EXPECTED_SET_ES_HIGH_VOLT: i16 = 11_000;
const EXPECTED_SET_ES_LOW_VOLT: i16 = 8_800;
const EXPECTED_SET_ES_HIGH_FREQ: u16 = 5_015;
const EXPECTED_SET_ES_LOW_FREQ: u16 = 4_750;
const EXPECTED_SET_ES_DELAY: u32 = 6_000;
const EXPECTED_SET_ES_RANDOM_DELAY: u32 = 3_000;
const EXPECTED_SET_ES_RAMP_TMS: u32 = 30_000;

// DERStatus
const DEVICE_SOC: u16 = 755; // 75.5%
const EXPECTED_STATE_OF_CHARGE: u16 = 7_550;

/// Tests that model 702 reaches the server as a DERCapability.
#[tokio::test]
async fn sends_der_capability() {
    let (_mock, sep2_mock, _tasks) = setup().await;

    let cap: DERCapability = wait_for_put(&sep2_mock, HREF_DERCAP).await;

    assert_eq!(
        cap.rtg_max_w,
        ActivePower {
            value: Int16(DEVICE_W_MAX_RTG as i16),
            multiplier: EXPECTED_W_MULTIPLIER,
        }
    );
    assert_eq!(
        cap.rtg_over_excited_w,
        Some(ActivePower {
            value: Int16(DEVICE_W_OVR_EXT_RTG as i16),
            multiplier: EXPECTED_W_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_over_excited_pf,
        Some(PowerFactor {
            displacement: Uint16(DEVICE_W_OVR_EXT_RTG_PF),
            multiplier: EXPECTED_PF_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_under_excited_w,
        Some(ActivePower {
            value: Int16(DEVICE_W_UND_EXT_RTG as i16),
            multiplier: EXPECTED_W_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_under_excited_pf,
        Some(PowerFactor {
            displacement: Uint16(DEVICE_W_UND_EXT_RTG_PF),
            multiplier: EXPECTED_PF_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_max_va,
        Some(ApparentPower {
            value: Uint16(DEVICE_VA_MAX_RTG),
            multiplier: EXPECTED_VA_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_max_var,
        Some(ReactivePower {
            value: Int16(DEVICE_VAR_MAX_INJ_RTG as i16),
            multiplier: EXPECTED_VAR_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_max_var_neg,
        Some(ReactivePower {
            value: Int16(EXPECTED_RTG_MAX_VAR_NEG),
            multiplier: EXPECTED_VAR_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_max_charge_rate_w,
        Some(ActivePower {
            value: Int16(DEVICE_W_CHA_RTE_MAX_RTG as i16),
            multiplier: EXPECTED_W_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_max_charge_rate_va,
        Some(ApparentPower {
            value: Uint16(DEVICE_VA_CHA_RTE_MAX_RTG),
            multiplier: EXPECTED_VA_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_v_nom,
        Some(VoltageRMS {
            value: Uint16(DEVICE_V_NOM_RTG),
            multiplier: EXPECTED_V_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_max_v,
        Some(VoltageRMS {
            value: Uint16(DEVICE_V_MAX_RTG),
            multiplier: EXPECTED_V_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_min_v,
        Some(VoltageRMS {
            value: Uint16(DEVICE_V_MIN_RTG),
            multiplier: EXPECTED_V_MULTIPLIER,
        })
    );
    assert_eq!(
        cap.rtg_reactive_susceptance,
        Some(ReactiveSusceptance {
            value: Uint16(DEVICE_REACT_SUSCEPT_RTG),
            multiplier: EXPECTED_S_MULTIPLIER,
        })
    );
    assert_eq!(cap.modes_supported, EXPECTED_MODES_SUPPORTED);
}

/// Tests that model 703 reaches the server as DERSettings.
#[tokio::test]
async fn sends_der_settings() {
    let (_mock, sep2_mock, _tasks) = setup().await;

    let settings: DERSettings = wait_for_put(&sep2_mock, HREF_DERG).await;

    assert_eq!(
        settings.set_es_high_volt,
        Some(Int16(EXPECTED_SET_ES_HIGH_VOLT))
    );
    assert_eq!(
        settings.set_es_low_volt,
        Some(Int16(EXPECTED_SET_ES_LOW_VOLT))
    );
    assert_eq!(
        settings.set_es_high_freq,
        Some(Uint16(EXPECTED_SET_ES_HIGH_FREQ))
    );
    assert_eq!(
        settings.set_es_low_freq,
        Some(Uint16(EXPECTED_SET_ES_LOW_FREQ))
    );
    assert_eq!(settings.set_es_delay, Some(Uint32(EXPECTED_SET_ES_DELAY)));
    assert_eq!(
        settings.set_es_random_delay,
        Some(Uint32(EXPECTED_SET_ES_RANDOM_DELAY))
    );
    assert_eq!(
        settings.set_es_ramp_tms,
        Some(Uint32(EXPECTED_SET_ES_RAMP_TMS))
    );
}

/// Tests that models 701 and 713 reach the server as a DERStatus.
#[tokio::test]
async fn sends_der_status() {
    let (_mock, sep2_mock, _tasks) = setup().await;

    let status: DERStatus = wait_for_put(&sep2_mock, HREF_DERS).await;

    assert_eq!(
        status.operational_mode_status.map(|status| status.value),
        Some(OperationalModeStatusValue::Operational)
    );
    // The mock seeds CONN_ST as None, so this cannot pass vacuously.
    assert_eq!(
        status.gen_connect_status.map(|status| status.value),
        Some(ConnectStatusValue::Connected)
    );
    assert_eq!(
        status.stor_connect_status.map(|status| status.value),
        Some(ConnectStatusValue::Connected)
    );
    assert_eq!(
        status.state_of_charge_status.map(|status| status.value),
        Percent::new(EXPECTED_STATE_OF_CHARGE)
    );
}

/////
// Helpers

/// Waits for a PUT to `path` on the SEP2 mock and returns the latest body
/// received there as a deserialized resource.
async fn wait_for_put<R: SEType>(mock: &MockServer, path: &str) -> R {
    let deadline = Instant::now() + SETTLE_TIMEOUT;

    while Instant::now() < deadline {
        let requests = mock
            .received_requests()
            .await
            .expect("Request recording is disabled");
        if let Some(request) = requests
            .iter()
            .rev()
            .find(|req| req.method.as_str() == "PUT" && req.url.path() == path)
        {
            let body = String::from_utf8(request.body.clone()).expect("Body is not UTF-8");
            return sep2_common::deserialize(&body).expect("Unable to deserialize body");
        }

        time::sleep(POLL_INTERVAL).await;
    }
    panic!("No PUT to {path} was received in time");
}

/// Starts a mocked sunspec device with known state, a mocked SEP2 server
/// accepting that state, and the full set of bridge tasks wiring the two
/// together.
async fn setup() -> (SunSpecMock, MockServer, JoinSet<Result<()>>) {
    let mut sunspec_mock = SunSpecMock::new(None)
        .await
        .expect("Couldn't create mock modbus server");
    sunspec_mock
        .start()
        .await
        .expect("Couldn't start mock modbus server");
    seed_device_state(&sunspec_mock);

    // The mocked SEP2 server. All endpoints are mounted before the tasks start
    // so that no polling cycle has to elapse before they are seen.
    //
    // Note: the dcap has no MirrorUsagePointListLink, so sending the meter
    // readings will fail and request a retry. This doesn't affect these tests,
    // as the meter readings are always sent after the status and settings.
    let sep2_mock = MockServer::start().await;
    setup_base_mocks(&sep2_mock).await;
    setup_der_mocks(&sep2_mock).await;

    let join_set = start_bridge(sunspec_mock.addr.unwrap(), &sep2_mock).await;

    (sunspec_mock, sep2_mock, join_set)
}

/// Writes the device values to be verified into the mock's registers.
fn seed_device_state(mock: &SunSpecMock) {
    // Model 702
    mock.set_value("model702::W_SF", Some(DEVICE_W_SF));
    mock.set_value("model702::PF_SF", Some(DEVICE_PF_SF));
    mock.set_value("model702::VA_SF", Some(DEVICE_VA_SF));
    mock.set_value("model702::VAR_SF", Some(DEVICE_VAR_SF));
    mock.set_value("model702::V_SF", Some(DEVICE_V_SF));
    mock.set_value("model702::S_SF", Some(DEVICE_S_SF));
    mock.set_value("model702::W_MAX_RTG", Some(DEVICE_W_MAX_RTG));
    mock.set_value("model702::W_OVR_EXT_RTG", Some(DEVICE_W_OVR_EXT_RTG));
    mock.set_value("model702::W_OVR_EXT_RTG_PF", Some(DEVICE_W_OVR_EXT_RTG_PF));
    mock.set_value("model702::W_UND_EXT_RTG", Some(DEVICE_W_UND_EXT_RTG));
    mock.set_value("model702::W_UND_EXT_RTG_PF", Some(DEVICE_W_UND_EXT_RTG_PF));
    mock.set_value("model702::VA_MAX_RTG", Some(DEVICE_VA_MAX_RTG));
    mock.set_value("model702::VAR_MAX_INJ_RTG", Some(DEVICE_VAR_MAX_INJ_RTG));
    mock.set_value("model702::VAR_MAX_ABS_RTG", Some(DEVICE_VAR_MAX_ABS_RTG));
    mock.set_value(
        "model702::W_CHA_RTE_MAX_RTG",
        Some(DEVICE_W_CHA_RTE_MAX_RTG),
    );
    mock.set_value(
        "model702::VA_CHA_RTE_MAX_RTG",
        Some(DEVICE_VA_CHA_RTE_MAX_RTG),
    );
    mock.set_value("model702::V_NOM_RTG", Some(DEVICE_V_NOM_RTG));
    mock.set_value("model702::V_MAX_RTG", Some(DEVICE_V_MAX_RTG));
    mock.set_value("model702::V_MIN_RTG", Some(DEVICE_V_MIN_RTG));
    mock.set_value(
        "model702::REACT_SUSCEPT_RTG",
        Some(DEVICE_REACT_SUSCEPT_RTG),
    );
    mock.set_value("model702::CTRL_MODES", Some(DEVICE_CTRL_MODES));

    // Model 703
    mock.set_value("model703::ESV_HI", Some(DEVICE_ESV_HI));
    mock.set_value("model703::ESV_LO", Some(DEVICE_ESV_LO));
    mock.set_value("model703::ES_HZ_HI", Some(DEVICE_ES_HZ_HI));
    mock.set_value("model703::ES_HZ_LO", Some(DEVICE_ES_HZ_LO));
    mock.set_value("model703::ES_DLY_TMS", Some(DEVICE_ES_DLY_TMS));
    mock.set_value("model703::ES_RND_TMS", Some(DEVICE_ES_RND_TMS));
    mock.set_value("model703::ES_RMP_TMS", Some(DEVICE_ES_RMP_TMS));

    // Models 701 and 713
    mock.set_value("model701::ST", Some(model701::St::On));
    mock.set_value("model701::CONN_ST", Some(model701::ConnSt::Connected));
    mock.set_value("model713::SOC", Some(DEVICE_SOC));
}

/// Mounts the DER endpoints the device state is PUT to.
async fn setup_der_mocks(mock: &MockServer) {
    for path in [HREF_DERCAP, HREF_DERG, HREF_DERS] {
        Mock::given(matchers::method("PUT"))
            .and(matchers::path(path))
            .respond_with(ResponseTemplate::new(204))
            .named(path)
            .mount(mock)
            .await;
    }
}
