//! Sensor reading domain entity
//!
//! This module defines the core domain entity for sensor readings.
//! It has no knowledge of how readings are stored or transmitted.

/// A sensor reading from the domain perspective.
///
/// This is the core domain entity representing a temperature measurement
/// at a specific point in time from a specific sensor.
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct SensorReading {
    /// Unique reading ID (0 means not yet assigned)
    pub id: u64,
    /// Timestamp in microseconds since boot
    pub timestamp_us: i64,
    /// Temperature in Celsius
    pub temperature_c: f32,
    /// Sensor identifier
    pub sensor_id: SensorId,
}

impl SensorReading {
    /// Create a new sensor reading (ID will be assigned on storage)
    pub const fn new(timestamp_us: i64, temperature_c: f32, sensor_id: SensorId) -> Self {
        Self {
            id: 0, // Will be assigned when stored
            timestamp_us,
            temperature_c,
            sensor_id,
        }
    }

    /// Create a sensor reading with a known ID (from database)
    pub const fn with_id(
        id: u64,
        timestamp_us: i64,
        temperature_c: f32,
        sensor_id: SensorId,
    ) -> Self {
        Self {
            id,
            timestamp_us,
            temperature_c,
            sensor_id,
        }
    }
}

/// Sensor identifier (memory-efficient representation)
///
/// Uses a single byte to identify sensors, with predefined constants
/// for common sensor types. This avoids heap allocation for sensor names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, defmt::Format)]
pub struct SensorId(pub u8);

impl SensorId {
    /// Onboard temperature sensor (RP2350 internal)
    pub const ONBOARD: SensorId = SensorId(0);

    /// External sensor slot 1 (e.g., I2C sensor)
    pub const EXTERNAL_1: SensorId = SensorId(1);

    /// External sensor slot 2 (e.g., SPI sensor)
    pub const EXTERNAL_2: SensorId = SensorId(2);

    /// External sensor slot 3
    pub const EXTERNAL_3: SensorId = SensorId(3);

    /// Test/mock sensor
    pub const TEST: SensorId = SensorId(255);

    /// AS7343 14-channel spectral sensor
    pub const AS7343: SensorId = SensorId(10);

    /// Create a new sensor ID from a raw value
    pub const fn new(id: u8) -> Self {
        Self(id)
    }

    /// Get the string representation of this sensor ID
    pub const fn as_str(&self) -> &'static str {
        match self.0 {
            0 => "onboard",
            1 => "external_1",
            2 => "external_2",
            3 => "external_3",
            10 => "as7343",
            255 => "test",
            _ => "unknown",
        }
    }

    /// Get the raw ID value
    pub const fn value(&self) -> u8 {
        self.0
    }
}
