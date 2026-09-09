use std::sync::Arc;

use sep2_common::packages::der::{DERControlBase, DefaultDERControl};

use super::Event;

/// The output of the scheduler, describing the set of controls to be applied to
/// a device at a given instant in time.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ControlAttributes {
    // The DefaultDERControl struct encompasses all possible 38 parameters (29
    // in DERControlBase, 9 in the DefaultDERControl struct itself).
    pub inner: DefaultDERControl,
}

impl ControlAttributes {
    pub fn new(controls: DefaultDERControl) -> ControlAttributes {
        ControlAttributes { inner: controls }
    }

    /// Total distinct attributes that would be applied.
    pub fn num_active(&self) -> u32 {
        self.inner.der_control_base.op_mod_connect.is_some() as u32
            + self.inner.der_control_base.op_mod_energize.is_some() as u32
            + self
                .inner
                .der_control_base
                .op_mod_fixed_pf_absorb_w
                .is_some() as u32
            + self
                .inner
                .der_control_base
                .op_mod_fixed_pf_inject_w
                .is_some() as u32
            + self.inner.der_control_base.op_mod_fixed_var.is_some() as u32
            + self.inner.der_control_base.op_mod_fixed_w.is_some() as u32
            + self.inner.der_control_base.op_mod_freq_droop.is_some() as u32
            + self.inner.der_control_base.op_mod_freq_watt.is_some() as u32
            + self.inner.der_control_base.op_mod_hfrt_may_trip.is_some() as u32
            + self.inner.der_control_base.op_mod_hfrt_must_trip.is_some() as u32
            + self.inner.der_control_base.op_mod_hvrt_may_trip.is_some() as u32
            + self
                .inner
                .der_control_base
                .op_mod_hvrt_momentary_cessation
                .is_some() as u32
            + self.inner.der_control_base.op_mod_hvrt_must_trip.is_some() as u32
            + self.inner.der_control_base.op_mod_lfrt_may_trip.is_some() as u32
            + self.inner.der_control_base.op_mod_lfrt_must_trip.is_some() as u32
            + self.inner.der_control_base.op_mod_lvrt_may_trip.is_some() as u32
            + self
                .inner
                .der_control_base
                .op_mod_lvrt_momentary_cessation
                .is_some() as u32
            + self.inner.der_control_base.op_mod_lvrt_must_trip.is_some() as u32
            + self.inner.der_control_base.op_mod_max_lim_w.is_some() as u32
            + self.inner.der_control_base.op_mod_target_var.is_some() as u32
            + self.inner.der_control_base.op_mod_target_w.is_some() as u32
            + self.inner.der_control_base.op_mod_volt_var.is_some() as u32
            + self.inner.der_control_base.op_mod_volt_watt.is_some() as u32
            + self.inner.der_control_base.op_mod_watt_pf.is_some() as u32
            + self.inner.der_control_base.op_mod_watt_var.is_some() as u32
            + self.inner.der_control_base.ramp_tms.is_some() as u32
            + self.inner.der_control_base.op_mod_imp_lim_w.is_some() as u32
            + self.inner.der_control_base.op_mod_exp_lim_w.is_some() as u32
            + self.inner.der_control_base.op_mod_gen_lim_w.is_some() as u32
            + self.inner.der_control_base.op_mod_load_lim_w.is_some() as u32
            + self.inner.set_es_delay.is_some() as u32
            + self.inner.set_es_high_freq.is_some() as u32
            + self.inner.set_es_high_volt.is_some() as u32
            + self.inner.set_es_low_freq.is_some() as u32
            + self.inner.set_es_low_volt.is_some() as u32
            + self.inner.set_es_ramp_tms.is_some() as u32
            + self.inner.set_es_random_delay.is_some() as u32
            + self.inner.set_grad_w.is_some() as u32
            + self.inner.set_soft_grad_w.is_some() as u32
    }
}

impl From<ControlAttributes> for Event {
    fn from(value: ControlAttributes) -> Self {
        Event::ParametersChanged(Arc::new(value))
    }
}

