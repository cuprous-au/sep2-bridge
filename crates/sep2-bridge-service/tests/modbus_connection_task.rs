mod modbus_server_mock;

use std::time::Duration;

use modbus_server_mock::SunSpecMock;
use sep2_bridge::{
    Result, ScaledValue,
    modbus_connection::{self, Capabilities, Metering, Model711Ctl, Settings, Status, Transport},
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

#[tokio::test]
async fn enables_derived_w_max_lim_pct() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // With mock W_MAX_RTG of 1000W and W_MAX_LIM_PCT_SF of 0.
                // Also emulate translation's export-only output.
                w_max_lim_pct_ena: Some(model704::WMaxLimPctEna::Disabled),
                w_max_lim: Some(ScaledValue::new(800, 0)),
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
        mock.get_value::<model704::WMaxLimPctEna>("model704::W_MAX_LIM_PCT_ENA"),
        model704::WMaxLimPctEna::Enabled
    );
}

#[tokio::test]
async fn import_site_limit_derived_w_set_pct_uses_wmaxpct_mode() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set: Some(ScaledValue::new(-250, 0)),
                w_set_is_import_site_limit: true,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::WMaxPct
    );
    assert_eq!(mock.get_value::<Option<i32>>("model704::W_SET"), None);
}

#[tokio::test]
async fn import_site_limit_supplied_w_set_pct_also_skips_w_set() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set_pct: Some(ScaledValue::new(-25, 0)),
                w_set: Some(ScaledValue::new(-250, 0)),
                w_set_is_import_site_limit: true,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::WMaxPct
    );
    assert_eq!(mock.get_value::<Option<i32>>("model704::W_SET"), None);
}

#[tokio::test]
async fn explicit_target_w_keeps_watts_mode() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set: Some(ScaledValue::new(-250, 0)),
                w_set_is_import_site_limit: false,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::Watts
    );
    assert!(mock.get_value::<Option<i32>>("model704::W_SET").is_some());
}

#[tokio::test]
async fn import_site_limit_w_set_derivation_defers_without_capacity_and_retries() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
        mock.set_value::<model704::WSetEna>("model704::W_SET_ENA", model704::WSetEna::Disabled);
        mock.set_value::<Option<model704::WSetMod>>("model704::W_SET_MOD", None);
        mock.set_value::<Option<i32>>("model704::W_SET", None);
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // Emulate translation output for import site limit controls.
                w_set_ena: Some(model704::WSetEna::Enabled),
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set: Some(ScaledValue::new(-250, 0)),
                w_set_is_import_site_limit: true,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    // Without capacity, leave mode/enable unchanged and avoid watts fallback.
    assert_eq!(
        mock.get_value::<model704::WSetEna>("model704::W_SET_ENA"),
        model704::WSetEna::Disabled
    );
    assert_eq!(
        mock.get_value::<Option<model704::WSetMod>>("model704::W_SET_MOD"),
        None
    );
    assert_eq!(mock.get_value::<Option<i32>>("model704::W_SET"), None);

    // Once capacity is available, the pending request should retry and apply.
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    time::sleep(WAIT_POLL_TIME).await;

    assert_eq!(
        mock.get_value::<model704::WSetEna>("model704::W_SET_ENA"),
        model704::WSetEna::Enabled
    );
    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::WMaxPct
    );
    assert_eq!(mock.get_value::<Option<i32>>("model704::W_SET"), None);
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-250)
    );
}

#[tokio::test]
async fn explicit_target_w_set_derivation_defers_and_retries_pct_when_capacity_appears() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
        mock.set_value::<Option<i16>>("model704::W_SET_PCT", Some(12));
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_ena: Some(model704::WSetEna::Enabled),
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set: Some(ScaledValue::new(-250, 0)),
                w_set_is_import_site_limit: false,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    // Watts path applies immediately for explicit targets.
    assert_eq!(
        mock.get_value::<model704::WSetEna>("model704::W_SET_ENA"),
        model704::WSetEna::Enabled
    );
    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::Watts
    );
    assert!(mock.get_value::<Option<i32>>("model704::W_SET").is_some());
    // Percentage remains pending while capacity is unavailable.
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(12)
    );

    // Once capacity is available, the same pending request should
    // automatically retry and apply WSetPct.
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    time::sleep(WAIT_POLL_TIME).await;

    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-250)
    );
}

