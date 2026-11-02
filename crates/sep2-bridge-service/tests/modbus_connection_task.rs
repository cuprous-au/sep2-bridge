mod modbus_server_mock;

use std::time::Duration;

use modbus_server_mock::SunSpecMock;
#[cfg(feature = "map-imp-exp-site-limits")]
use sep2_bridge::scheduler::ControlAttributes;
use sep2_bridge::{
    Result, ScaledValue,
    modbus_connection::{self, Capabilities, Metering, Model711Ctl, Settings, Status, Transport},
};
#[cfg(feature = "map-imp-exp-site-limits")]
use sep2_common::packages::{
    der::{ActivePower, DERControlBase, DefaultDERControl},
    primitives::Int16,
    types::PowerOfTenMultiplierType,
};
use sunspec::models::{model701, model703, model704, model711};
use tokio::{
    sync::mpsc,
    task::{self, JoinHandle},
    time,
};

const WAIT_TIME: Duration = Duration::from_millis(100);
const WAIT_POLL_TIME: Duration = Duration::from_millis(1200);

/// Tests that a device receives parameters when a control is applied.
#[tokio::test]
async fn sends_parameters_to_device() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;

    // Provide a parameters command
    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                es: Some(model703::Es::Enabled),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    // Wait for processing
    time::sleep(WAIT_TIME).await;

    // Ensure the device mock has received the parameters.
    let value = mock.get_value::<model703::Es>("model703::ES");
    assert_eq!(value, model703::Es::Enabled);
}

/// Tests that parameters are rescaled from whatever scale factor they arrive
/// with to the one the device advertises.
///
/// The scale factors here are deliberately not the ones SEP2 fixes its values
/// at, so this covers the rescale itself rather than the SEP2 translation.
#[tokio::test]
async fn rescales_parameters_to_device_scale_factors() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<i16>>("model702::W_SF", Some(-1));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // 24% and 20% of nominal voltage, to the mock's V_SF of -1.
                esv_hi: Some(ScaledValue::new(24, 0)),
                esv_lo: Some(ScaledValue::new(200, -1)),
                // 51.0 Hz and 49.0 Hz, to the mock's HZ_SF of -3.
                es_hz_hi: Some(ScaledValue::new(510, -1)),
                es_hz_lo: Some(ScaledValue::new(4900, -2)),
                // 80% and -25%, to the mock's SFs of 0 and -1.
                w_max_lim_pct: Some(ScaledValue::new(800, -1)),
                w_set_pct: Some(ScaledValue::new(-25, 0)),
                droop_ctl: Some(Model711Ctl {
                    // 1.00 Hz and 0.50 Hz, to the mock's DB_SF of -2.
                    db_of: ScaledValue::new(1, 0),
                    db_uf: ScaledValue::new(500, -3),
                    // 0.05 and 0.04 per unit, to the mock's K_SF of -4.
                    k_of: ScaledValue::new(5, -2),
                    k_uf: ScaledValue::new(40, -3),
                    // 5 seconds, to the mock's RSP_TMS_SF of 0.
                    rsp_tms: ScaledValue::new(5000, -3),
                }),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(mock.get_value::<Option<u16>>("model703::ESV_HI"), Some(240));
    assert_eq!(mock.get_value::<Option<u16>>("model703::ESV_LO"), Some(200));
    assert_eq!(
        mock.get_value::<Option<u32>>("model703::ES_HZ_HI"),
        Some(51_000)
    );
    assert_eq!(
        mock.get_value::<Option<u32>>("model703::ES_HZ_LO"),
        Some(49_000)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-250)
    );
    assert_eq!(mock.get_value::<u32>("model711::CTL_2::DB_OF"), 100);
    assert_eq!(mock.get_value::<u32>("model711::CTL_2::DB_UF"), 50);
    assert_eq!(mock.get_value::<u16>("model711::CTL_2::K_OF"), 500);
    assert_eq!(mock.get_value::<u16>("model711::CTL_2::K_UF"), 400);
    assert_eq!(mock.get_value::<u32>("model711::CTL_2::RSP_TMS"), 5);
    assert_eq!(
        mock.get_value::<model711::Ena>("model711::ENA"),
        model711::Ena::Enabled
    );
    assert_eq!(mock.get_value::<u16>("model711::ADPT_CTL_REQ"), 2);
}

