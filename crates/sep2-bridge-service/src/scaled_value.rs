use num_traits::Bounded;

/// A value with a scale factor. This is common for translation between SEP2 and sunspec.
///
/// Note: we derive PartialEq, even though that will not recognise the same
/// value at different scale factors as equivalent. This is fine for our
/// purposes of seeing if a value has changed, as a false negative is just a
/// little bit of extra communication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaledValue<T> {
    pub value: T,
    /// The scale factor (power of ten) that is attached to value.
    pub sf: i16,
}

impl<T: ScaledValueInner> ScaledValue<T> {
    pub fn new(value: T, sf: i16) -> ScaledValue<T> {
        ScaledValue { value, sf }
    }

    /// Rescale a value to a new scale factor.
    ///
    /// Moving to a coarser scale loses precision: the result is rounded to the
    /// nearest value, with a half step rounding away from zero. Moving to a
    /// finer scale can overflow, in which case the value is bounded rather than
    /// erroring.
    pub fn rescale(self, target_sf: i16) -> ScaledValue<T> {
        // Note the ordering of the terms here is to indicate how many powers of
        // ten we need to apply to convert the value. E.g. if we move from 1 to
        // 0, we need to apply a 10^1 scaling.
        //
        // The SFs are widened to i32. This is because saturating_pow requires a
        // u32 anyway, and widening avoids a potential overflow.
        let sf_difference = i32::from(self.sf) - i32::from(target_sf);

        // If the SFs are the same, we exit early
        if sf_difference == 0 {
            return self;
        }

        // The arithmetic is done in i64 to avoid duplication for each individual type.
        let value: i64 = self.value.into();
        let factor = 10i64.saturating_pow(sf_difference.unsigned_abs());

        let scaled = if sf_difference > 0 {
            value.saturating_mul(factor)
        } else {
            // We want to round rather than truncate. A half-step nudge allows
            // us to avoid remainders or floating point rounding.
            let half_step = factor / 2;
            let nudge = if value < 0 { -half_step } else { half_step };
            (value + nudge) / factor
        };

        ScaledValue {
            value: saturating_from_i64(scaled),
            sf: target_sf,
        }
    }
}

/// The integer types a ScaledValue can hold.
///
/// ScaledValue::rescale converts to (Into) and from (TryFrom) an i64 and needs
/// to know the min/max values (Bounded) of the type.
pub trait ScaledValueInner: Copy + Into<i64> + TryFrom<i64> + Bounded {}

impl ScaledValueInner for u16 {}
impl ScaledValueInner for i16 {}
impl ScaledValueInner for u32 {}
impl ScaledValueInner for i32 {}

/// Narrows back to the value type, capping at its bounds rather than wrapping.
fn saturating_from_i64<T: ScaledValueInner>(value: i64) -> T {
    match T::try_from(value) {
        Ok(value) => value,
        // Return the saturating limits.
        Err(_) => {
            if value > 0 {
                T::max_value()
            } else {
                T::min_value()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rescale_identity() {
        let value = ScaledValue::new(2450u16, -2);
        assert_eq!(value.rescale(-2), value);
    }

    #[test]
    fn rescale_sets_the_target_sf() {
        assert_eq!(ScaledValue::new(2450u16, -2).rescale(0).sf, 0);
        assert_eq!(ScaledValue::new(2450u16, -2).rescale(-4).sf, -4);
    }

    #[test]
    fn rescale_down_rounds_to_nearest() {
        // Exact conversions are unaffected by the rounding.
        assert_eq!(ScaledValue::new(2400u16, -2).rescale(0).value, 24);
        assert_eq!(ScaledValue::new(2450u16, -2).rescale(-1).value, 245);

        // Otherwise the nearest value wins, rather than always rounding down.
        assert_eq!(ScaledValue::new(2449u16, -2).rescale(0).value, 24);
        assert_eq!(ScaledValue::new(2451u16, -2).rescale(0).value, 25);
        assert_eq!(ScaledValue::new(2499u16, -2).rescale(0).value, 25);
    }

    #[test]
    fn rescale_rounds_a_half_step_away_from_zero() {
        assert_eq!(ScaledValue::new(2450u16, -2).rescale(0).value, 25);
        assert_eq!(ScaledValue::new(2550u16, -2).rescale(0).value, 26);
        assert_eq!(ScaledValue::new(-2450i16, -2).rescale(0).value, -25);
        assert_eq!(ScaledValue::new(-2550i16, -2).rescale(0).value, -26);
    }

    #[test]
    fn rescale_up() {
        assert_eq!(ScaledValue::new(24u16, 0).rescale(-2).value, 2400);
        assert_eq!(ScaledValue::new(51u32, -2).rescale(-3).value, 510);
    }

    #[test]
    fn rescale_saturates_rather_than_wrapping() {
        // 70_000 exceeds u16.
        assert_eq!(ScaledValue::new(700u16, 0).rescale(-2).value, u16::MAX);
        assert_eq!(ScaledValue::new(700i16, 0).rescale(-2).value, i16::MAX);
        assert_eq!(ScaledValue::new(-700i16, 0).rescale(-2).value, i16::MIN);
    }

    #[test]
    fn rescale_negative_values() {
        assert_eq!(ScaledValue::new(-2500i16, -2).rescale(0).value, -25);
        assert_eq!(ScaledValue::new(-2550i16, -2).rescale(-1).value, -255);
        assert_eq!(ScaledValue::new(-2549i16, -2).rescale(-1).value, -255);
        assert_eq!(ScaledValue::new(-24i16, -1).rescale(0).value, -2);
    }

    #[test]
    fn rescale_handles_an_exponent_too_large_for_the_type() {
        // 10^10 does not fit in a u16. Dividing by it must give zero, not
        // divide by a saturated factor, and multiplying must saturate.
        assert_eq!(ScaledValue::new(5u16, -5).rescale(5).value, 0);
        assert_eq!(ScaledValue::new(5u16, 5).rescale(-5).value, u16::MAX);
        assert_eq!(ScaledValue::new(0u16, 5).rescale(-5).value, 0);
        assert_eq!(ScaledValue::new(-5i16, 5).rescale(-5).value, i16::MIN);
    }

    proptest! {
        /// Scaling up and back down again is lossless, as long as the
        /// intermediate value is small enough not to saturate.
        #[test]
        fn rescale_up_then_down_round_trips(value in 0..=(u16::MAX / 100)) {
            let original = ScaledValue::new(value, 0);
            prop_assert_eq!(original.rescale(-2).rescale(0), original);
        }

        /// Rescaling always reports the scale factor it was asked for.
        #[test]
        fn rescale_always_reaches_the_target_sf(value in any::<u16>(), from in -6i16..6, to in -6i16..6) {
            prop_assert_eq!(ScaledValue::new(value, from).rescale(to).sf, to);
        }
    }
}