#[tokio::test]
async fn explicit_target_pending_retry_does_not_rewrite_watts_controls() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_ena: Some(model704::WSetEna::Enabled),
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set: Some(ScaledValue::new(-250, 0)),
                w_set_is_import_site_limit: false,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    // Simulate an external change while WSetPct derivation remains pending.
    mock.set_value::<model704::WSetEna>("model704::W_SET_ENA", model704::WSetEna::Disabled);
    mock.set_value::<Option<model704::WSetMod>>(
        "model704::W_SET_MOD",
        Some(model704::WSetMod::WMaxPct),
    );
    mock.set_value::<Option<i32>>("model704::W_SET", Some(1234));

    // Pending retries should only target derivation-dependent writes.
    time::sleep(WAIT_POLL_TIME).await;

    assert_eq!(
        mock.get_value::<model704::WSetEna>("model704::W_SET_ENA"),
        model704::WSetEna::Disabled
    );
    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::WMaxPct
    );
    assert_eq!(mock.get_value::<Option<i32>>("model704::W_SET"), Some(1234));
}

#[tokio::test]
async fn explicit_target_pending_retry_does_not_rewrite_export_controls() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_ena: Some(model704::WSetEna::Enabled),
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set: Some(ScaledValue::new(-250, 0)),
                // Explicit export controls are present but not pending derivation.
                w_max_lim_pct_ena: Some(model704::WMaxLimPctEna::Enabled),
                w_max_lim_pct: Some(ScaledValue::new(8000, -2)),
                w_set_is_import_site_limit: false,
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    // Simulate external change while only WSetPct derivation remains pending.
    mock.set_value::<model704::WMaxLimPctEna>(
        "model704::W_MAX_LIM_PCT_ENA",
        model704::WMaxLimPctEna::Disabled,
    );
    mock.set_value::<Option<u16>>("model704::W_MAX_LIM_PCT", Some(42));

    time::sleep(WAIT_POLL_TIME).await;

    // Retry loop should not rewrite non-pending export controls.
    assert_eq!(
        mock.get_value::<model704::WMaxLimPctEna>("model704::W_MAX_LIM_PCT_ENA"),
        model704::WMaxLimPctEna::Disabled
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(42)
    );
}

#[tokio::test]
async fn export_pending_retry_does_not_rewrite_supplied_w_set_pct_controls() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_set_ena: Some(model704::WSetEna::Enabled),
                // import-site-limit requests force WMaxPct mode when pct is present
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set_pct: Some(ScaledValue::new(-250, 0)),
                w_set_is_import_site_limit: true,
                // export-site-limit derivation remains pending without capacity
                w_max_lim_pct_ena: Some(model704::WMaxLimPctEna::Disabled),
                w_max_lim: Some(ScaledValue::new(800, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    // Simulate external changes while only export derivation is pending.
    mock.set_value::<model704::WSetEna>("model704::W_SET_ENA", model704::WSetEna::Disabled);
    mock.set_value::<model704::WSetMod>("model704::W_SET_MOD", model704::WSetMod::Watts);
    mock.set_value::<Option<i16>>("model704::W_SET_PCT", Some(12));

    time::sleep(WAIT_POLL_TIME).await;

    // Retry loop should not rewrite W-set controls when WSetPct derivation is not pending.
    assert_eq!(
        mock.get_value::<model704::WSetEna>("model704::W_SET_ENA"),
        model704::WSetEna::Disabled
    );
    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::Watts
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(12)
    );
}