/// Tests that site import/export limits can be converted to AS5438-compatible
/// WSetPct/WMaxLimPct using model 702 WMaxRtg as the base.
#[tokio::test]
async fn maps_site_limits_to_model704_percentages() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // Import: 250W => -25% as WSetPct.
                site_import_limit_w: Some(ScaledValue::new(250, 0)),
                // Export: 800W => 80% as WMaxLimPct.
                site_export_limit_w: Some(ScaledValue::new(800, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<model704::WSetEna>>("model704::W_SET_ENA"),
        Some(model704::WSetEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-25)
    );
    assert_eq!(
        mock.get_value::<Option<model704::WSetMod>>("model704::W_SET_MOD"),
        Some(model704::WSetMod::WMaxPct)
    );
    assert_eq!(
        mock.get_value::<Option<model704::WMaxLimPctEna>>("model704::W_MAX_LIM_PCT_ENA"),
        Some(model704::WMaxLimPctEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
}

/// Tests that site-only controls still enable WSetPct/WMaxLimPct when the
/// translated defaults carry disabled enable flags.
#[cfg(feature = "map-imp-exp-site-limits")]
#[tokio::test]
async fn site_only_controls_enable_percent_limits_after_translation() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    let controls = ControlAttributes {
        inner: DefaultDERControl {
            der_control_base: DERControlBase {
                op_mod_imp_lim_w: Some(ActivePower {
                    multiplier: PowerOfTenMultiplierType::None,
                    value: Int16(250),
                }),
                op_mod_exp_lim_w: Some(ActivePower {
                    multiplier: PowerOfTenMultiplierType::None,
                    value: Int16(800),
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let parameters: modbus_connection::Parameters = controls
        .try_into()
        .expect("Control translation should succeed");
    assert_eq!(parameters.w_set_ena, Some(model704::WSetEna::Disabled));
    assert_eq!(
        parameters.w_max_lim_pct_ena,
        Some(model704::WMaxLimPctEna::Disabled)
    );

    input_ch
        .send(modbus_connection::Command::UpdateParameters(parameters))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<model704::WSetEna>>("model704::W_SET_ENA"),
        Some(model704::WSetEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-25)
    );
    assert_eq!(
        mock.get_value::<Option<model704::WMaxLimPctEna>>("model704::W_MAX_LIM_PCT_ENA"),
        Some(model704::WMaxLimPctEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
}

/// Tests that when both an explicit WMaxLimPct and a derived site export limit
/// are present, the stricter export cap is applied.
#[tokio::test]
async fn combines_explicit_and_site_export_limits_using_stricter_cap() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // 80.00% explicit max limit.
                w_max_lim_pct: Some(ScaledValue::new(8000, -2)),
                // 250W site export limit on a 1kW device => 25%.
                site_export_limit_w: Some(ScaledValue::new(250, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<model704::WMaxLimPctEna>>("model704::W_MAX_LIM_PCT_ENA"),
        Some(model704::WMaxLimPctEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(25)
    );
}

/// Tests that explicit export limits are clamped against the site export cap
/// at device scale so nearest rounding cannot exceed the site limit.
#[tokio::test]
async fn explicit_export_rounding_is_clamped_to_site_cap_at_device_scale() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // Explicit 25.55% rounds to 26% at scale 0.
                w_max_lim_pct: Some(ScaledValue::new(2555, -2)),
                // Site cap 256W on 1kW => 25.6%, which truncates to 25% at scale 0.
                site_export_limit_w: Some(ScaledValue::new(256, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(25)
    );
}

/// Tests that site-derived limits round toward zero so device-side coarse
/// percentage scales do not exceed requested site limits.
#[tokio::test]
async fn site_derived_percentages_round_toward_zero() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(700));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                site_import_limit_w: Some(ScaledValue::new(25, 0)),
                site_export_limit_w: Some(ScaledValue::new(25, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-3)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(3)
    );
}

/// Tests explicit percentage controls keep the existing nearest-value rounding.
#[tokio::test]
async fn explicit_percentages_keep_existing_rounding() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_pct: Some(ScaledValue::new(-357, -2)),
                w_max_lim_pct: Some(ScaledValue::new(357, -2)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-4)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(4)
    );
}

/// Tests explicit max-limit percentages preserve omitted enable fields.
#[tokio::test]
async fn explicit_max_limit_preserves_omitted_enable() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_max_lim_pct: Some(ScaledValue::new(8000, -2)),
                w_max_lim_pct_ena: None,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<model704::WMaxLimPctEna>>("model704::W_MAX_LIM_PCT_ENA"),
        Some(model704::WMaxLimPctEna::Disabled)
    );
}

/// Tests explicit setpoint percentages preserve omitted enable/mode fields.
#[tokio::test]
async fn explicit_setpoint_preserves_omitted_enable_and_mode() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_pct: Some(ScaledValue::new(-2500, -2)),
                w_set_ena: None,
                w_set_mod: None,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<model704::WSetEna>>("model704::W_SET_ENA"),
        Some(model704::WSetEna::Disabled)
    );
    assert_eq!(
        mock.get_value::<Option<model704::WSetMod>>("model704::W_SET_MOD"),
        None
    );
}

