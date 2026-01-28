//! Temperature calibration domain service
//!
//! This module provides temperature calibration logic for converting
//! raw ADC readings to temperature values.

/// Temperature calibration parameters
///
/// Converts raw ADC readings to temperature in Celsius using a linear formula:
/// `temperature = adc_value * scale + offset`
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct TemperatureCalibration {
    /// Scale factor for ADC to temperature conversion
    pub scale: f32,
    /// Offset for calibration
    pub offset: f32,
}

impl TemperatureCalibration {
    /// RP2350 empirical calibration
    ///
    /// The standard RP2040 formula `temp = 27 - (voltage - 0.706) / 0.001721`
    /// does NOT work on RP2350. The ADC returns values around 50-60 at room
    /// temperature instead of the expected ~876.
    ///
    /// This empirical calibration is based on observed ADC values:
    /// - ADC ~ 57 at room temperature (~27C)
    /// - Linear scale factor: 0.474 (so temp ~ ADC * 0.474)
    pub const RP2350_DEFAULT: Self = Self {
        scale: 0.474,
        offset: 0.0,
    };

    /// RP2040 calibration (for reference, not used on RP2350)
    ///
    /// Standard formula: temp = 27 - (voltage - 0.706) / 0.001721
    /// where voltage = adc_value * 3.3 / 4096
    ///
    /// This simplifies to approximately: temp = -0.567 * adc + 520.9
    pub const RP2040_DEFAULT: Self = Self {
        scale: -0.567,
        offset: 520.9,
    };

    /// Create a new calibration with custom parameters
    pub const fn new(scale: f32, offset: f32) -> Self {
        Self { scale, offset }
    }

    /// Convert an ADC reading to temperature in Celsius
    #[inline]
    pub fn adc_to_celsius(&self, adc_value: u16) -> f32 {
        adc_value as f32 * self.scale + self.offset
    }

    /// Create calibration from two known points (for custom calibration)
    ///
    /// Given two (adc_value, temperature) pairs, calculates the linear
    /// calibration parameters.
    pub fn from_two_points(
        adc1: u16,
        temp1: f32,
        adc2: u16,
        temp2: f32,
    ) -> Self {
        let adc1 = adc1 as f32;
        let adc2 = adc2 as f32;

        // Linear interpolation: temp = scale * adc + offset
        // scale = (temp2 - temp1) / (adc2 - adc1)
        // offset = temp1 - scale * adc1
        let scale = (temp2 - temp1) / (adc2 - adc1);
        let offset = temp1 - scale * adc1;

        Self { scale, offset }
    }
}

impl Default for TemperatureCalibration {
    fn default() -> Self {
        Self::RP2350_DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rp2350_calibration() {
        let cal = TemperatureCalibration::RP2350_DEFAULT;
        // ADC 57 should give approximately 27C
        let temp = cal.adc_to_celsius(57);
        assert!((temp - 27.0).abs() < 1.0);
    }

    #[test]
    fn test_from_two_points() {
        let cal = TemperatureCalibration::from_two_points(50, 24.0, 60, 28.0);
        // Should interpolate correctly
        let temp = cal.adc_to_celsius(55);
        assert!((temp - 26.0).abs() < 0.1);
    }
}
