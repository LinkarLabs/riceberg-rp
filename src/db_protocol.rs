//! Shared protocol for RP2350 Riceberg database communication
//!
//! This module defines the message protocol used between the host CLI
//! and the RP2350 device for database operations.
//!
//! Messages are serialized using `postcard` with COBS encoding for framing.

// Prelude types needed for no_std compatibility
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use core::clone::Clone;
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use core::option::Option::{self, None, Some};

use serde::{Deserialize, Serialize};

#[cfg(feature = "std")]
use std::string::{String, ToString};
#[cfg(feature = "std")]
use std::vec::Vec;

/// Maximum number of readings per response
pub const MAX_READINGS_PER_RESPONSE: usize = 32;

/// Maximum number of filters in a query
pub const MAX_QUERY_FILTERS: usize = 4;

// ============================================================================
// Filter Types for Predicate-Based Queries
// ============================================================================

/// Comparison operator for filter predicates
///
/// Matches riceberg_core::CompareOp for conversion
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterOp {
    /// Equal
    Eq,
    /// Not equal
    Ne,
    /// Less than
    Lt,
    /// Less than or equal
    Le,
    /// Greater than
    Gt,
    /// Greater than or equal
    Ge,
}

/// Fields available for filtering (mapped to schema indices)
///
/// Schema layout:
/// - 0: id (Int64)
/// - 1: timestamp_us (Int64)
/// - 2: temperature_c (Float32)
/// - 3: sensor_id (String)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterField {
    /// Reading ID (field index 0)
    Id,
    /// Timestamp in microseconds (field index 1)
    TimestampUs,
    /// Temperature in Celsius (field index 2)
    TemperatureC,
    /// Sensor identifier (field index 3)
    SensorId,
}

impl FilterField {
    /// Convert to schema field index
    pub fn to_field_idx(self) -> usize {
        match self {
            FilterField::Id => 0,
            FilterField::TimestampUs => 1,
            FilterField::TemperatureC => 2,
            FilterField::SensorId => 3,
        }
    }
}

/// Type-safe filter values
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FilterValue {
    /// 64-bit integer (for id, timestamp)
    Int64(i64),
    /// 32-bit float (for temperature)
    Float32(f32),
    /// String value (for sensor_id)
    #[cfg(not(feature = "std"))]
    String(heapless::String<16>),
    #[cfg(feature = "std")]
    String(String),
}

impl FilterValue {
    /// Create an Int64 filter value
    pub fn int64(v: i64) -> Self {
        FilterValue::Int64(v)
    }

    /// Create a Float32 filter value
    pub fn float32(v: f32) -> Self {
        FilterValue::Float32(v)
    }

    /// Create a String filter value
    #[cfg(not(feature = "std"))]
    pub fn string(s: &str) -> Option<Self> {
        heapless::String::try_from(s).ok().map(FilterValue::String)
    }

    /// Create a String filter value (std version)
    #[cfg(feature = "std")]
    pub fn string(s: &str) -> Option<Self> {
        Some(FilterValue::String(s.to_string()))
    }
}

/// A single filter condition for queries
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryFilter {
    /// Which field to filter on
    pub field: FilterField,
    /// Comparison operator
    pub op: FilterOp,
    /// Value to compare against
    pub value: FilterValue,
}

impl QueryFilter {
    /// Create a new query filter
    pub fn new(field: FilterField, op: FilterOp, value: FilterValue) -> Self {
        Self { field, op, value }
    }

    /// Convenience: field == value
    pub fn eq(field: FilterField, value: FilterValue) -> Self {
        Self::new(field, FilterOp::Eq, value)
    }

    /// Convenience: field != value
    pub fn ne(field: FilterField, value: FilterValue) -> Self {
        Self::new(field, FilterOp::Ne, value)
    }

    /// Convenience: field < value
    pub fn lt(field: FilterField, value: FilterValue) -> Self {
        Self::new(field, FilterOp::Lt, value)
    }

    /// Convenience: field <= value
    pub fn le(field: FilterField, value: FilterValue) -> Self {
        Self::new(field, FilterOp::Le, value)
    }

    /// Convenience: field > value
    pub fn gt(field: FilterField, value: FilterValue) -> Self {
        Self::new(field, FilterOp::Gt, value)
    }

