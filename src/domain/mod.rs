//! Domain layer - pure business logic independent of infrastructure
//!
//! This module contains the core domain entities and services that
//! represent the business logic of the sensor application.

pub mod calibration;
pub mod reading;

pub use calibration::TemperatureCalibration;
pub use reading::{SensorId, SensorReading};
