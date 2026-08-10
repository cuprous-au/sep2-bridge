use std::sync::Arc;

use sep2_common::packages::{
    der::{DERControl, DERControlBase, DefaultDERControl},
    primitives::{Int16, Uint16, Uint32},
};

use super::Event;

/// Lists all of the attributes that can be set by DERControl or DefaultDERControl
/// objects in a way that allows us to easily project higher primacy controls on
/// top of one another.
///
/// This is a subset of the parameters available on a DefaultDERControl struct.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ControlAttributes {
    pub base: DERControlBase,

    pub set_es_delay: Option<Uint32>,
    pub set_es_high_freq: Option<Uint16>,
    pub set_es_high_volt: Option<Int16>,
    pub set_es_low_freq: Option<Uint16>,
    pub set_es_low_volt: Option<Int16>,
    pub set_es_ramp_tms: Option<Uint32>,
    pub set_es_random_delay: Option<Uint32>,
    pub set_grad_w: Option<Uint16>,
    pub set_soft_grad_w: Option<Uint16>,
}

impl ControlAttributes {
    /// Total distinct attributes that would be applied.
    pub fn num_active(&self) -> u32 {
        self.base.op_mod_connect.is_some() as u32
            + self.base.op_mod_energize.is_some() as u32
            + self.base.op_mod_fixed_pf_absorb_w.is_some() as u32
            + self.base.op_mod_fixed_pf_inject_w.is_some() as u32
            + self.base.op_mod_fixed_var.is_some() as u32
            + self.base.op_mod_fixed_w.is_some() as u32
            + self.base.op_mod_freq_droop.is_some() as u32
            + self.base.op_mod_freq_watt.is_some() as u32
            + self.base.op_mod_hfrt_may_trip.is_some() as u32
            + self.base.op_mod_hfrt_must_trip.is_some() as u32
            + self.base.op_mod_hvrt_may_trip.is_some() as u32
            + self.base.op_mod_hvrt_momentary_cessation.is_some() as u32
            + self.base.op_mod_hvrt_must_trip.is_some() as u32
            + self.base.op_mod_lfrt_may_trip.is_some() as u32
            + self.base.op_mod_lfrt_must_trip.is_some() as u32
            + self.base.op_mod_lvrt_may_trip.is_some() as u32
            + self.base.op_mod_lvrt_momentary_cessation.is_some() as u32
            + self.base.op_mod_lvrt_must_trip.is_some() as u32
            + self.base.op_mod_max_lim_w.is_some() as u32
            + self.base.op_mod_target_var.is_some() as u32
            + self.base.op_mod_target_w.is_some() as u32
            + self.base.op_mod_volt_var.is_some() as u32
            + self.base.op_mod_volt_watt.is_some() as u32
            + self.base.op_mod_watt_pf.is_some() as u32
            + self.base.op_mod_watt_var.is_some() as u32
            + self.base.ramp_tms.is_some() as u32
            + self.base.op_mod_imp_lim_w.is_some() as u32
            + self.base.op_mod_exp_lim_w.is_some() as u32
            + self.base.op_mod_gen_lim_w.is_some() as u32
            + self.base.op_mod_load_lim_w.is_some() as u32
            + self.set_es_delay.is_some() as u32
            + self.set_es_high_freq.is_some() as u32
            + self.set_es_high_volt.is_some() as u32
            + self.set_es_low_freq.is_some() as u32
            + self.set_es_low_volt.is_some() as u32
            + self.set_es_ramp_tms.is_some() as u32
            + self.set_es_random_delay.is_some() as u32
            + self.set_grad_w.is_some() as u32
            + self.set_soft_grad_w.is_some() as u32
    }