/// Tests that site limits remain pending when WMaxRtg is unavailable, and are
/// retried/applied once WMaxRtg becomes valid without changing controls.
#[tokio::test]
async fn retries_site_limits_when_wmaxrtg_becomes_available() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                site_import_limit_w: Some(ScaledValue::new(250, 0)),
                site_export_limit_w: Some(ScaledValue::new(800, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;
    assert_eq!(
        mock.get_value::<Option<model704::WSetEna>>("model704::W_SET_ENA"),
        Some(model704::WSetEna::Disabled)
    );
    assert_eq!(
        mock.get_value::<Option<model704::WMaxLimPctEna>>("model704::W_MAX_LIM_PCT_ENA"),
        Some(model704::WMaxLimPctEna::Disabled)
    );

    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));

    // The loop wakes every second and should retry unchanged pending controls.
    time::sleep(WAIT_POLL_TIME).await;

    assert_eq!(
        mock.get_value::<Option<model704::WSetEna>>("model704::W_SET_ENA"),
        Some(model704::WSetEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-25)
    );
    assert_eq!(
        mock.get_value::<Option<model704::WMaxLimPctEna>>("model704::W_MAX_LIM_PCT_ENA"),
        Some(model704::WMaxLimPctEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
}

/// Tests that site limits remain pending when WMaxRtg is zero, and are
/// retried/applied once WMaxRtg becomes positive without changing controls.
#[tokio::test]
async fn retries_site_limits_when_wmaxrtg_is_zero_then_positive() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(0));
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                site_import_limit_w: Some(ScaledValue::new(250, 0)),
                site_export_limit_w: Some(ScaledValue::new(800, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;
    assert_eq!(
        mock.get_value::<Option<model704::WSetEna>>("model704::W_SET_ENA"),
        Some(model704::WSetEna::Disabled)
    );
    assert_eq!(
        mock.get_value::<Option<model704::WMaxLimPctEna>>("model704::W_MAX_LIM_PCT_ENA"),
        Some(model704::WMaxLimPctEna::Disabled)
    );

    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    time::sleep(WAIT_POLL_TIME).await;

    assert_eq!(
        mock.get_value::<Option<model704::WSetEna>>("model704::W_SET_ENA"),
        Some(model704::WSetEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-25)
    );
    assert_eq!(
        mock.get_value::<Option<model704::WMaxLimPctEna>>("model704::W_MAX_LIM_PCT_ENA"),
        Some(model704::WMaxLimPctEna::Enabled)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
}

/// Tests that a pending write invalidates cached sent-parameters so reverting
/// to a previous parameter set is resent and clears partial writes.
#[tokio::test]
async fn pending_write_invalidates_cache_for_reapplied_previous_parameters() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));

    let previous = modbus_connection::Parameters {
        es: Some(model703::Es::Enabled),
        ..Default::default()
    };
    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            previous.clone(),
        ))
        .await
        .expect("Send error");
    time::sleep(WAIT_TIME).await;
    assert_eq!(
        mock.get_value::<model703::Es>("model703::ES"),
        model703::Es::Enabled
    );

    // Force pending after a partial write by removing WMaxRtg while changing
    // ES and including site limits.
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                es: Some(model703::Es::Disabled),
                site_import_limit_w: Some(ScaledValue::new(250, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");
    time::sleep(WAIT_TIME).await;
    assert_eq!(
        mock.get_value::<model703::Es>("model703::ES"),
        model703::Es::Disabled
    );

    // Reapply previous parameters unchanged; this must resend despite equality
    // with the previously successful cached set.
    input_ch
        .send(modbus_connection::Command::UpdateParameters(previous))
        .await
        .expect("Send error");
    time::sleep(WAIT_TIME).await;
    assert_eq!(
        mock.get_value::<model703::Es>("model703::ES"),
        model703::Es::Enabled
    );
}

/// Tests that pending site-limit derivation does not block independent model
/// updates (for example explicit model 704 controls and model 711 droop).
#[tokio::test]
async fn pending_site_limits_do_not_block_independent_updates() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // Triggers pending site-limit derivation.
                site_export_limit_w: Some(ScaledValue::new(250, 0)),
                // Independent explicit export limit should still be applied.
                w_max_lim_pct: Some(ScaledValue::new(8000, -2)),
                // Independent model 711 update should still be applied.
                droop_ctl: Some(Model711Ctl {
                    db_of: ScaledValue::new(1, 0),
                    db_uf: ScaledValue::new(500, -3),
                    k_of: ScaledValue::new(5, -2),
                    k_uf: ScaledValue::new(40, -3),
                    rsp_tms: ScaledValue::new(5000, -3),
                }),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
    assert_eq!(
        mock.get_value::<model711::Ena>("model711::ENA"),
        model711::Ena::Enabled
    );
    assert_eq!(mock.get_value::<u16>("model711::ADPT_CTL_REQ"), 2);
}

