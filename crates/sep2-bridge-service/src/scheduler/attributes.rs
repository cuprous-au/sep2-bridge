use std::sync::Arc;

use sep2_common::packages::{
    der::{
        ActivePower, DERControl, DERControlBase, DERCurve, DefaultDERControl, FixedVar,
        FreqDroopType, PowerFactorWithExcitation, ReactivePower,
    },
    primitives::{Int16, Uint16, Uint32},
    types::{Percent, SignedPercent},
};

use super::Event;

/// Lists all of the attributes that can be set by DERControl or DefaultDERControl
/// objects in a way that allows us to easily project higher primacy controls on
/// top of one another.
///
/// This is a subset of the parameters available on a DefaultDERControl struct.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ControlAttributes {
    // These parameters come from DERControlBase, with DERCurveLink replaced with DERCurve.
    pub op_mod_connect: Option<bool>,
    pub op_mod_energize: Option<bool>,
    pub op_mod_fixed_pf_absorb_w: Option<PowerFactorWithExcitation>,
    pub op_mod_fixed_pf_inject_w: Option<PowerFactorWithExcitation>,
    pub op_mod_fixed_var: Option<FixedVar>,
    pub op_mod_fixed_w: Option<SignedPercent>,
    pub op_mod_freq_droop: Option<FreqDroopType>,
    pub op_mod_freq_watt: Option<DERCurve>,
    pub op_mod_hfrt_may_trip: Option<DERCurve>,
    pub op_mod_hfrt_must_trip: Option<DERCurve>,
    pub op_mod_hvrt_may_trip: Option<DERCurve>,
    pub op_mod_hvrt_momentary_cessation: Option<DERCurve>,
    pub op_mod_hvrt_must_trip: Option<DERCurve>,
    pub op_mod_lfrt_may_trip: Option<DERCurve>,
    pub op_mod_lfrt_must_trip: Option<DERCurve>,
    pub op_mod_lvrt_may_trip: Option<DERCurve>,
    pub op_mod_lvrt_momentary_cessation: Option<DERCurve>,
    pub op_mod_lvrt_must_trip: Option<DERCurve>,
    pub op_mod_max_lim_w: Option<Percent>,
    pub op_mod_target_var: Option<ReactivePower>,
    pub op_mod_target_w: Option<ActivePower>,
    pub op_mod_volt_var: Option<DERCurve>,
    pub op_mod_volt_watt: Option<DERCurve>,
    pub op_mod_watt_pf: Option<DERCurve>,
    pub op_mod_watt_var: Option<DERCurve>,
    pub ramp_tms: Option<Uint16>,
    pub op_mod_imp_lim_w: Option<ActivePower>,
    pub op_mod_exp_lim_w: Option<ActivePower>,
    pub op_mod_gen_lim_w: Option<ActivePower>,
    pub op_mod_load_lim_w: Option<ActivePower>,

    // These parameters come from DefaultDERControl.
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
        self.op_mod_connect.is_some() as u32
            + self.op_mod_energize.is_some() as u32
            + self.op_mod_fixed_pf_absorb_w.is_some() as u32
            + self.op_mod_fixed_pf_inject_w.is_some() as u32
            + self.op_mod_fixed_var.is_some() as u32
            + self.op_mod_fixed_w.is_some() as u32
            + self.op_mod_freq_droop.is_some() as u32
            + self.op_mod_freq_watt.is_some() as u32
            + self.op_mod_hfrt_may_trip.is_some() as u32
            + self.op_mod_hfrt_must_trip.is_some() as u32
            + self.op_mod_hvrt_may_trip.is_some() as u32
            + self.op_mod_hvrt_momentary_cessation.is_some() as u32
            + self.op_mod_hvrt_must_trip.is_some() as u32
            + self.op_mod_lfrt_may_trip.is_some() as u32
            + self.op_mod_lfrt_must_trip.is_some() as u32
            + self.op_mod_lvrt_may_trip.is_some() as u32
            + self.op_mod_lvrt_momentary_cessation.is_some() as u32
            + self.op_mod_lvrt_must_trip.is_some() as u32
            + self.op_mod_max_lim_w.is_some() as u32
            + self.op_mod_target_var.is_some() as u32
            + self.op_mod_target_w.is_some() as u32
            + self.op_mod_volt_var.is_some() as u32
            + self.op_mod_volt_watt.is_some() as u32
            + self.op_mod_watt_pf.is_some() as u32
            + self.op_mod_watt_var.is_some() as u32
            + self.ramp_tms.is_some() as u32
            + self.op_mod_imp_lim_w.is_some() as u32
            + self.op_mod_exp_lim_w.is_some() as u32
            + self.op_mod_gen_lim_w.is_some() as u32
            + self.op_mod_load_lim_w.is_some() as u32
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
            op_mod_connect: self.op_mod_connect.or(fallback.op_mod_connect),
            op_mod_energize: self.op_mod_energize.or(fallback.op_mod_energize),
            op_mod_fixed_pf_absorb_w: self
                .op_mod_fixed_pf_absorb_w
                .or(fallback.op_mod_fixed_pf_absorb_w),
            op_mod_fixed_pf_inject_w: self
                .op_mod_fixed_pf_inject_w
                .or(fallback.op_mod_fixed_pf_inject_w),
            op_mod_fixed_var: self.op_mod_fixed_var.or(fallback.op_mod_fixed_var),
            op_mod_fixed_w: self.op_mod_fixed_w.or(fallback.op_mod_fixed_w),
            op_mod_freq_droop: self.op_mod_freq_droop.or(fallback.op_mod_freq_droop),
            op_mod_freq_watt: self.op_mod_freq_watt.or(fallback.op_mod_freq_watt),
            op_mod_hfrt_may_trip: self.op_mod_hfrt_may_trip.or(fallback.op_mod_hfrt_may_trip),
            op_mod_hfrt_must_trip: self
                .op_mod_hfrt_must_trip
                .or(fallback.op_mod_hfrt_must_trip),
            op_mod_hvrt_may_trip: self.op_mod_hvrt_may_trip.or(fallback.op_mod_hvrt_may_trip),
            op_mod_hvrt_momentary_cessation: self
                .op_mod_hvrt_momentary_cessation
                .or(fallback.op_mod_hvrt_momentary_cessation),
            op_mod_hvrt_must_trip: self
                .op_mod_hvrt_must_trip
                .or(fallback.op_mod_hvrt_must_trip),
            op_mod_lfrt_may_trip: self.op_mod_lfrt_may_trip.or(fallback.op_mod_lfrt_may_trip),
            op_mod_lfrt_must_trip: self
                .op_mod_lfrt_must_trip
                .or(fallback.op_mod_lfrt_must_trip),
            op_mod_lvrt_may_trip: self.op_mod_lvrt_may_trip.or(fallback.op_mod_lvrt_may_trip),
            op_mod_lvrt_momentary_cessation: self
                .op_mod_lvrt_momentary_cessation
                .or(fallback.op_mod_lvrt_momentary_cessation),
            op_mod_lvrt_must_trip: self
                .op_mod_lvrt_must_trip
                .or(fallback.op_mod_lvrt_must_trip),
            op_mod_max_lim_w: self.op_mod_max_lim_w.or(fallback.op_mod_max_lim_w),
            op_mod_target_var: self.op_mod_target_var.or(fallback.op_mod_target_var),
            op_mod_target_w: self.op_mod_target_w.or(fallback.op_mod_target_w),
            op_mod_volt_var: self.op_mod_volt_var.or(fallback.op_mod_volt_var),
            op_mod_volt_watt: self.op_mod_volt_watt.or(fallback.op_mod_volt_watt),
            op_mod_watt_pf: self.op_mod_watt_pf.or(fallback.op_mod_watt_pf),
            op_mod_watt_var: self.op_mod_watt_var.or(fallback.op_mod_watt_var),
            ramp_tms: self.ramp_tms.or(fallback.ramp_tms),
            op_mod_imp_lim_w: self.op_mod_imp_lim_w.or(fallback.op_mod_imp_lim_w),
            op_mod_exp_lim_w: self.op_mod_exp_lim_w.or(fallback.op_mod_exp_lim_w),
            op_mod_gen_lim_w: self.op_mod_gen_lim_w.or(fallback.op_mod_gen_lim_w),
            op_mod_load_lim_w: self.op_mod_load_lim_w.or(fallback.op_mod_load_lim_w),
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