    /// Convenience: field >= value
    pub fn ge(field: FilterField, value: FilterValue) -> Self {
        Self::new(field, FilterOp::Ge, value)
    }
}

/// Command sent from host to device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DbCommand {
    /// Get latest N readings
    QueryLatest { count: u16 },

    /// Get readings in time range (microseconds since boot)
    QueryRange { start_us: i64, end_us: i64 },

    /// Get database statistics
    Stats,

    /// Scan all readings (returns first batch)
    ScanAll { offset: u32 },

    /// Get system diagnostics (for debugging without RTT probe)
    Diagnostics,

    /// Delete a reading by ID
    Delete { id: u64 },

    /// Get a single reading by ID
    GetById { id: u64 },

    /// Query with predicate filters (max 4 filters, AND logic)
    #[cfg(not(feature = "std"))]
    QueryFiltered {
        /// Filter predicates (all must match)
        filters: heapless::Vec<QueryFilter, MAX_QUERY_FILTERS>,
        /// Maximum number of results to return
        limit: Option<u16>,
        /// Number of results to skip (for pagination)
        offset: Option<u32>,
    },
    /// Query with predicate filters (max 4 filters, AND logic)
    #[cfg(feature = "std")]
    QueryFiltered {
        /// Filter predicates (all must match)
        filters: Vec<QueryFilter>,
        /// Maximum number of results to return
        limit: Option<u16>,
        /// Number of results to skip (for pagination)
        offset: Option<u32>,
    },

    /// Run compaction on the database to merge data files
    Compact,

    /// Expire old snapshots, keeping the last N
    Expire {
        /// Minimum number of snapshots to keep
        keep_last: u32,
    },

    /// Get storage capacity information
    Capacity,
}

/// Temperature reading (simplified for COBS)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemperatureReading {
    /// Unique reading ID (auto-generated)
    pub id: u64,
    /// Timestamp in microseconds since boot
    pub timestamp_us: i64,
    /// Temperature in Celsius
    pub temperature_c: f32,
    /// Sensor identifier
    #[cfg(not(feature = "std"))]
    pub sensor_id: heapless::String<16>,
    #[cfg(feature = "std")]
    pub sensor_id: String,
}

impl TemperatureReading {
    /// Create a new reading
    #[cfg(not(feature = "std"))]
    pub fn new(id: u64, timestamp_us: i64, temperature_c: f32, sensor_id: &str) -> Option<Self> {
        let sensor_id_str = heapless::String::try_from(sensor_id).ok()?;

        Some(Self {
            id,
            timestamp_us,
            temperature_c,
            sensor_id: sensor_id_str,
        })
    }

    /// Create a new reading (std version)
    #[cfg(feature = "std")]
    pub fn new(id: u64, timestamp_us: i64, temperature_c: f32, sensor_id: &str) -> Option<Self> {
        Some(Self {
            id,
            timestamp_us,
            temperature_c,
            sensor_id: sensor_id.to_string(),
        })
    }
}

/// Response sent from device to host
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DbResponse {
    /// Success
    Ok,

    /// Error message
    #[cfg(not(feature = "std"))]
    Error { message: heapless::String<128> },
    #[cfg(feature = "std")]
    Error { message: String },

    /// Temperature readings (using Vec instead of fixed array)
    #[cfg(not(feature = "std"))]
    Readings {
        data: heapless::Vec<TemperatureReading, MAX_READINGS_PER_RESPONSE>,
        total: u32,
        has_more: bool,
    },
    #[cfg(feature = "std")]
    Readings {
        data: Vec<TemperatureReading>,
        total: u32,
        has_more: bool,
    },

    /// Single reading response (for GetById)
    SingleReading {
        reading: Option<TemperatureReading>,
    },

    /// Deletion result
    Deleted {
        /// ID of deleted reading
        id: u64,
        /// Whether deletion was successful
        success: bool,
    },

    /// Database statistics
    Stats {
        /// Total number of readings in database
        total_readings: u32,
        /// Oldest timestamp (microseconds)
        oldest_timestamp_us: i64,
        /// Newest timestamp (microseconds)
        newest_timestamp_us: i64,
        /// Current snapshot ID
        snapshot_id: u64,
    },

    /// System diagnostics
    Diagnostics {
        /// Whether DB_READY flag is set
        db_ready: bool,
        /// Whether sensor task is running
        sensor_task_running: bool,
        /// Number of readings sent by sensor task
        sensor_readings_sent: u32,
        /// Number of readings received by DB task
        sensor_readings_received: u32,
        /// Number of successful DB writes
        db_writes_success: u32,
        /// Number of failed DB inserts
        db_insert_failed: u32,
        /// Number of failed DB commits
        db_commit_failed: u32,
        /// Last raw ADC value from temperature sensor
        last_adc_value: u16,
        /// Current uptime in milliseconds
        uptime_ms: u64,
    },

    /// Compaction result
    Compacted {
        /// Number of data files before compaction
        files_before: u32,
        /// Number of data files after compaction
        files_after: u32,
        /// Number of rows compacted
        rows_compacted: u64,
        /// Whether compaction was needed
        was_needed: bool,
    },

    /// Snapshot expiration result
    Expired {
        /// Number of snapshots expired
        snapshots_expired: u32,
        /// Number of pages freed
        pages_freed: u32,
    },

    /// Storage capacity information
    Capacity {
        /// Total pages in the database
        total_pages: u32,
        /// Pages currently allocated
        allocated_pages: u32,
        /// Pages available for use
        free_pages: u32,
        /// Approximate storage used in bytes
        used_bytes: u64,
        /// Approximate storage free in bytes
        free_bytes: u64,
    },
}