/// Tests high-power ratings use widened scaled arithmetic (no u16 saturation
/// from rescaling to whole watts) when deriving percentages.
#[tokio::test]
async fn derives_percentages_without_whole_watt_saturation() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    // 100 kW rating represented as 100 * 10^3 W.
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(100));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(3));
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(0));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // 50 kW import/export site limits.
                site_import_limit_w: Some(ScaledValue::new(50, 3)),
                site_export_limit_w: Some(ScaledValue::new(50, 3)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-50)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(50)
    );
}

/// Tests that derived site limits retain precision at the destination register
/// scale rather than truncating to fixed hundredths first.
#[tokio::test]
async fn derives_percentages_at_destination_scale() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    // 100 kW rating represented as 100 * 10^3 W.
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(100));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(3));
    // Device supports 0.001% steps.
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(-3));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(-3));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // 1 W import/export site limits => 0.001%.
                site_import_limit_w: Some(ScaledValue::new(1, 0)),
                site_export_limit_w: Some(ScaledValue::new(1, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-1)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(1)
    );
}

/// Tests derived percentages never write SunSpec unimplemented markers when a
/// finer device scale overflows.
#[tokio::test]
async fn derived_percentages_avoid_unimplemented_markers_at_fine_scale() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(0));
    // Finer scale than the internal hundredths representation.
    mock.set_value::<Option<i16>>("model704::W_SET_PCT_SF", Some(-3));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(-3));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // -50% import and +80% export derived limits.
                site_import_limit_w: Some(ScaledValue::new(500, 0)),
                site_export_limit_w: Some(ScaledValue::new(800, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-32767)
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(65534)
    );
}

/// Tests intermediate derived export percentages clamp below the unimplemented
/// marker before device-scale conversion.
#[tokio::test]
async fn derived_export_percentage_clamps_below_unimplemented_marker() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1));
    mock.set_value::<Option<i16>>("model702::W_SF", Some(0));
    mock.set_value::<Option<i16>>("model704::W_MAX_LIM_PCT_SF", Some(-2));

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                site_export_limit_w: Some(ScaledValue::new(1000, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(65534)
    );
}

