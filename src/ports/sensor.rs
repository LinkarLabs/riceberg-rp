//! Sensor port - abstraction for reading sensor data
//!
//! This trait allows the application to read sensor data without knowing
//! the specific hardware implementation (ADC, I2C, SPI, mock, etc.)

use crate::domain::{SensorId, SensorReading};

/// Error type for sensor operations
#[derive(Clone, Copy, Debug, defmt::Format)]
pub enum SensorError {
    /// Failed to read from sensor
    ReadFailed,
    /// Sensor not initialized
    NotInitialized,
    /// Sensor returned invalid data
    InvalidData,
    /// Hardware error
    HardwareError,
    /// Timeout waiting for sensor
    Timeout,
}

/// Configuration for sensor behavior
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct SensorConfig {
    /// How often to read (milliseconds)
    pub read_interval_ms: u64,
    /// Whether to apply calibration
    pub calibrate: bool,
    /// Number of readings to average (for noise reduction)
    pub averaging_samples: u8,
}

impl Default for SensorConfig {
    fn default() -> Self {
        Self {
            read_interval_ms: 5000, // 5 seconds
            calibrate: true,
            averaging_samples: 1,
        }
    }
}

impl SensorConfig {
    /// Create config for high-frequency sampling
    pub const fn high_frequency() -> Self {
        Self {
            read_interval_ms: 100, // 100ms
            calibrate: true,
            averaging_samples: 4,
        }
    }

    /// Create config for low-power operation
    pub const fn low_power() -> Self {
        Self {
            read_interval_ms: 60000, // 1 minute
            calibrate: true,
            averaging_samples: 1,
        }
    }
}

/// Port for reading sensor data
///
/// This trait abstracts hardware sensors, allowing the domain to remain
/// independent of specific sensor implementations (ADC, I2C, SPI, etc.)
///
/// # Example Implementation
///
/// ```ignore
/// struct Rp2350TempSensor {
///     adc: Adc<'static, Async>,
///     channel: AdcChannel<'static>,
///     calibration: TemperatureCalibration,
/// }
///
/// impl SensorPort for Rp2350TempSensor {
///     async fn read(&mut self) -> Result<SensorReading, SensorError> {
///         let adc_value = self.adc.read(&mut self.channel).await?;
///         let temp = self.calibration.adc_to_celsius(adc_value);
///         Ok(SensorReading::new(get_timestamp_us(), temp, SensorId::ONBOARD))
///     }
///
///     fn sensor_id(&self) -> SensorId { SensorId::ONBOARD }
/// }
/// ```
pub trait SensorPort {
    /// Read a single sensor value
    ///
    /// Returns a `SensorReading` containing the timestamp, temperature,
    /// and sensor ID.
    fn read(&mut self) -> impl core::future::Future<Output = Result<SensorReading, SensorError>>;

    /// Get the sensor identifier
    ///
    /// This identifies which sensor is being read (e.g., onboard, external).
    fn sensor_id(&self) -> SensorId;

    /// Get the last raw ADC value (for diagnostics)
    ///
    /// Returns `None` if the sensor doesn't expose raw values.
    fn last_raw_value(&self) -> Option<u16> {
        None
    }
}