impl DbResponse {
    /// Create error response
    #[cfg(not(feature = "std"))]
    pub fn error(msg: &str) -> Self {
        let message = heapless::String::try_from(msg).unwrap_or_else(|_| {
            // If message too long, truncate
            let truncated = &msg[..msg.len().min(128)];
            heapless::String::try_from(truncated).unwrap_or_default()
        });

        Self::Error { message }
    }

    /// Create error response (std version)
    #[cfg(feature = "std")]
    pub fn error(msg: &str) -> Self {
        Self::Error {
            message: msg.to_string(),
        }
    }

    /// Create readings response
    #[cfg(not(feature = "std"))]
    pub fn readings(readings: &[TemperatureReading], total: u32, has_more: bool) -> Self {
        let mut data = heapless::Vec::new();
        for reading in readings.iter().take(MAX_READINGS_PER_RESPONSE) {
            let _ = data.push(reading.clone());
        }

        Self::Readings {
            data,
            total,
            has_more,
        }
    }

    /// Create readings response (std version)
    #[cfg(feature = "std")]
    pub fn readings(readings: &[TemperatureReading], total: u32, has_more: bool) -> Self {
        Self::Readings {
            data: readings.to_vec(),
            total,
            has_more,
        }
    }
}

impl DbCommand {
    /// Create query latest command
    pub fn query_latest(count: u16) -> Self {
        Self::QueryLatest { count }
    }

    /// Create query range command
    pub fn query_range(start_us: i64, end_us: i64) -> Self {
        Self::QueryRange { start_us, end_us }
    }

    /// Create stats command
    pub fn stats() -> Self {
        Self::Stats
    }

    /// Create scan all command
    pub fn scan_all(offset: u32) -> Self {
        Self::ScanAll { offset }
    }

    /// Create diagnostics command
    pub fn diagnostics() -> Self {
        Self::Diagnostics
    }

    /// Create delete command
    pub fn delete(id: u64) -> Self {
        Self::Delete { id }
    }

    /// Create get by ID command
    pub fn get_by_id(id: u64) -> Self {
        Self::GetById { id }
    }

    /// Create query filtered command (no_std version)
    #[cfg(not(feature = "std"))]
    pub fn query_filtered(
        filters: heapless::Vec<QueryFilter, MAX_QUERY_FILTERS>,
        limit: Option<u16>,
        offset: Option<u32>,
    ) -> Self {
        Self::QueryFiltered { filters, limit, offset }
    }

    /// Create query filtered command (std version)
    #[cfg(feature = "std")]
    pub fn query_filtered(
        filters: Vec<QueryFilter>,
        limit: Option<u16>,
        offset: Option<u32>,
    ) -> Self {
        Self::QueryFiltered { filters, limit, offset }
    }

    /// Create compact command
    pub fn compact() -> Self {
        Self::Compact
    }

    /// Create expire command
    pub fn expire(keep_last: u32) -> Self {
        Self::Expire { keep_last }
    }

    /// Create capacity command
    pub fn capacity() -> Self {
        Self::Capacity
    }
}
