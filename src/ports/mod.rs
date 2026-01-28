//! Ports (interfaces) defining the boundaries of the application
//!
//! Ports are traits that define how the domain interacts with external systems.
//! They allow the domain to remain independent of specific implementations.
//!
//! # Hexagonal Architecture
//!
//! In hexagonal architecture, ports define the "holes" in the hexagon where
//! adapters plug in:
//!
//! - **SensorPort**: How we read sensor data (ADC, I2C, SPI, mock)
//! - **StoragePort**: How we persist and query data (EdgeDatabase, mock)
//! - **CommunicationPort**: How we communicate with hosts (USB CDC, UART, WiFi)

pub mod communication;
pub mod sensor;
pub mod storage;

pub use communication::CommunicationPort;
pub use sensor::{SensorConfig, SensorError, SensorPort};
pub use storage::{StorageError, StoragePort, StorageStats, MAX_QUERY_RESULTS};