impl ControlAttributes {
    pub fn from_default_control<F>(value: &DefaultDERControl, curve_lookup: F) -> Self
    where
        F: Fn(String) -> Option<DERCurve>,
    {
        ControlAttributes {
            set_es_delay: value.set_es_delay,
            set_es_high_freq: value.set_es_high_freq,
            set_es_high_volt: value.set_es_high_volt,
            set_es_low_freq: value.set_es_low_freq,
            set_es_low_volt: value.set_es_low_volt,
            set_es_ramp_tms: value.set_es_ramp_tms,
            set_es_random_delay: value.set_es_random_delay,
            set_grad_w: value.set_grad_w,
            set_soft_grad_w: value.set_soft_grad_w,

            ..ControlAttributes::from_control_base(value.der_control_base.clone(), curve_lookup)
        }
    }

    pub fn from_control<F>(value: &DERControl, curve_lookup: F) -> Self
    where
        F: Fn(String) -> Option<DERCurve>,
    {
        ControlAttributes::from_control_base(value.der_control_base.clone(), curve_lookup)
    }

    fn from_control_base<F>(value: DERControlBase, curve_lookup: F) -> Self
    where
        F: Fn(String) -> Option<DERCurve>,
    {
        ControlAttributes {
            op_mod_connect: value.op_mod_connect,
            op_mod_energize: value.op_mod_energize,
            op_mod_fixed_pf_absorb_w: value.op_mod_fixed_pf_absorb_w,
            op_mod_fixed_pf_inject_w: value.op_mod_fixed_pf_inject_w,
            op_mod_fixed_var: value.op_mod_fixed_var,
            op_mod_fixed_w: value.op_mod_fixed_w,
            op_mod_freq_droop: value.op_mod_freq_droop,
            op_mod_freq_watt: value
                .op_mod_freq_watt
                .and_then(|link| curve_lookup(link.href)),
            op_mod_hfrt_may_trip: value
                .op_mod_hfrt_may_trip
                .and_then(|link| curve_lookup(link.href)),
            op_mod_hfrt_must_trip: value
                .op_mod_hfrt_must_trip
                .and_then(|link| curve_lookup(link.href)),
            op_mod_hvrt_may_trip: value
                .op_mod_hvrt_may_trip
                .and_then(|link| curve_lookup(link.href)),
            op_mod_hvrt_momentary_cessation: value
                .op_mod_hvrt_momentary_cessation
                .and_then(|link| curve_lookup(link.href)),
            op_mod_hvrt_must_trip: value
                .op_mod_hvrt_must_trip
                .and_then(|link| curve_lookup(link.href)),
            op_mod_lfrt_may_trip: value
                .op_mod_lfrt_may_trip
                .and_then(|link| curve_lookup(link.href)),
            op_mod_lfrt_must_trip: value
                .op_mod_lfrt_must_trip
                .and_then(|link| curve_lookup(link.href)),
            op_mod_lvrt_may_trip: value
                .op_mod_lvrt_may_trip
                .and_then(|link| curve_lookup(link.href)),
            op_mod_lvrt_momentary_cessation: value
                .op_mod_lvrt_momentary_cessation
                .and_then(|link| curve_lookup(link.href)),
            op_mod_lvrt_must_trip: value
                .op_mod_lvrt_must_trip
                .and_then(|link| curve_lookup(link.href)),
            op_mod_max_lim_w: value.op_mod_max_lim_w,
            op_mod_target_var: value.op_mod_target_var,
            op_mod_target_w: value.op_mod_target_w,
            op_mod_volt_var: value
                .op_mod_volt_var
                .and_then(|link| curve_lookup(link.href)),
            op_mod_volt_watt: value
                .op_mod_volt_watt
                .and_then(|link| curve_lookup(link.href)),
            op_mod_watt_pf: value
                .op_mod_watt_pf
                .and_then(|link| curve_lookup(link.href)),
            op_mod_watt_var: value
                .op_mod_watt_var
                .and_then(|link| curve_lookup(link.href)),
            ramp_tms: value.ramp_tms,
            op_mod_imp_lim_w: value.op_mod_imp_lim_w,
            op_mod_exp_lim_w: value.op_mod_exp_lim_w,
            op_mod_gen_lim_w: value.op_mod_gen_lim_w,
            op_mod_load_lim_w: value.op_mod_load_lim_w,

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
                    let mut ca = ControlAttributes::default();
                    if base_flags[0] {
                        ca.op_mod_connect = Some(Default::default());
                    }
                    if base_flags[1] {
                        ca.op_mod_energize = Some(Default::default());
                    }
                    if base_flags[2] {
                        ca.op_mod_fixed_pf_absorb_w = Some(Default::default());
                    }
                    if base_flags[3] {
                        ca.op_mod_fixed_pf_inject_w = Some(Default::default());
                    }
                    if base_flags[4] {
                        ca.op_mod_fixed_var = Some(Default::default());
                    }
                    if base_flags[5] {
                        ca.op_mod_fixed_w = Some(Default::default());
                    }
                    if base_flags[6] {
                        ca.op_mod_freq_droop = Some(Default::default());
                    }
                    if base_flags[7] {
                        ca.op_mod_freq_watt = Some(Default::default());
                    }
                    if base_flags[8] {
                        ca.op_mod_hfrt_may_trip = Some(Default::default());
                    }
                    if base_flags[9] {
                        ca.op_mod_hfrt_must_trip = Some(Default::default());
                    }
                    if base_flags[10] {
                        ca.op_mod_hvrt_may_trip = Some(Default::default());
                    }
                    if base_flags[11] {
                        ca.op_mod_hvrt_momentary_cessation = Some(Default::default());
                    }
                    if base_flags[12] {
                        ca.op_mod_hvrt_must_trip = Some(Default::default());
                    }
                    if base_flags[13] {
                        ca.op_mod_lfrt_may_trip = Some(Default::default());
                    }
                    if base_flags[14] {
                        ca.op_mod_lfrt_must_trip = Some(Default::default());
                    }
                    if base_flags[15] {
                        ca.op_mod_lvrt_may_trip = Some(Default::default());
                    }
                    if base_flags[16] {
                        ca.op_mod_lvrt_momentary_cessation = Some(Default::default());
                    }
                    if base_flags[17] {
                        ca.op_mod_lvrt_must_trip = Some(Default::default());
                    }
                    if base_flags[18] {
                        ca.op_mod_max_lim_w = Some(Default::default());
                    }
                    if base_flags[19] {
                        ca.op_mod_target_var = Some(Default::default());
                    }
                    if base_flags[20] {
                        ca.op_mod_target_w = Some(Default::default());
                    }
                    if base_flags[21] {
                        ca.op_mod_volt_var = Some(Default::default());
                    }
                    if base_flags[22] {
                        ca.op_mod_volt_watt = Some(Default::default());
                    }
                    if base_flags[23] {
                        ca.op_mod_watt_pf = Some(Default::default());
                    }
                    if base_flags[24] {
                        ca.op_mod_watt_var = Some(Default::default());
                    }
                    if base_flags[25] {
                        ca.ramp_tms = Some(Default::default());
                    }
                    if base_flags[26] {
                        ca.op_mod_imp_lim_w = Some(Default::default());
                    }
                    if base_flags[27] {
                        ca.op_mod_exp_lim_w = Some(Default::default());
                    }
                    if base_flags[28] {
                        ca.op_mod_gen_lim_w = Some(Default::default());
                    }
                    if base_flags[29] {
                        ca.op_mod_load_lim_w = Some(Default::default());
                    }

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
            let ca1 = ControlAttributes {
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

        let curve_lookup = |_href| None;
        let ca = ControlAttributes::from_default_control(&dderc, curve_lookup);
        assert_eq!(ca.op_mod_connect, Some(Default::default()));
        assert_eq!(ca.set_es_delay, Some(Uint32(100)));
        assert_eq!(ca.set_grad_w, Some(Uint16(200)));
        assert_eq!(ca.set_es_high_freq, None);
    }

    #[test]
    fn test_from_der_control() {
        let mut derc = DERControl::default();
        derc.der_control_base.op_mod_energize = Some(Default::default());

        let curve_lookup = |_href| None;
        let ca = ControlAttributes::from_control(&derc, curve_lookup);
        assert_eq!(ca.op_mod_energize, Some(Default::default()));
        assert_eq!(ca.set_es_delay, None);
        assert_eq!(ca.set_grad_w, None);
    }
}