/// Combine a set of attributes (first) with another set (overlay). When an
/// attribute is defined in both sets, prefer the attribute from the `first`
/// argument.
pub fn overlay_controls(first: DefaultDERControl, overlay: DefaultDERControl) -> DefaultDERControl {
    DefaultDERControl {
        der_control_base: DERControlBase {
            op_mod_connect: first
                .der_control_base
                .op_mod_connect
                .or(overlay.der_control_base.op_mod_connect),
            op_mod_energize: first
                .der_control_base
                .op_mod_energize
                .or(overlay.der_control_base.op_mod_energize),
            op_mod_fixed_pf_absorb_w: first
                .der_control_base
                .op_mod_fixed_pf_absorb_w
                .or(overlay.der_control_base.op_mod_fixed_pf_absorb_w),
            op_mod_fixed_pf_inject_w: first
                .der_control_base
                .op_mod_fixed_pf_inject_w
                .or(overlay.der_control_base.op_mod_fixed_pf_inject_w),
            op_mod_fixed_var: first
                .der_control_base
                .op_mod_fixed_var
                .or(overlay.der_control_base.op_mod_fixed_var),
            op_mod_fixed_w: first
                .der_control_base
                .op_mod_fixed_w
                .or(overlay.der_control_base.op_mod_fixed_w),
            op_mod_freq_droop: first
                .der_control_base
                .op_mod_freq_droop
                .or(overlay.der_control_base.op_mod_freq_droop),
            op_mod_freq_watt: first
                .der_control_base
                .op_mod_freq_watt
                .or(overlay.der_control_base.op_mod_freq_watt),
            op_mod_hfrt_may_trip: first
                .der_control_base
                .op_mod_hfrt_may_trip
                .or(overlay.der_control_base.op_mod_hfrt_may_trip),
            op_mod_hfrt_must_trip: first
                .der_control_base
                .op_mod_hfrt_must_trip
                .or(overlay.der_control_base.op_mod_hfrt_must_trip),
            op_mod_hvrt_may_trip: first
                .der_control_base
                .op_mod_hvrt_may_trip
                .or(overlay.der_control_base.op_mod_hvrt_may_trip),
            op_mod_hvrt_momentary_cessation: first
                .der_control_base
                .op_mod_hvrt_momentary_cessation
                .or(overlay.der_control_base.op_mod_hvrt_momentary_cessation),
            op_mod_hvrt_must_trip: first
                .der_control_base
                .op_mod_hvrt_must_trip
                .or(overlay.der_control_base.op_mod_hvrt_must_trip),
            op_mod_lfrt_may_trip: first
                .der_control_base
                .op_mod_lfrt_may_trip
                .or(overlay.der_control_base.op_mod_lfrt_may_trip),
            op_mod_lfrt_must_trip: first
                .der_control_base
                .op_mod_lfrt_must_trip
                .or(overlay.der_control_base.op_mod_lfrt_must_trip),
            op_mod_lvrt_may_trip: first
                .der_control_base
                .op_mod_lvrt_may_trip
                .or(overlay.der_control_base.op_mod_lvrt_may_trip),
            op_mod_lvrt_momentary_cessation: first
                .der_control_base
                .op_mod_lvrt_momentary_cessation
                .or(overlay.der_control_base.op_mod_lvrt_momentary_cessation),
            op_mod_lvrt_must_trip: first
                .der_control_base
                .op_mod_lvrt_must_trip
                .or(overlay.der_control_base.op_mod_lvrt_must_trip),
            op_mod_max_lim_w: first
                .der_control_base
                .op_mod_max_lim_w
                .or(overlay.der_control_base.op_mod_max_lim_w),
            op_mod_target_var: first
                .der_control_base
                .op_mod_target_var
                .or(overlay.der_control_base.op_mod_target_var),
            op_mod_target_w: first
                .der_control_base
                .op_mod_target_w
                .or(overlay.der_control_base.op_mod_target_w),
            op_mod_volt_var: first
                .der_control_base
                .op_mod_volt_var
                .or(overlay.der_control_base.op_mod_volt_var),
            op_mod_volt_watt: first
                .der_control_base
                .op_mod_volt_watt
                .or(overlay.der_control_base.op_mod_volt_watt),
            op_mod_watt_pf: first
                .der_control_base
                .op_mod_watt_pf
                .or(overlay.der_control_base.op_mod_watt_pf),
            op_mod_watt_var: first
                .der_control_base
                .op_mod_watt_var
                .or(overlay.der_control_base.op_mod_watt_var),
            ramp_tms: first
                .der_control_base
                .ramp_tms
                .or(overlay.der_control_base.ramp_tms),
            op_mod_imp_lim_w: first
                .der_control_base
                .op_mod_imp_lim_w
                .or(overlay.der_control_base.op_mod_imp_lim_w),
            op_mod_exp_lim_w: first
                .der_control_base
                .op_mod_exp_lim_w
                .or(overlay.der_control_base.op_mod_exp_lim_w),
            op_mod_gen_lim_w: first
                .der_control_base
                .op_mod_gen_lim_w
                .or(overlay.der_control_base.op_mod_gen_lim_w),
            op_mod_load_lim_w: first
                .der_control_base
                .op_mod_load_lim_w
                .or(overlay.der_control_base.op_mod_load_lim_w),
        },
        set_es_delay: first.set_es_delay.or(overlay.set_es_delay),
        set_es_high_freq: first.set_es_high_freq.or(overlay.set_es_high_freq),
        set_es_high_volt: first.set_es_high_volt.or(overlay.set_es_high_volt),
        set_es_low_freq: first.set_es_low_freq.or(overlay.set_es_low_freq),
        set_es_low_volt: first.set_es_low_volt.or(overlay.set_es_low_volt),
        set_es_ramp_tms: first.set_es_ramp_tms.or(overlay.set_es_ramp_tms),
        set_es_random_delay: first.set_es_random_delay.or(overlay.set_es_random_delay),
        set_grad_w: first.set_grad_w.or(overlay.set_grad_w),
        set_soft_grad_w: first.set_soft_grad_w.or(overlay.set_soft_grad_w),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use sep2_common::packages::primitives::{Int16, Uint16, Uint32};

    // Helper strategy to generate random DefaultDERControls
    fn arb_default_der_control() -> impl Strategy<Value = DefaultDERControl> {
        (
            prop::collection::vec(any::<bool>(), 30), // 30 flags for base fields
            prop::collection::vec(any::<bool>(), 9),  // 9 flags for default fields
            any::<u32>(),                             // set_es_delay
            any::<u16>(),                             // set_es_high_freq
            any::<i16>(),                             // set_es_high_volt
            any::<u16>(),                             // set_es_low_freq
            any::<i16>(),                             // set_es_low_volt
            any::<u32>(),                             // set_es_ramp_tms
            any::<u32>(),                             // set_es_random_delay
            any::<u16>(),                             // set_grad_w
            any::<u16>(),                             // set_soft_grad_w
        )
            .prop_map(
                |(
                    base_flags,
                    ca_flags,
                    es_delay,
                    es_high_freq,
                    es_high_volt,
                    es_low_freq,
                    es_low_volt,
                    es_ramp_tms,
                    es_random_delay,
                    grad_w,
                    soft_grad_w,
                )| {
                    let mut controls = DefaultDERControl::default();
                    if base_flags[0] {
                        controls.der_control_base.op_mod_connect = Some(Default::default());
                    }
                    if base_flags[1] {
                        controls.der_control_base.op_mod_energize = Some(Default::default());
                    }
                    if base_flags[2] {
                        controls.der_control_base.op_mod_fixed_pf_absorb_w =
                            Some(Default::default());
                    }
                    if base_flags[3] {
                        controls.der_control_base.op_mod_fixed_pf_inject_w =
                            Some(Default::default());
                    }
                    if base_flags[4] {
                        controls.der_control_base.op_mod_fixed_var = Some(Default::default());
                    }
                    if base_flags[5] {
                        controls.der_control_base.op_mod_fixed_w = Some(Default::default());
                    }
                    if base_flags[6] {
                        controls.der_control_base.op_mod_freq_droop = Some(Default::default());
                    }
                    if base_flags[7] {
                        controls.der_control_base.op_mod_freq_watt = Some(Default::default());
                    }
                    if base_flags[8] {
                        controls.der_control_base.op_mod_hfrt_may_trip = Some(Default::default());
                    }
                    if base_flags[9] {
                        controls.der_control_base.op_mod_hfrt_must_trip = Some(Default::default());
                    }
                    if base_flags[10] {
                        controls.der_control_base.op_mod_hvrt_may_trip = Some(Default::default());
                    }
                    if base_flags[11] {
                        controls.der_control_base.op_mod_hvrt_momentary_cessation =
                            Some(Default::default());
                    }
                    if base_flags[12] {
                        controls.der_control_base.op_mod_hvrt_must_trip = Some(Default::default());
                    }
                    if base_flags[13] {
                        controls.der_control_base.op_mod_lfrt_may_trip = Some(Default::default());
                    }
                    if base_flags[14] {
                        controls.der_control_base.op_mod_lfrt_must_trip = Some(Default::default());
                    }
                    if base_flags[15] {
                        controls.der_control_base.op_mod_lvrt_may_trip = Some(Default::default());
                    }
                    if base_flags[16] {
                        controls.der_control_base.op_mod_lvrt_momentary_cessation =
                            Some(Default::default());
                    }
                    if base_flags[17] {
                        controls.der_control_base.op_mod_lvrt_must_trip = Some(Default::default());
                    }
                    if base_flags[18] {
                        controls.der_control_base.op_mod_max_lim_w = Some(Default::default());
                    }
                    if base_flags[19] {
                        controls.der_control_base.op_mod_target_var = Some(Default::default());
                    }
                    if base_flags[20] {
                        controls.der_control_base.op_mod_target_w = Some(Default::default());
                    }
                    if base_flags[21] {
                        controls.der_control_base.op_mod_volt_var = Some(Default::default());
                    }
                    if base_flags[22] {
                        controls.der_control_base.op_mod_volt_watt = Some(Default::default());
                    }
                    if base_flags[23] {
                        controls.der_control_base.op_mod_watt_pf = Some(Default::default());
                    }
                    if base_flags[24] {
                        controls.der_control_base.op_mod_watt_var = Some(Default::default());
                    }
                    if base_flags[25] {
                        controls.der_control_base.ramp_tms = Some(Default::default());
                    }
                    if base_flags[26] {
                        controls.der_control_base.op_mod_imp_lim_w = Some(Default::default());
                    }
                    if base_flags[27] {
                        controls.der_control_base.op_mod_exp_lim_w = Some(Default::default());
                    }
                    if base_flags[28] {
                        controls.der_control_base.op_mod_gen_lim_w = Some(Default::default());
                    }
                    if base_flags[29] {
                        controls.der_control_base.op_mod_load_lim_w = Some(Default::default());
                    }

                    if ca_flags[0] {
                        controls.set_es_delay = Some(Uint32(es_delay));
                    }
                    if ca_flags[1] {
                        controls.set_es_high_freq = Some(Uint16(es_high_freq));
                    }
                    if ca_flags[2] {
                        controls.set_es_high_volt = Some(Int16(es_high_volt));
                    }
                    if ca_flags[3] {
                        controls.set_es_low_freq = Some(Uint16(es_low_freq));
                    }
                    if ca_flags[4] {
                        controls.set_es_low_volt = Some(Int16(es_low_volt));
                    }
                    if ca_flags[5] {
                        controls.set_es_ramp_tms = Some(Uint32(es_ramp_tms));
                    }
                    if ca_flags[6] {
                        controls.set_es_random_delay = Some(Uint32(es_random_delay));
                    }
                    if ca_flags[7] {
                        controls.set_grad_w = Some(Uint16(grad_w));
                    }
                    if ca_flags[8] {
                        controls.set_soft_grad_w = Some(Uint16(soft_grad_w));
                    }

                    controls
                },
            )
    }

    proptest! {
        #[test]
        fn test_overlay_identity(controls in arb_default_der_control()) {
            prop_assert_eq!(overlay_controls(controls.clone(), DefaultDERControl::default()), controls.clone());
            prop_assert_eq!(overlay_controls(DefaultDERControl::default(), controls.clone()), controls);
        }

        #[test]
        fn test_overlay_associative(controls1 in arb_default_der_control(), controls2 in arb_default_der_control(), controls3 in arb_default_der_control()) {
            let left = overlay_controls(overlay_controls(controls1.clone(), controls2.clone()), controls3.clone());
            let right = overlay_controls(controls1.clone(), overlay_controls(controls2.clone(), controls3.clone()));
            prop_assert_eq!(left, right);
        }

        #[test]
        fn test_overlay_idempotence(controls in arb_default_der_control()) {
            prop_assert_eq!(overlay_controls(controls.clone(), controls.clone()), controls);
        }

        #[test]
        fn test_overlay_precedence(controls2 in arb_default_der_control()) {
            // Construct a ControlAttributes where all fields are Some
            let controls1 = DefaultDERControl {
                der_control_base: DERControlBase {
                    op_mod_connect: Some(Default::default()),
                    op_mod_energize: Some(Default::default()),
                    op_mod_fixed_pf_absorb_w: Some(Default::default()),
                    op_mod_fixed_pf_inject_w: Some(Default::default()),
                    op_mod_fixed_var: Some(Default::default()),
                    op_mod_fixed_w: Some(Default::default()),
                    op_mod_freq_droop: Some(Default::default()),
                    op_mod_freq_watt: Some(Default::default()),
                    op_mod_hfrt_may_trip: Some(Default::default()),
                    op_mod_hfrt_must_trip: Some(Default::default()),
                    op_mod_hvrt_may_trip: Some(Default::default()),
                    op_mod_hvrt_momentary_cessation: Some(Default::default()),
                    op_mod_hvrt_must_trip: Some(Default::default()),
                    op_mod_lfrt_may_trip: Some(Default::default()),
                    op_mod_lfrt_must_trip: Some(Default::default()),
                    op_mod_lvrt_may_trip: Some(Default::default()),
                    op_mod_lvrt_momentary_cessation: Some(Default::default()),
                    op_mod_lvrt_must_trip: Some(Default::default()),
                    op_mod_max_lim_w: Some(Default::default()),
                    op_mod_target_var: Some(Default::default()),
                    op_mod_target_w: Some(Default::default()),
                    op_mod_volt_var: Some(Default::default()),
                    op_mod_volt_watt: Some(Default::default()),
                    op_mod_watt_pf: Some(Default::default()),
                    op_mod_watt_var: Some(Default::default()),
                    ramp_tms: Some(Default::default()),
                    op_mod_imp_lim_w: Some(Default::default()),
                    op_mod_exp_lim_w: Some(Default::default()),
                    op_mod_gen_lim_w: Some(Default::default()),
                    op_mod_load_lim_w: Some(Default::default()),
                },

                set_es_delay: Some(Uint32(1)),
                set_es_high_freq: Some(Uint16(2)),
                set_es_high_volt: Some(Int16(3)),
                set_es_low_freq: Some(Uint16(4)),
                set_es_low_volt: Some(Int16(5)),
                set_es_ramp_tms: Some(Uint32(6)),
                set_es_random_delay: Some(Uint32(7)),
                set_grad_w: Some(Uint16(8)),
                set_soft_grad_w: Some(Uint16(9)),

                ..Default::default()
            };

            prop_assert_eq!(overlay_controls(controls1.clone(), controls2.clone()), controls1);
        }

        #[test]
        fn test_active_attribute_count_consistency(controls1 in arb_default_der_control(), controls2 in arb_default_der_control()) {
            let merged = overlay_controls(controls1.clone(), controls2.clone());
            // Convert to a ControlAttributes struct to easily calculate the number of active controls.
            let ca1 = ControlAttributes::new(controls1);
            let ca2 = ControlAttributes::new(controls2);
            let ca_merged = ControlAttributes::new(merged);
            let n1 = ca1.num_active();
            let n2 = ca2.num_active();
            let n_merged = ca_merged.num_active();

            prop_assert!(n_merged >= n1);
            prop_assert!(n_merged >= n2);
            prop_assert!(n_merged <= n1 + n2);
        }
    }
}
