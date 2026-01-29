//! AS7343 spectral sensor adapter
//!
//! This adapter implements the SpectralSensorPort trait using the as7343-rs
//! driver for the AMS/OSRAM AS7343 14-channel spectral sensor.

use as7343::{As7343Async, ChannelData, Config, Gain};
use embassy_time::{Duration, Timer};

use crate::domain::{SensorId, SpectralReading};
use crate::ports::spectral_sensor::{SpectralSensorError, SpectralSensorPort};

/// AS7343 adapter implementing SpectralSensorPort
///
/// Wraps the as7343-rs async driver to provide spectral readings
/// through the hexagonal port interface.
pub struct As7343Adapter<I: embedded_hal_async::i2c::I2c> {
    sensor: As7343Async<I>,
    sensor_id: SensorId,
    ready: bool,
}

impl<I: embedded_hal_async::i2c::I2c> As7343Adapter<I> {
    /// Create a new AS7343 adapter
    ///
    /// The sensor is not initialized until `init()` is called.
    pub fn new(i2c: I, sensor_id: SensorId) -> Self {
        Self {
            sensor: As7343Async::new(i2c),
            sensor_id,
            ready: false,
        }
    }

    /// Initialize the AS7343 sensor with default configuration
    ///
    /// Sets up SMUX for 18-channel (all channels) mode with reasonable defaults:
    /// - Gain: 128x
    /// - Integration time: ~29ms
    pub async fn init(&mut self) -> Result<(), SpectralSensorError> {
        self.sensor.init().await.map_err(|_| SpectralSensorError::NotDetected)?;
        self.ready = true;
        Ok(())
    }

    /// Initialize with a custom configuration
    pub async fn init_with_config(&mut self, config: Config) -> Result<(), SpectralSensorError> {
        self.sensor
            .init_with_config(config)
            .await
            .map_err(|_| SpectralSensorError::NotDetected)?;
        self.ready = true;
        Ok(())
    }

    /// Convert AS7343 ChannelData to domain SpectralReading
    fn channel_data_to_reading(&self, data: &ChannelData, timestamp_us: i64) -> SpectralReading {
        SpectralReading::new(
            timestamp_us,
            self.sensor_id,
            data.f1,
            data.f2,
            data.fz,
            data.f3,
            data.f4,
            data.f5,
            data.fy,
            data.fxl,
            data.f6,
            data.f7,
            data.f8,
            data.nir,
            data.clear,
            data.flicker,
            data.gain as u8,
            data.saturated,
        )
    }

    /// Release the underlying I2C bus
    pub fn release(self) -> I {
        self.sensor.release()
    }
}

/// Map AS7343 gain index (0-12) to the Gain enum
fn gain_from_u8(val: u8) -> Option<Gain> {
    Gain::from_raw(val)
}

impl<I: embedded_hal_async::i2c::I2c> SpectralSensorPort for As7343Adapter<I> {
    async fn read(&mut self) -> Result<SpectralReading, SpectralSensorError> {
        if !self.ready {
            return Err(SpectralSensorError::NotInitialized);
        }

        let delay = || Timer::after(Duration::from_millis(1));
        let data = self
            .sensor
            .measure_with_delay(delay, 1000)
            .await
            .map_err(|_| SpectralSensorError::ReadFailed)?;

        let timestamp_us = embassy_time::Instant::now().as_micros() as i64;
        let reading = self.channel_data_to_reading(&data, timestamp_us);

        if data.saturated {
            defmt::warn!("AS7343: measurement saturated");
        }

        Ok(reading)
    }

    fn sensor_id(&self) -> SensorId {
        self.sensor_id
    }

    async fn set_gain(&mut self, gain: u8) -> Result<(), SpectralSensorError> {
        let gain_enum = gain_from_u8(gain).ok_or(SpectralSensorError::InvalidConfig)?;
        self.sensor
            .set_gain(gain_enum)
            .await
            .map_err(|_| SpectralSensorError::I2cError)
    }

    async fn set_integration_time_ms(&mut self, ms: f32) -> Result<(), SpectralSensorError> {
        self.sensor
            .set_integration_time_ms(ms)
            .await
            .map_err(|_| SpectralSensorError::I2cError)
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}
