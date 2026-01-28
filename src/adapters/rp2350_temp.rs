//! RP2350 onboard temperature sensor adapter
//!
//! This adapter implements the SensorPort trait for the RP2350's
//! built-in temperature sensor, accessed via ADC.

use crate::domain::{SensorId, SensorReading, TemperatureCalibration};
use crate::ports::sensor::{SensorError, SensorPort};
use core::sync::atomic::{AtomicU16, Ordering};
use embassy_rp::adc::{Adc, Channel as AdcChannel};

/// RP2350 onboard temperature sensor adapter
///
/// Reads the internal temperature sensor via ADC and converts
/// the raw value to temperature using the configured calibration.
pub struct Rp2350TempSensor<'a> {
    /// ADC peripheral (blocking mode to avoid DMA conflicts with flash)
    adc: Adc<'a, embassy_rp::adc::Blocking>,
    /// Temperature sensor channel
    channel: AdcChannel<'a>,
    /// Calibration parameters
    calibration: TemperatureCalibration,
    /// Last raw ADC value (for diagnostics)
    last_raw: AtomicU16,
}

impl<'a> Rp2350TempSensor<'a> {
    /// Create a new RP2350 temperature sensor adapter
    ///
    /// # Arguments
    ///
    /// * `adc` - ADC peripheral in blocking mode
    /// * `channel` - Temperature sensor ADC channel
    pub fn new(adc: Adc<'a, embassy_rp::adc::Blocking>, channel: AdcChannel<'a>) -> Self {
        Self {
            adc,
            channel,
            calibration: TemperatureCalibration::RP2350_DEFAULT,
            last_raw: AtomicU16::new(0),
        }
    }

    /// Create with custom calibration
    pub fn with_calibration(
        adc: Adc<'a, embassy_rp::adc::Blocking>,
        channel: AdcChannel<'a>,
        calibration: TemperatureCalibration,
    ) -> Self {
        Self {
            adc,
            channel,
            calibration,
            last_raw: AtomicU16::new(0),
        }
    }

    /// Update calibration parameters
    pub fn set_calibration(&mut self, calibration: TemperatureCalibration) {
        self.calibration = calibration;
    }

    /// Get current calibration
    pub fn calibration(&self) -> TemperatureCalibration {
        self.calibration
    }

    /// Get current timestamp in microseconds since boot
    fn get_timestamp_us() -> i64 {
        embassy_time::Instant::now().as_micros() as i64
    }
}

impl<'a> SensorPort for Rp2350TempSensor<'a> {
    async fn read(&mut self) -> Result<SensorReading, SensorError> {
        // Use blocking read to avoid DMA conflicts with flash
        let adc_value = self
            .adc
            .blocking_read(&mut self.channel)
            .map_err(|_| SensorError::ReadFailed)?;

        // Store for diagnostics
        self.last_raw.store(adc_value, Ordering::Relaxed);

        // Convert to temperature
        let temperature = self.calibration.adc_to_celsius(adc_value);
        let timestamp = Self::get_timestamp_us();

        Ok(SensorReading::new(timestamp, temperature, SensorId::ONBOARD))
    }

    fn sensor_id(&self) -> SensorId {
        SensorId::ONBOARD
    }

    fn last_raw_value(&self) -> Option<u16> {
        Some(self.last_raw.load(Ordering::Relaxed))
    }
}