    /// Combine this set of attributes with another. The attributes of this object are preferred.
    pub fn overlay_on(self, fallback: ControlAttributes) -> ControlAttributes {
        ControlAttributes {
            base: DERControlBase {
                op_mod_connect: self.base.op_mod_connect.or(fallback.base.op_mod_connect),
                op_mod_energize: self.base.op_mod_energize.or(fallback.base.op_mod_energize),
                op_mod_fixed_pf_absorb_w: self
                    .base
                    .op_mod_fixed_pf_absorb_w
                    .or(fallback.base.op_mod_fixed_pf_absorb_w),
                op_mod_fixed_pf_inject_w: self
                    .base
                    .op_mod_fixed_pf_inject_w
                    .or(fallback.base.op_mod_fixed_pf_inject_w),
                op_mod_fixed_var: self
                    .base
                    .op_mod_fixed_var
                    .or(fallback.base.op_mod_fixed_var),
                op_mod_fixed_w: self.base.op_mod_fixed_w.or(fallback.base.op_mod_fixed_w),
                op_mod_freq_droop: self
                    .base
                    .op_mod_freq_droop
                    .or(fallback.base.op_mod_freq_droop),
                op_mod_freq_watt: self
                    .base
                    .op_mod_freq_watt
                    .or(fallback.base.op_mod_freq_watt),
                op_mod_hfrt_may_trip: self
                    .base
                    .op_mod_hfrt_may_trip
                    .or(fallback.base.op_mod_hfrt_may_trip),
                op_mod_hfrt_must_trip: self
                    .base
                    .op_mod_hfrt_must_trip
                    .or(fallback.base.op_mod_hfrt_must_trip),
                op_mod_hvrt_may_trip: self
                    .base
                    .op_mod_hvrt_may_trip
                    .or(fallback.base.op_mod_hvrt_may_trip),
                op_mod_hvrt_momentary_cessation: self
                    .base
                    .op_mod_hvrt_momentary_cessation
                    .or(fallback.base.op_mod_hvrt_momentary_cessation),
                op_mod_hvrt_must_trip: self
                    .base
                    .op_mod_hvrt_must_trip
                    .or(fallback.base.op_mod_hvrt_must_trip),
                op_mod_lfrt_may_trip: self
                    .base
                    .op_mod_lfrt_may_trip
                    .or(fallback.base.op_mod_lfrt_may_trip),
                op_mod_lfrt_must_trip: self
                    .base
                    .op_mod_lfrt_must_trip
                    .or(fallback.base.op_mod_lfrt_must_trip),
                op_mod_lvrt_may_trip: self
                    .base
                    .op_mod_lvrt_may_trip
                    .or(fallback.base.op_mod_lvrt_may_trip),
                op_mod_lvrt_momentary_cessation: self
                    .base
                    .op_mod_lvrt_momentary_cessation
                    .or(fallback.base.op_mod_lvrt_momentary_cessation),
                op_mod_lvrt_must_trip: self
                    .base
                    .op_mod_lvrt_must_trip
                    .or(fallback.base.op_mod_lvrt_must_trip),
                op_mod_max_lim_w: self
                    .base
                    .op_mod_max_lim_w
                    .or(fallback.base.op_mod_max_lim_w),
                op_mod_target_var: self
                    .base
                    .op_mod_target_var
                    .or(fallback.base.op_mod_target_var),
                op_mod_target_w: self.base.op_mod_target_w.or(fallback.base.op_mod_target_w),
                op_mod_volt_var: self.base.op_mod_volt_var.or(fallback.base.op_mod_volt_var),
                op_mod_volt_watt: self
                    .base
                    .op_mod_volt_watt
                    .or(fallback.base.op_mod_volt_watt),
                op_mod_watt_pf: self.base.op_mod_watt_pf.or(fallback.base.op_mod_watt_pf),
                op_mod_watt_var: self.base.op_mod_watt_var.or(fallback.base.op_mod_watt_var),
                ramp_tms: self.base.ramp_tms.or(fallback.base.ramp_tms),
                op_mod_imp_lim_w: self
                    .base
                    .op_mod_imp_lim_w
                    .or(fallback.base.op_mod_imp_lim_w),
                op_mod_exp_lim_w: self
                    .base
                    .op_mod_exp_lim_w
                    .or(fallback.base.op_mod_exp_lim_w),
                op_mod_gen_lim_w: self
                    .base
                    .op_mod_gen_lim_w
                    .or(fallback.base.op_mod_gen_lim_w),
                op_mod_load_lim_w: self
                    .base
                    .op_mod_load_lim_w
                    .or(fallback.base.op_mod_load_lim_w),
            },
            set_es_delay: self.set_es_delay.or(fallback.set_es_delay),
            set_es_high_freq: self.set_es_high_freq.or(fallback.set_es_high_freq),
            set_es_high_volt: self.set_es_high_volt.or(fallback.set_es_high_volt),
            set_es_low_freq: self.set_es_low_freq.or(fallback.set_es_low_freq),
            set_es_low_volt: self.set_es_low_volt.or(fallback.set_es_low_volt),
            set_es_ramp_tms: self.set_es_ramp_tms.or(fallback.set_es_ramp_tms),
            set_es_random_delay: self.set_es_random_delay.or(fallback.set_es_random_delay),
            set_grad_w: self.set_grad_w.or(fallback.set_grad_w),
            set_soft_grad_w: self.set_soft_grad_w.or(fallback.set_soft_grad_w),
        }
    }
}