#[tokio::test]
async fn deferred_update_does_not_block_reapply_of_prior_parameters() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
    })
    .await;

    let params_a = modbus_connection::Parameters {
        es: Some(model703::Es::Disabled),
        ..Default::default()
    };
    let params_b = modbus_connection::Parameters {
        es: Some(model703::Es::Enabled),
        // Emulate translation output for export-only extension.
        w_max_lim_pct_ena: Some(model704::WMaxLimPctEna::Disabled),
        w_max_lim: Some(ScaledValue::new(800, 0)),
        ..Default::default()
    };

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            params_a.clone(),
        ))
        .await
        .expect("Send error");
    time::sleep(WAIT_TIME).await;
    assert_eq!(
        mock.get_value::<model703::Es>("model703::ES"),
        model703::Es::Disabled
    );

    input_ch
        .send(modbus_connection::Command::UpdateParameters(params_b))
        .await
        .expect("Send error");
    time::sleep(WAIT_TIME).await;
    assert_eq!(
        mock.get_value::<model703::Es>("model703::ES"),
        model703::Es::Enabled
    );

    // Re-sending A must not be skipped, even though it previously matched the
    // last fully-applied parameters before the deferred update.
    input_ch
        .send(modbus_connection::Command::UpdateParameters(params_a))
        .await
        .expect("Send error");
    time::sleep(WAIT_TIME).await;
    assert_eq!(
        mock.get_value::<model703::Es>("model703::ES"),
        model703::Es::Disabled
    );
}

