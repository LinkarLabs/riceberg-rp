//! Spectral sensor port - interface for spectral light sensors
//!
//! This port defines how the domain interacts with multi-channel spectral sensors
//! like the AS7343 14-channel sensor.

use crate::domain::{SensorId, SpectralReading};
use core::future::Future;

/// Port trait for spectral sensors
///
/// Implementations provide access to multi-channel spectral light measurements.
/// The AS7343 is the primary target, but this interface is generic enough
/// for other spectral sensors.
pub trait SpectralSensorPort {
    /// Read all spectral channels and return a complete reading
    ///
    /// This triggers a full measurement cycle including all channels.
    /// The integration time determines how long the sensor accumulates light.
    fn read(&mut self) -> impl Future<Output = Result<SpectralReading, SpectralSensorError>>;

    /// Get the sensor ID for this sensor instance
    fn sensor_id(&self) -> SensorId;

    /// Set the analog gain for measurements
    ///
    /// Higher gain increases sensitivity but reduces dynamic range.
    /// Valid gain values depend on the specific sensor.
    ///
    /// For AS7343: 0-10 maps to 0.5x to 512x gain
    fn set_gain(&mut self, gain: u8) -> impl Future<Output = Result<(), SpectralSensorError>>;

    /// Set the integration time in milliseconds
    ///
    /// Longer integration times increase sensitivity and reduce noise
    /// but take longer to complete a measurement.
    ///
    /// For AS7343: Typical range 2.78ms to 182ms
    fn set_integration_time_ms(
        &mut self,
        ms: f32,
    ) -> impl Future<Output = Result<(), SpectralSensorError>>;

    /// Check if the sensor is initialized and ready
    fn is_ready(&self) -> bool;
}

/// Spectral sensor configuration
#[derive(Clone, Copy, Debug)]
pub struct SpectralSensorConfig {
    /// Gain setting (sensor-specific meaning)
    pub gain: u8,
    /// Integration time in milliseconds
    pub integration_time_ms: f32,
    /// How often to take readings (milliseconds)
    pub read_interval_ms: u64,
}

impl Default for SpectralSensorConfig {
    fn default() -> Self {
        Self {
            gain: 8, // 128x gain (good middle ground for AS7343)
            integration_time_ms: 29.0,
            read_interval_ms: 1000, // 1 second
        }
    }
}

/// Errors that can occur during spectral sensor operations
#[derive(Clone, Copy, Debug, defmt::Format)]
pub enum SpectralSensorError {
    /// Sensor read operation failed
    ReadFailed,
    /// Sensor has not been initialized
    NotInitialized,
    /// Operation timed out
    Timeout,
    /// One or more channels are saturated (values at maximum)
    Saturated,
    /// I2C communication error
    I2cError,
    /// Invalid configuration parameter
    InvalidConfig,
    /// Sensor not responding or not detected
    NotDetected,
}