impl From<&DefaultDERControl> for ControlAttributes {
    fn from(value: &DefaultDERControl) -> Self {
        ControlAttributes {
            base: value.der_control_base.clone(),

            set_es_delay: value.set_es_delay,
            set_es_high_freq: value.set_es_high_freq,
            set_es_high_volt: value.set_es_high_volt,
            set_es_low_freq: value.set_es_low_freq,
            set_es_low_volt: value.set_es_low_volt,
            set_es_ramp_tms: value.set_es_ramp_tms,
            set_es_random_delay: value.set_es_random_delay,
            set_grad_w: value.set_grad_w,
            set_soft_grad_w: value.set_soft_grad_w,
        }
    }
}

impl From<&DERControl> for ControlAttributes {
    fn from(value: &DERControl) -> Self {
        ControlAttributes {
            base: value.der_control_base.clone(),

            // All of the DefaultDERControl-specific options are None
            ..ControlAttributes::default()
        }
    }
}

impl From<ControlAttributes> for Event {
    fn from(value: ControlAttributes) -> Self {
        Event::ParametersChanged(Arc::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Helper strategy to generate random ControlAttributes
    fn arb_control_attributes() -> impl Strategy<Value = ControlAttributes> {
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
                    let mut base = DERControlBase::default();
                    if base_flags[0] {
                        base.op_mod_connect = Some(Default::default());
                    }
                    if base_flags[1] {
                        base.op_mod_energize = Some(Default::default());
                    }
                    if base_flags[2] {
                        base.op_mod_fixed_pf_absorb_w = Some(Default::default());
                    }
                    if base_flags[3] {
                        base.op_mod_fixed_pf_inject_w = Some(Default::default());
                    }
                    if base_flags[4] {
                        base.op_mod_fixed_var = Some(Default::default());
                    }
                    if base_flags[5] {
                        base.op_mod_fixed_w = Some(Default::default());
                    }
                    if base_flags[6] {
                        base.op_mod_freq_droop = Some(Default::default());
                    }
                    if base_flags[7] {
                        base.op_mod_freq_watt = Some(Default::default());
                    }
                    if base_flags[8] {
                        base.op_mod_hfrt_may_trip = Some(Default::default());
                    }
                    if base_flags[9] {
                        base.op_mod_hfrt_must_trip = Some(Default::default());
                    }
                    if base_flags[10] {
                        base.op_mod_hvrt_may_trip = Some(Default::default());
                    }
                    if base_flags[11] {
                        base.op_mod_hvrt_momentary_cessation = Some(Default::default());
                    }
                    if base_flags[12] {
                        base.op_mod_hvrt_must_trip = Some(Default::default());
                    }
                    if base_flags[13] {
                        base.op_mod_lfrt_may_trip = Some(Default::default());
                    }
                    if base_flags[14] {
                        base.op_mod_lfrt_must_trip = Some(Default::default());
                    }
                    if base_flags[15] {
                        base.op_mod_lvrt_may_trip = Some(Default::default());
                    }
                    if base_flags[16] {
                        base.op_mod_lvrt_momentary_cessation = Some(Default::default());
                    }
                    if base_flags[17] {
                        base.op_mod_lvrt_must_trip = Some(Default::default());
                    }
                    if base_flags[18] {
                        base.op_mod_max_lim_w = Some(Default::default());
                    }
                    if base_flags[19] {
                        base.op_mod_target_var = Some(Default::default());
                    }
                    if base_flags[20] {
                        base.op_mod_target_w = Some(Default::default());
                    }
                    if base_flags[21] {
                        base.op_mod_volt_var = Some(Default::default());
                    }
                    if base_flags[22] {
                        base.op_mod_volt_watt = Some(Default::default());
                    }
                    if base_flags[23] {
                        base.op_mod_watt_pf = Some(Default::default());
                    }
                    if base_flags[24] {
                        base.op_mod_watt_var = Some(Default::default());
                    }
                    if base_flags[25] {
                        base.ramp_tms = Some(Default::default());
                    }
                    if base_flags[26] {
                        base.op_mod_imp_lim_w = Some(Default::default());
                    }
                    if base_flags[27] {
                        base.op_mod_exp_lim_w = Some(Default::default());
                    }
                    if base_flags[28] {
                        base.op_mod_gen_lim_w = Some(Default::default());
                    }
                    if base_flags[29] {
                        base.op_mod_load_lim_w = Some(Default::default());
                    }

                    let mut ca = ControlAttributes {
                        base,
                        ..Default::default()
                    };

                    if ca_flags[0] {
                        ca.set_es_delay = Some(Uint32(es_delay));
                    }
                    if ca_flags[1] {
                        ca.set_es_high_freq = Some(Uint16(es_high_freq));
                    }
                    if ca_flags[2] {
                        ca.set_es_high_volt = Some(Int16(es_high_volt));
                    }
                    if ca_flags[3] {
                        ca.set_es_low_freq = Some(Uint16(es_low_freq));
                    }
                    if ca_flags[4] {
                        ca.set_es_low_volt = Some(Int16(es_low_volt));
                    }
                    if ca_flags[5] {
                        ca.set_es_ramp_tms = Some(Uint32(es_ramp_tms));
                    }
                    if ca_flags[6] {
                        ca.set_es_random_delay = Some(Uint32(es_random_delay));
                    }
                    if ca_flags[7] {
                        ca.set_grad_w = Some(Uint16(grad_w));
                    }
                    if ca_flags[8] {
                        ca.set_soft_grad_w = Some(Uint16(soft_grad_w));
                    }

                    ca
                },
            )
    }