#[tokio::test]
async fn export_limit_derivation_defers_without_capacity_and_retries() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
        mock.set_value::<Option<u16>>("model704::W_MAX_LIM_PCT", Some(42));
        mock.set_value::<model704::WMaxLimPctEna>(
            "model704::W_MAX_LIM_PCT_ENA",
            model704::WMaxLimPctEna::Enabled,
        );
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // Emulate translation output for export-only extension.
                w_max_lim_pct_ena: Some(model704::WMaxLimPctEna::Disabled),
                w_max_lim: Some(ScaledValue::new(800, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    // Without capacity, the write should be deferred and existing protection
    // left unchanged.
    assert_eq!(
        mock.get_value::<model704::WMaxLimPctEna>("model704::W_MAX_LIM_PCT_ENA"),
        model704::WMaxLimPctEna::Enabled
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(42)
    );

    // Once capacity is available, the same pending parameters should
    // automatically retry and apply.
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    time::sleep(WAIT_POLL_TIME).await;

    assert_eq!(
        mock.get_value::<model704::WMaxLimPctEna>("model704::W_MAX_LIM_PCT_ENA"),
        model704::WMaxLimPctEna::Enabled
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
}

#[tokio::test]
async fn export_limit_uses_more_restrictive_of_explicit_and_site_limits() {
    let (mock, _task, input_ch, _output_ch) = setup(None).await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_max_lim_pct_ena: Some(model704::WMaxLimPctEna::Enabled),
                // 80% explicit.
                w_max_lim_pct: Some(ScaledValue::new(8000, -2)),
                // 100W on 1000W rating => 10% derived, which is more restrictive.
                w_max_lim: Some(ScaledValue::new(100, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    assert_eq!(
        mock.get_value::<model704::WMaxLimPctEna>("model704::W_MAX_LIM_PCT_ENA"),
        model704::WMaxLimPctEna::Enabled
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(10)
    );
}

#[cfg(feature = "map-imp-exp-site-limits")]
#[tokio::test]
async fn import_export_site_limits_derive_independently_and_recover_with_capacity() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
        // Seed prior control state to verify deferred retries preserve it.
        mock.set_value::<model704::WSetEna>("model704::W_SET_ENA", model704::WSetEna::Disabled);
        mock.set_value::<model704::WSetMod>("model704::W_SET_MOD", model704::WSetMod::Watts);
        mock.set_value::<Option<i16>>("model704::W_SET_PCT", Some(12));
        mock.set_value::<Option<i32>>("model704::W_SET", Some(777));
        mock.set_value::<model704::WMaxLimPctEna>(
            "model704::W_MAX_LIM_PCT_ENA",
            model704::WMaxLimPctEna::Enabled,
        );
        mock.set_value::<Option<u16>>("model704::W_MAX_LIM_PCT", Some(42));
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                // Emulate feature-enabled translation output carrying both
                // import and export site limits in one request.
                w_set_ena: Some(model704::WSetEna::Enabled),
                w_set_mod: Some(model704::WSetMod::Watts),
                w_set: Some(ScaledValue::new(-250, 0)),
                w_set_is_import_site_limit: true,
                w_max_lim_pct_ena: Some(model704::WMaxLimPctEna::Disabled),
                w_max_lim: Some(ScaledValue::new(800, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    // Without capacity, both derivations defer and existing controls remain unchanged.
    assert_eq!(
        mock.get_value::<model704::WSetEna>("model704::W_SET_ENA"),
        model704::WSetEna::Disabled
    );
    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::Watts
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(12)
    );
    assert_eq!(mock.get_value::<Option<i32>>("model704::W_SET"), Some(777));
    assert_eq!(
        mock.get_value::<model704::WMaxLimPctEna>("model704::W_MAX_LIM_PCT_ENA"),
        model704::WMaxLimPctEna::Enabled
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(42)
    );

    // Once capacity appears, both pending percentages should be applied.
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    time::sleep(WAIT_POLL_TIME).await;

    assert_eq!(
        mock.get_value::<model704::WSetEna>("model704::W_SET_ENA"),
        model704::WSetEna::Enabled
    );
    assert_eq!(
        mock.get_value::<model704::WSetMod>("model704::W_SET_MOD"),
        model704::WSetMod::WMaxPct
    );
    assert_eq!(
        mock.get_value::<Option<i16>>("model704::W_SET_PCT"),
        Some(-250)
    );
    // Import-site-limit percentage mode should not rewrite optional watts register.
    assert_eq!(mock.get_value::<Option<i32>>("model704::W_SET"), Some(777));
    assert_eq!(
        mock.get_value::<model704::WMaxLimPctEna>("model704::W_MAX_LIM_PCT_ENA"),
        model704::WMaxLimPctEna::Enabled
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(80)
    );
}

#[tokio::test]
async fn export_limit_with_explicit_and_site_defers_when_capacity_unavailable() {
    let (mock, _task, input_ch, _output_ch) = setup_with(None, |mock| {
        mock.set_value::<Option<u16>>("model702::W_MAX_RTG", None);
        mock.set_value::<Option<u16>>("model704::W_MAX_LIM_PCT", Some(42));
        mock.set_value::<model704::WMaxLimPctEna>(
            "model704::W_MAX_LIM_PCT_ENA",
            model704::WMaxLimPctEna::Enabled,
        );
    })
    .await;

    input_ch
        .send(modbus_connection::Command::UpdateParameters(
            modbus_connection::Parameters {
                w_max_lim_pct_ena: Some(model704::WMaxLimPctEna::Enabled),
                // Explicit 80%, but site limit should still be considered once capacity appears.
                w_max_lim_pct: Some(ScaledValue::new(8000, -2)),
                w_max_lim: Some(ScaledValue::new(100, 0)),
                ..Default::default()
            },
        ))
        .await
        .expect("Send error");

    time::sleep(WAIT_TIME).await;

    // Leave existing protection unchanged while site-derived percentage cannot be computed.
    assert_eq!(
        mock.get_value::<model704::WMaxLimPctEna>("model704::W_MAX_LIM_PCT_ENA"),
        model704::WMaxLimPctEna::Enabled
    );
    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(42)
    );

    // Once capacity appears, apply the tighter site-derived limit.
    mock.set_value::<Option<u16>>("model702::W_MAX_RTG", Some(1000));
    time::sleep(WAIT_POLL_TIME).await;

    assert_eq!(
        mock.get_value::<Option<u16>>("model704::W_MAX_LIM_PCT"),
        Some(10)
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