/// Tests that a device which doesn't implement a scale factor register is
/// treated as using a scale factor of zero, rather than failing the write.
#[tokio::test]
async fn tolerates_missing_scale_factors() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<i16>>("model703::V_SF", None);
        mock.set_value::<Option<i16>>("model703::HZ_SF", None);
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // 24.50% and 51.00 Hz, which fall back to whole units.
                esv_hi: Some(ScaledValue::new(2450, -2)),
                es_hz_hi: Some(ScaledValue::new(5100, -2)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(mock.get_value::<Option<u16>>("model703::ESV_HI"), Some(25));
    assert_eq!(
        mock.get_value::<Option<u32>>("model703::ES_HZ_HI"),
        Some(51)
    );
}

/// Tests the task reads and emits the device capabilities, status and state.
#[tokio::test]
async fn reads_device_state() {
    // Setup
    let (mock, _task, _input_ch, mut output_ch) = setup(None).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect the default parameters from the mock.
    let all_events = collect_all(&mut output_ch).await;

    let expected_w_max_rtg = mock.get_value::<Option<u16>>("model702::W_MAX_RTG");
    let expected_st = mock.get_value::<Option<model701::St>>("model701::ST");
    let expected_w = mock.get_value::<Option<i16>>("model701::W");

    // Values that carry a scale factor must be reported with the scale factor
    // the device advertises alongside them, not bare.
    let expected_esv_hi = mock
        .get_value::<Option<u16>>("model703::ESV_HI")
        .map(|esv_hi| {
            ScaledValue::new(
                esv_hi,
                mock.get_value::<Option<i16>>("model703::V_SF")
                    .expect("Mock has no V_SF"),
            )
        });
    let expected_soc = mock.get_value::<Option<u16>>("model713::SOC").map(|soc| {
        ScaledValue::new(
            soc,
            mock.get_value::<Option<i16>>("model713::PCT_SF")
                .expect("Mock has no PCT_SF"),
        )
    });
    // Expect received capabilities struct.
    assert!(all_events.iter().any(
        |ev| matches!(ev, modbus_connection::Event::CapabilitiesPolled(
            Capabilities { w_max_rtg, .. }
        )
            if w_max_rtg.map(|w| w.value) == expected_w_max_rtg
        )
    ));

    // Expect received status and settings structs.
    assert!(
        all_events
            .iter()
            .any(|ev| matches!(ev, modbus_connection::Event::StatePolled(
                Some(Status {
                    st, soc, ..
                }),
                Some(Settings {
                    esv_hi, ..
                }),
                Some(Metering {
                    w, ..
                }),
            ) if esv_hi == &expected_esv_hi
                && soc == &expected_soc
                && st == &expected_st
                && w == &expected_w
            ))
    );
}

/// Tests the reconnection of the task when the server goes down.
#[tokio::test]
async fn reconnects() {
    // Setup
    let (mut mock, _task, _input_ch, mut output_ch) = setup(None).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect to have received a capabilities from the mock.
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));

    // Now disconnect the mock server.
    mock.stop().await;

    time::sleep(WAIT_POLL_TIME).await;

    // After wait, change the capaibilities and reconnect mock.
    let expected_w_max_rtg = Some(999u16);
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", expected_w_max_rtg);
    mock.start()
        .await
        .expect("Couldn't start mock modbus server");

    // Wait for the task to reconnect
    time::sleep(WAIT_POLL_TIME).await;

    // And we expect to again receive the capabilities from the mock.
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(
        |ev| matches!(ev, modbus_connection::Event::CapabilitiesPolled(
            Capabilities { w_max_rtg, .. }
        ) if w_max_rtg.map(|w| w.value) == expected_w_max_rtg
        )
    ));
}

/// Tests the reconnection logic when the device only supports model 1, then
/// disconnects and reappears with more models.
#[tokio::test]
async fn reconnects_and_discovers_new_models() {
    // Setup
    let (mut mock, _task, _input_ch, mut output_ch) = setup(Some(&[1])).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect a device connected message and at least one poll with no data
    let all_events = collect_all(&mut output_ch).await;
    assert!(
        all_events
            .iter()
            .any(|ev| matches!(ev, modbus_connection::Event::DeviceConnected(_)))
    );
    assert!(
        all_events
            .iter()
            .any(|ev| matches!(ev, modbus_connection::Event::StatePolled(None, None, None)))
    );

    // Now disconnect the mock server.
    mock.stop().await;

    time::sleep(WAIT_POLL_TIME).await;

    // After wait, change the mock to support capabilities.
    mock.reinit(Some(&[1, 701, 702, 703]));
    mock.start()
        .await
        .expect("Couldn't start mock modbus server");

    // Wait for the task to reconnect
    time::sleep(WAIT_POLL_TIME).await;

    // And we expect to receive some capabilities from the mock.
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));
}