    proptest! {
        #[test]
        fn test_overlay_identity(ca in arb_control_attributes()) {
            prop_assert_eq!(ca.clone().overlay_on(ControlAttributes::default()), ca.clone());
            prop_assert_eq!(ControlAttributes::default().overlay_on(ca.clone()), ca);
        }

        #[test]
        fn test_overlay_associative(ca1 in arb_control_attributes(), ca2 in arb_control_attributes(), ca3 in arb_control_attributes()) {
            let left = ca1.clone().overlay_on(ca2.clone().overlay_on(ca3.clone()));
            let right = ca1.overlay_on(ca2).overlay_on(ca3);
            prop_assert_eq!(left, right);
        }

        #[test]
        fn test_overlay_idempotence(ca in arb_control_attributes()) {
            prop_assert_eq!(ca.clone().overlay_on(ca.clone()), ca);
        }

        #[test]
        fn test_overlay_precedence(ca2 in arb_control_attributes()) {
            // Construct a ControlAttributes where all fields are Some
            let base = DERControlBase {
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
            };

            let ca1 = ControlAttributes {
                base,
                set_es_delay: Some(Uint32(1)),
                set_es_high_freq: Some(Uint16(2)),
                set_es_high_volt: Some(Int16(3)),
                set_es_low_freq: Some(Uint16(4)),
                set_es_low_volt: Some(Int16(5)),
                set_es_ramp_tms: Some(Uint32(6)),
                set_es_random_delay: Some(Uint32(7)),
                set_grad_w: Some(Uint16(8)),
                set_soft_grad_w: Some(Uint16(9)),
            };

            prop_assert_eq!(ca1.clone().overlay_on(ca2), ca1);
        }

        #[test]
        fn test_active_attribute_count_consistency(ca1 in arb_control_attributes(), ca2 in arb_control_attributes()) {
            let merged = ca1.clone().overlay_on(ca2.clone());
            let n1 = ca1.num_active();
            let n2 = ca2.num_active();
            let n_merged = merged.num_active();

            prop_assert!(n_merged >= n1);
            prop_assert!(n_merged >= n2);
            prop_assert!(n_merged <= n1 + n2);
        }
    }

    #[test]
    fn test_from_default_der_control() {
        let mut dderc = DefaultDERControl::default();
        dderc.der_control_base.op_mod_connect = Some(Default::default());
        dderc.set_es_delay = Some(Uint32(100));
        dderc.set_grad_w = Some(Uint16(200));

        let ca = ControlAttributes::from(&dderc);
        assert_eq!(ca.base.op_mod_connect, Some(Default::default()));
        assert_eq!(ca.set_es_delay, Some(Uint32(100)));
        assert_eq!(ca.set_grad_w, Some(Uint16(200)));
        assert_eq!(ca.set_es_high_freq, None);
    }

    #[test]
    fn test_from_der_control() {
        let mut derc = DERControl::default();
        derc.der_control_base.op_mod_energize = Some(Default::default());

        let ca = ControlAttributes::from(&derc);
        assert_eq!(ca.base.op_mod_energize, Some(Default::default()));
        assert_eq!(ca.set_es_delay, None);
        assert_eq!(ca.set_grad_w, None);
    }
}
