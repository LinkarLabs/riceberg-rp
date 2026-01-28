//! Storage port - abstraction for persisting and querying sensor data
//!
//! This trait allows the application to store and retrieve data without
//! knowing the specific storage implementation (EdgeDatabase, mock, etc.)

use crate::db_protocol::QueryFilter;
use crate::domain::SensorReading;
use heapless::Vec;

/// Maximum readings to return in a single query
pub const MAX_QUERY_RESULTS: usize = 32;

/// Error type for storage operations
#[derive(Clone, Copy, Debug, defmt::Format)]
pub enum StorageError {
    /// Storage not initialized
    NotInitialized,
    /// Failed to create/open database
    DatabaseError,
    /// Failed to create table
    TableError,
    /// Failed to insert reading
    InsertFailed,
    /// Failed to commit transaction
    CommitFailed,
    /// Failed to execute query
    QueryFailed,
    /// Schema not found
    SchemaNotFound,
    /// Storage is full
    StorageFull,
    /// Flash operation failed
    FlashError,
    /// Reading not found
    NotFound,
    /// Failed to delete reading
    DeleteFailed,
}

/// Storage statistics
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct StorageStats {
    /// Total number of readings stored
    pub total_readings: u32,
    /// Oldest reading timestamp (microseconds since boot)
    pub oldest_timestamp_us: i64,
    /// Newest reading timestamp (microseconds since boot)
    pub newest_timestamp_us: i64,
    /// Current snapshot ID (database version)
    pub snapshot_id: u64,
}

impl Default for StorageStats {
    fn default() -> Self {
        Self {
            total_readings: 0,
            oldest_timestamp_us: 0,
            newest_timestamp_us: 0,
            snapshot_id: 0,
        }
    }
}

/// Port for persisting and querying sensor readings
///
/// This trait abstracts the database, allowing different storage backends
/// (EdgeDatabase, mock storage for testing, etc.)
///
/// # Example Implementation
///
/// ```ignore
/// struct EdgeStorageAdapter<S: Storage> {
///     db: EdgeDatabase<S>,
///     table_id: u32,
/// }
///
/// impl<S: Storage + MaybeSendSync> StoragePort for EdgeStorageAdapter<S> {
///     async fn store(&mut self, reading: &SensorReading) -> Result<(), StorageError> {
///         // EdgeDatabase.save() handles commit + refresh internally!
///         self.db.save(self.table_id, EntityId(0), |row| {
///             row.set(0, Value::Int64(reading.timestamp_us))?;
///             row.set(1, Value::Float32(reading.temperature_c))?;
///             Ok(())
///         }).await.map_err(|_| StorageError::InsertFailed)?;
///         Ok(())
///     }
/// }
/// ```
pub trait StoragePort {
    /// Initialize the storage (create tables, etc.)
    ///
    /// This should be called once at startup to ensure the storage
    /// is ready for use.
    fn initialize(&mut self) -> impl core::future::Future<Output = Result<(), StorageError>>;

    /// Store a sensor reading
    ///
    /// The reading is persisted immediately. The implementation should
    /// handle transactions and commit automatically.
    fn store(&mut self, reading: &SensorReading)
        -> impl core::future::Future<Output = Result<(), StorageError>>;

    /// Get the latest N readings
    ///
    /// Returns readings in chronological order (oldest first).
    /// The `count` is capped at `MAX_QUERY_RESULTS`.
    fn get_latest(
        &mut self,
        count: u16,
    ) -> impl core::future::Future<Output = Result<Vec<SensorReading, MAX_QUERY_RESULTS>, StorageError>>;

    /// Get readings in a time range
    ///
    /// Returns readings where `start_us <= timestamp_us <= end_us`.
    /// Results are capped at `MAX_QUERY_RESULTS`.
    fn get_range(
        &mut self,
        start_us: i64,
        end_us: i64,
    ) -> impl core::future::Future<Output = Result<Vec<SensorReading, MAX_QUERY_RESULTS>, StorageError>>;

    /// Get storage statistics
    fn stats(&mut self) -> impl core::future::Future<Output = Result<StorageStats, StorageError>>;

    /// Get total count of readings
    fn count(&mut self) -> impl core::future::Future<Output = Result<u32, StorageError>>;

    /// Scan all readings with pagination
    ///
    /// Returns readings starting from `offset`, up to `MAX_QUERY_RESULTS`.
    /// Returns `(readings, total_count, has_more)`.
    fn scan_all(
        &mut self,
        offset: u32,
    ) -> impl core::future::Future<
        Output = Result<(Vec<SensorReading, MAX_QUERY_RESULTS>, u32, bool), StorageError>,
    >;

    /// Get a reading by its ID
    ///
    /// Returns `None` if the reading doesn't exist.
    fn get_by_id(
        &mut self,
        id: u64,
    ) -> impl core::future::Future<Output = Result<Option<SensorReading>, StorageError>>;

    /// Delete a reading by its ID
    ///
    /// Returns `true` if the reading was deleted, `false` if it didn't exist.
    fn delete(&mut self, id: u64) -> impl core::future::Future<Output = Result<bool, StorageError>>;

    /// Query readings with predicate filters
    ///
    /// Filters are combined with AND logic - all must match.
    /// Returns `(readings, total_count, has_more)`.
    ///
    /// # Arguments
    ///
    /// * `filters` - Slice of QueryFilter predicates to apply
    /// * `limit` - Maximum number of results to return (capped at MAX_QUERY_RESULTS)
    /// * `offset` - Number of results to skip (for pagination)
    fn query_filtered(
        &mut self,
        filters: &[QueryFilter],
        limit: Option<u16>,
        offset: Option<u32>,
    ) -> impl core::future::Future<
        Output = Result<(Vec<SensorReading, MAX_QUERY_RESULTS>, u32, bool), StorageError>,
    >;

    /// Run compaction to merge data files
    ///
    /// Returns `(files_before, files_after, rows_compacted, was_needed)`.
    fn compact(&mut self) -> impl core::future::Future<Output = Result<CompactionResult, StorageError>>;

    /// Expire old snapshots, keeping the last N
    ///
    /// Returns `(snapshots_expired, pages_freed)`.
    fn expire(&mut self, keep_last: u32) -> impl core::future::Future<Output = Result<ExpireResult, StorageError>>;

    /// Get storage capacity information
    ///
    /// Returns capacity info including total, allocated, and free pages.
    fn capacity(&self) -> CapacityResult;
}

/// Result of a compaction operation
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct CompactionResult {
    /// Number of data files before compaction
    pub files_before: u32,
    /// Number of data files after compaction
    pub files_after: u32,
    /// Number of rows compacted
    pub rows_compacted: u64,
    /// Whether compaction was needed
    pub was_needed: bool,
}

/// Result of a snapshot expiration operation
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct ExpireResult {
    /// Number of snapshots expired
    pub snapshots_expired: u32,
    /// Number of pages freed
    pub pages_freed: u32,
}

/// Storage capacity information
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct CapacityResult {
    /// Total pages in the database
    pub total_pages: u32,
    /// Pages currently allocated
    pub allocated_pages: u32,
    /// Pages available for use
    pub free_pages: u32,
}