/// Tests whether the task tolerates missing capabilities (model 702)
#[tokio::test]
async fn tolerates_missing_capabilities() {
    // Setup
    let (_mock, task, _input_ch, mut output_ch) = setup(Some(&[1, 701, 703])).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect to have received a status from the mock, but no capabilities.
    let all_events = collect_all(&mut output_ch).await;
    assert!(!all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::StatePolled(Some(_status), Some(_settings), Some(_metering)),
    )));

    // And the task should still be active.
    assert!(!task.is_finished());
}

/// Tests whether the task tolerates missing meter readings (model 701)
#[tokio::test]
async fn tolerates_missing_metering() {
    // Setup
    let (_mock, task, _input_ch, mut output_ch) = setup(Some(&[1, 702, 703])).await;

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect to have received a status from the mock, but no capabilities.
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));
    assert!(!all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::StatePolled(_status, _settings, Some(_metering)),
    )));

    // And the task should still be active.
    assert!(!task.is_finished());
}

/// Tests whether the task tolerates missing control parameter locations
#[tokio::test]
async fn tolerates_missing_control_parameters() {
    // Setup
    let (_mock, task, input_ch, mut output_ch) = setup(Some(&[1, 701, 702])).await;

    // Provide a parameters command
    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                es: Some(model703::Es::Enabled),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    // Wait for first poll of the device
    time::sleep(WAIT_POLL_TIME).await;

    // We expect to have received a polled state but no errors
    let all_events = collect_all(&mut output_ch).await;
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::CapabilitiesPolled(Capabilities { .. })
    )));
    assert!(all_events.iter().any(|ev| matches!(
        ev,
        modbus_connection::Event::StatePolled(_status, _settings, Some(_metering)),
    )));

    // And the task should still be active.
    assert!(!task.is_finished());
}

/////
// Helpers

/// Sets up the modbus server mock with default values, and starts the modbus
/// connection task.
async fn setup(
    enabled_models: Option<&[u32]>,
) -> (
    SunSpecMock,
    JoinHandle<Result<()>>,
    mpsc::Sender<modbus_connection::Command>,
    async_broadcast::Receiver<modbus_connection::Event>,
) {
    setup_with(enabled_models, |_| {}).await
}

/// Like setup, but also runs the callback `seed` against the mock before the
/// server starts, so a test can vary what the device advertises without a race
/// condition.
async fn setup_with(
    enabled_models: Option<&[u32]>,
    seed: impl FnOnce(&SunSpecMock),
) -> (
    SunSpecMock,
    JoinHandle<Result<()>>,
    mpsc::Sender<modbus_connection::Command>,
    async_broadcast::Receiver<modbus_connection::Event>,
) {
    let mut mock = SunSpecMock::new(enabled_models)
        .await
        .expect("Couldn't create mock modbus server");
    seed(&mock);
    mock.start()
        .await
        .expect("Couldn't start mock modbus server");

    let (input_tx, input_rx) = mpsc::channel(10);
    let (output_tx, output_rx) = async_broadcast::broadcast(10);
    let task = task::spawn(modbus_connection::task(
        output_tx,
        input_rx,
        Transport::Tcp(mock.addr.unwrap()),
        1,
    ));

    (mock, task, input_tx, output_rx)
}

async fn collect_all<T: Clone>(output_ch: &mut async_broadcast::Receiver<T>) -> Vec<T> {
    let mut events = Vec::new();
    loop {
        match time::timeout(WAIT_TIME, output_ch.recv()).await {
            Err(_) => break,
            Ok(Err(_)) => panic!("Recv error"),
            Ok(Ok(event)) => events.push(event),
        }
    }
    events
}
