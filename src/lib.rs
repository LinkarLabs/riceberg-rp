//! RP2350 Riceberg Database Library
//!
//! This library provides a hexagonal architecture for embedded sensor applications
//! using riceberg-edge for flash-based storage.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     Domain Layer                                 │
//! │  - SensorReading entity                                          │
//! │  - TemperatureCalibration service                               │
//! └─────────────────────────────────────────────────────────────────┘
//!                               │
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     Ports (Traits)                               │
//! │  - SensorPort: read sensor data                                 │
//! │  - StoragePort: persist/query readings                          │
//! │  - CommunicationPort: host communication                        │
//! └─────────────────────────────────────────────────────────────────┘
//!                               │
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     Adapters                                     │
//! │  - Rp2350TempSensor: ADC temperature sensor                     │
//! │  - EdgeStorageAdapter: riceberg-edge database                   │
//! │  - UsbCdcAdapter: USB CDC serial                                │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Key Benefits
//!
//! - **EdgeDatabase handles commit+refresh internally** - No more manual `notify_commit()`
//! - **Testable** - Ports allow mocking sensors, storage, communication
//! - **Extensible** - Easy to add I2C/SPI sensors by implementing SensorPort

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

// ============================================================================
// Protocol (shared between host and device)
// ============================================================================

pub mod db_protocol;

pub use db_protocol::{
    DbCommand, DbResponse, FilterField, FilterOp, FilterValue, QueryFilter,
    SpectralReadingProtocol, TemperatureReading, MAX_QUERY_FILTERS, MAX_READINGS_PER_RESPONSE,
    MAX_SPECTRAL_READINGS_PER_RESPONSE,
};

// ============================================================================
// Hexagonal Architecture (embedded only)
// ============================================================================

/// Domain layer - pure business logic
#[cfg(not(feature = "std"))]
pub mod domain;

/// Ports - traits defining boundaries
#[cfg(not(feature = "std"))]
pub mod ports;

/// Adapters - concrete implementations
#[cfg(not(feature = "std"))]
pub mod adapters;

// Re-export key domain types
#[cfg(not(feature = "std"))]
pub use domain::{SensorId, SensorReading, SpectralReading, TemperatureCalibration};

// Re-export key port traits
#[cfg(not(feature = "std"))]
pub use ports::{CommunicationPort, SensorPort, SpectralSensorPort, StoragePort};

// Re-export adapters
#[cfg(not(feature = "std"))]
pub use adapters::{As7343Adapter, EdgeStorageAdapter, Rp2350TempSensor, UsbCdcAdapter};
