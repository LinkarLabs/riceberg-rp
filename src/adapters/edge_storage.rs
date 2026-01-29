//! EdgeDatabase storage adapter
//!
//! This adapter implements the StoragePort trait using riceberg-edge's
//! EdgeDatabase for flash-based storage.

extern crate alloc;

use alloc::vec::Vec as AllocVec;
use heapless::Vec;
use riceberg_core::{FieldType, MaybeSendSync, SchemaBuilder, Storage, Value};
use riceberg_edge::{CompareOp, EdgeDatabase, EntityId, PredicateValue, QueryBuilder};

use riceberg_core::CompactionConfig;

use crate::db_protocol::{FilterOp, FilterValue, QueryFilter, SpectralReadingProtocol};
use crate::domain::{SensorId, SensorReading, SpectralReading};
use crate::ports::storage::{
    CapacityResult, CompactionResult, ExpireResult, StorageError, StoragePort, StorageStats,
    MAX_QUERY_RESULTS,
};

/// EdgeDatabase storage adapter
///
/// Wraps riceberg-edge's EdgeDatabase to implement the StoragePort trait.
/// EdgeDatabase handles the critical `commit() + refresh()` cycle internally,
/// eliminating the manual `notify_commit()` footgun.
pub struct EdgeStorageAdapter<S: Storage + MaybeSendSync> {
    /// EdgeDatabase instance (handles transactions internally)
    db: EdgeDatabase<S>,
    /// Cached table ID for the temperature table
    table_id: Option<u32>,
    /// Cached table ID for the spectral table
    spectral_table_id: Option<u32>,
    /// Next ID counter for readings (auto-increment)
    next_id: u64,
    /// Next ID counter for spectral readings (auto-increment)
    next_spectral_id: u64,
}

impl<S: Storage + MaybeSendSync> EdgeStorageAdapter<S> {
    /// Create a new EdgeStorageAdapter wrapping an EdgeDatabase
    ///
    /// Note: Call `initialize()` after construction to create the table.
    pub fn new(db: EdgeDatabase<S>) -> Self {
        Self {
            db,
            table_id: None,
            spectral_table_id: None,
            next_id: 1,
            next_spectral_id: 1,
        }
    }

    /// Create adapter from storage with database creation
    ///
    /// This creates a new EdgeDatabase from raw storage.
    pub async fn create(
        storage: S,
        config: riceberg_core::DatabaseConfig,
    ) -> Result<Self, StorageError> {
        let db = EdgeDatabase::create(storage, config)
            .await
            .map_err(|_| StorageError::DatabaseError)?;
        Ok(Self::new(db))
    }

    /// Get the underlying EdgeDatabase (for advanced operations)
    pub fn database(&self) -> &EdgeDatabase<S> {
        &self.db
    }

    /// Get mutable access to the underlying EdgeDatabase
    pub fn database_mut(&mut self) -> &mut EdgeDatabase<S> {
        &mut self.db
    }

    /// Get the table ID (after initialization)
    pub fn table_id(&self) -> Option<u32> {
        self.table_id
    }

    /// Get current snapshot ID
    pub fn current_snapshot_id(&self) -> u64 {
        self.db.current_snapshot_id()
    }

    /// Create the temperature readings schema
    ///
    /// Schema fields:
    /// - id (Int64): Unique reading ID (auto-generated)
    /// - timestamp_us (Int64): Microseconds since boot
    /// - temperature_c (Float32): Temperature in Celsius
    /// - sensor_id (String): Sensor identifier
    fn create_schema() -> riceberg_core::Schema {
        SchemaBuilder::new(1)
            .required("id", FieldType::Int64)
            .unwrap()
            .required("timestamp_us", FieldType::Int64)
            .unwrap()
            .required("temperature_c", FieldType::Float32)
            .unwrap()
            .optional("sensor_id", FieldType::String)
            .unwrap()
            .build()
    }

    // ========================================================================
    // Spectral Table Support
    // ========================================================================

    /// Create the spectral readings schema
    ///
    /// Schema fields (16 total - matches riceberg memory-minimal MAX_INLINE_FIELDS):
    /// - 0: timestamp_us (Int64) - Microseconds since boot
    /// - 1-12: spectral channels (Int64) - F1, F2, FZ, F3, F4, F5, FY, FXL, F6, F7, F8, NIR
    /// - 13: clear (Int64) - Clear channel
    /// - 14: flicker (Int64) - Flicker channel
    /// - 15: metadata (Int64) - low byte = gain, bit 8 = saturated
    ///
    /// Note: `id` is not stored as a schema field. The entity ID is passed
    /// separately via `EntityId` when using `EdgeDatabase::save()`.
    fn create_spectral_schema() -> riceberg_core::Schema {
        SchemaBuilder::new(1)
            .required("timestamp_us", FieldType::Int64)
            .unwrap()
            .required("f1_405nm", FieldType::Int64)
            .unwrap()
            .required("f2_425nm", FieldType::Int64)
            .unwrap()
            .required("fz_450nm", FieldType::Int64)
            .unwrap()
            .required("f3_475nm", FieldType::Int64)
            .unwrap()
            .required("f4_515nm", FieldType::Int64)
            .unwrap()
            .required("f5_550nm", FieldType::Int64)
            .unwrap()
            .required("fy_555nm", FieldType::Int64)
            .unwrap()
            .required("fxl_600nm", FieldType::Int64)
            .unwrap()
            .required("f6_640nm", FieldType::Int64)
            .unwrap()
            .required("f7_690nm", FieldType::Int64)
            .unwrap()
            .required("f8_745nm", FieldType::Int64)
            .unwrap()
            .required("nir_855nm", FieldType::Int64)
            .unwrap()
            .required("clear", FieldType::Int64)
            .unwrap()
            .required("flicker", FieldType::Int64)
            .unwrap()
            .required("metadata", FieldType::Int64)
            .unwrap()
            .build()
    }

    /// Pack gain and saturated flag into a single i64 metadata value
    fn pack_metadata(gain: u8, saturated: bool) -> i64 {
        gain as i64 | if saturated { 1 << 8 } else { 0 }
    }

    /// Unpack gain and saturated flag from metadata value
    fn unpack_metadata(metadata: i64) -> (u8, bool) {
        let gain = (metadata & 0xFF) as u8;
        let saturated = (metadata & (1 << 8)) != 0;
        (gain, saturated)
    }

    /// Initialize the spectral table
    pub async fn initialize_spectral(&mut self) -> Result<(), StorageError> {
        let schema = Self::create_spectral_schema();
        let table_id = self
            .db
            .register_table("spectral", schema)
            .await
            .map_err(|_| StorageError::TableError)?;
        self.spectral_table_id = Some(table_id);
        Ok(())
    }

    /// Get the spectral table ID (after initialization)
    pub fn spectral_table_id(&self) -> Option<u32> {
        self.spectral_table_id
    }

    /// Store a spectral reading
    pub async fn store_spectral(
        &mut self,
        reading: &SpectralReading,
    ) -> Result<(), StorageError> {
        let table_id = self
            .spectral_table_id
            .ok_or(StorageError::NotInitialized)?;

        let id = self.next_spectral_id;
        self.next_spectral_id += 1;

        let channels = reading.all_channels();
        let metadata = Self::pack_metadata(reading.gain, reading.saturated);

        self.db
            .save(table_id, EntityId(id as i64), |row| {
                row.set(0, Value::Int64(reading.timestamp_us))?;
                for i in 0..14 {
                    row.set(1 + i, Value::Int64(channels[i] as i64))?;
                }
                row.set(15, Value::Int64(metadata))?;
                Ok(())
            })
            .await
            .map_err(|_| StorageError::InsertFailed)?;

        Ok(())
    }

    /// Get the latest N spectral readings
    pub async fn get_spectral_latest(
        &mut self,
        count: u16,
    ) -> Result<AllocVec<SpectralReadingProtocol>, StorageError> {
        let table_id = self
            .spectral_table_id
            .ok_or(StorageError::NotInitialized)?;
        let schema = self
            .db
            .schema(table_id)
            .ok_or(StorageError::SchemaNotFound)?
            .clone();

        let scan = riceberg_core::ScanBuilder::new(self.db.storage(), table_id, &schema)
            .await
            .map_err(|_| StorageError::QueryFailed)?
            .build()
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        let mut results = riceberg_core::ScanResults::new(scan);
        let mut all_readings = AllocVec::new();

        while let Ok(Some(row)) = results.next_row().await {
            if let Some(reading) = Self::row_to_spectral_protocol(&row) {
                all_readings.push(reading);
            }
        }

        let count = count as usize;
        let start_idx = all_readings.len().saturating_sub(count);
        Ok(all_readings.split_off(start_idx))
    }

    /// Get spectral readings in a time range
    pub async fn get_spectral_range(
        &mut self,
        start_us: i64,
        end_us: i64,
    ) -> Result<AllocVec<SpectralReadingProtocol>, StorageError> {
        let table_id = self
            .spectral_table_id
            .ok_or(StorageError::NotInitialized)?;
        let schema = self
            .db
            .schema(table_id)
            .ok_or(StorageError::SchemaNotFound)?
            .clone();

        let scan = riceberg_core::ScanBuilder::new(self.db.storage(), table_id, &schema)
            .await
            .map_err(|_| StorageError::QueryFailed)?
            .build()
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        let mut results = riceberg_core::ScanResults::new(scan);
        let mut output = AllocVec::new();

        while let Ok(Some(row)) = results.next_row().await {
            if let Some(reading) = Self::row_to_spectral_protocol(&row) {
                if reading.timestamp_us >= start_us && reading.timestamp_us <= end_us {
                    output.push(reading);
                }
            }
        }

        Ok(output)
    }

    /// Get spectral table statistics
    pub async fn spectral_stats(&mut self) -> Result<StorageStats, StorageError> {
        let table_id = self
            .spectral_table_id
            .ok_or(StorageError::NotInitialized)?;
        let schema = self
            .db
            .schema(table_id)
            .ok_or(StorageError::SchemaNotFound)?
            .clone();

        let scan = riceberg_core::ScanBuilder::new(self.db.storage(), table_id, &schema)
            .await
            .map_err(|_| StorageError::QueryFailed)?
            .build()
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        let mut results = riceberg_core::ScanResults::new(scan);
        let mut count = 0u32;
        let mut oldest = i64::MAX;
        let mut newest = i64::MIN;

        while let Ok(Some(row)) = results.next_row().await {
            count += 1;
            if let Ok(Some(Value::Int64(ts))) = row.get(0) {
                oldest = oldest.min(ts);
                newest = newest.max(ts);
            }
        }

        Ok(StorageStats {
            total_readings: count,
            oldest_timestamp_us: if count > 0 { oldest } else { 0 },
            newest_timestamp_us: if count > 0 { newest } else { 0 },
            snapshot_id: self.db.current_snapshot_id(),
        })
    }

    /// Convert a database row to a SpectralReadingProtocol
    ///
    /// Schema layout (16 fields, no id field):
    /// - 0: timestamp_us, 1-12: spectral channels, 13: clear, 14: flicker, 15: metadata
    fn row_to_spectral_protocol(
        row: &riceberg_core::ScannedRow<'_>,
    ) -> Option<SpectralReadingProtocol> {
        let timestamp_us = match row.get(0) {
            Ok(Some(Value::Int64(v))) => v,
            _ => return None,
        };

        let mut channels = [0u16; 14];
        // Fields 1-12: spectral channels (F1..NIR)
        for i in 0..12 {
            channels[i] = match row.get(1 + i) {
                Ok(Some(Value::Int64(v))) => v as u16,
                _ => 0,
            };
        }
        // Field 13: clear
        channels[12] = match row.get(13) {
            Ok(Some(Value::Int64(v))) => v as u16,
            _ => 0,
        };
        // Field 14: flicker
        channels[13] = match row.get(14) {
            Ok(Some(Value::Int64(v))) => v as u16,
            _ => 0,
        };

        // Field 15: packed metadata (gain + saturated)
        let (gain, saturated) = match row.get(15) {
            Ok(Some(Value::Int64(v))) => Self::unpack_metadata(v),
            _ => (0, false),
        };

        Some(SpectralReadingProtocol::new(
            0,
            timestamp_us,
            channels,
            gain,
            saturated,
        ))
    }

    /// Convert a database row to a SensorReading
    fn row_to_reading(row: &riceberg_core::ScannedRow<'_>) -> Option<SensorReading> {
        // Field 0: id
        let id = match row.get(0) {
            Ok(Some(Value::Int64(id))) => id as u64,
            _ => return None,
        };

        // Field 1: timestamp_us
        let timestamp_us = match row.get(1) {
            Ok(Some(Value::Int64(ts))) => ts,
            _ => return None,
        };

        // Field 2: temperature_c
        let temperature_c = match row.get(2) {
            Ok(Some(Value::Float32(temp))) => temp,
            _ => return None,
        };

        // Field 3: sensor_id (optional)
        let sensor_id = match row.get(3) {
            Ok(Some(Value::String(s))) => {
                let s = core::str::from_utf8(s).unwrap_or("onboard");
                match s {
                    "onboard" => SensorId::ONBOARD,
                    "external_1" => SensorId::EXTERNAL_1,
                    "external_2" => SensorId::EXTERNAL_2,
                    "external_3" => SensorId::EXTERNAL_3,
                    "test" | "TEST" => SensorId::TEST,
                    _ => SensorId::ONBOARD,
                }
            }
            _ => SensorId::ONBOARD,
        };

        Some(SensorReading::with_id(id, timestamp_us, temperature_c, sensor_id))
    }
}

impl<S: Storage + MaybeSendSync> StoragePort for EdgeStorageAdapter<S> {
    async fn initialize(&mut self) -> Result<(), StorageError> {
        let schema = Self::create_schema();

        // EdgeDatabase.register_table() handles table creation and schema caching
        let table_id = self
            .db
            .register_table("temperature", schema)
            .await
            .map_err(|_| StorageError::TableError)?;

        self.table_id = Some(table_id);
        Ok(())
    }

    async fn store(&mut self, reading: &SensorReading) -> Result<(), StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;

        // Assign an ID to this reading
        let id = self.next_id;
        self.next_id += 1;

        // EdgeDatabase.save() handles:
        // 1. begin_write()
        // 2. insert()
        // 3. commit()
        // 4. refresh() <- This is what notify_commit() was doing!
        self.db
            .save(table_id, EntityId(id as i64), |row| {
                row.set(0, Value::Int64(id as i64))?;
                row.set(1, Value::Int64(reading.timestamp_us))?;
                row.set(2, Value::Float32(reading.temperature_c))?;
                row.set(3, Value::String(reading.sensor_id.as_str().as_bytes()))?;
                Ok(())
            })
            .await
            .map_err(|_| StorageError::InsertFailed)?;

        Ok(())
    }

    async fn get_latest(
        &mut self,
        count: u16,
    ) -> Result<Vec<SensorReading, MAX_QUERY_RESULTS>, StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;
        let schema = self
            .db
            .schema(table_id)
            .ok_or(StorageError::SchemaNotFound)?
            .clone();

        // Scan all readings, keep the last N
        let scan = riceberg_core::ScanBuilder::new(self.db.storage(), table_id, &schema)
            .await
            .map_err(|_| StorageError::QueryFailed)?
            .build()
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        let mut results = riceberg_core::ScanResults::new(scan);
        let mut all_readings = AllocVec::new();

        while let Ok(Some(row)) = results.next_row().await {
            if let Some(reading) = Self::row_to_reading(&row) {
                all_readings.push(reading);
            }
        }

        // Take last N readings
        let count = count as usize;
        let start_idx = all_readings.len().saturating_sub(count);
        let mut output = Vec::new();

        for reading in all_readings[start_idx..].iter() {
            let _ = output.push(*reading);
        }

        Ok(output)
    }

    async fn get_range(
        &mut self,
        start_us: i64,
        end_us: i64,
    ) -> Result<Vec<SensorReading, MAX_QUERY_RESULTS>, StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;
        let schema = self
            .db
            .schema(table_id)
            .ok_or(StorageError::SchemaNotFound)?
            .clone();

        let scan = riceberg_core::ScanBuilder::new(self.db.storage(), table_id, &schema)
            .await
            .map_err(|_| StorageError::QueryFailed)?
            .build()
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        let mut results = riceberg_core::ScanResults::new(scan);
        let mut output = Vec::new();

        while let Ok(Some(row)) = results.next_row().await {
            if let Some(reading) = Self::row_to_reading(&row) {
                if reading.timestamp_us >= start_us && reading.timestamp_us <= end_us {
                    if output.push(reading).is_err() {
                        break; // Output full
                    }
                }
            }
        }

        Ok(output)
    }

    async fn stats(&mut self) -> Result<StorageStats, StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;
        let schema = self
            .db
            .schema(table_id)
            .ok_or(StorageError::SchemaNotFound)?
            .clone();

        let scan = riceberg_core::ScanBuilder::new(self.db.storage(), table_id, &schema)
            .await
            .map_err(|_| StorageError::QueryFailed)?
            .build()
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        let mut results = riceberg_core::ScanResults::new(scan);
        let mut count = 0u32;
        let mut oldest = i64::MAX;
        let mut newest = i64::MIN;

        while let Ok(Some(row)) = results.next_row().await {
            count += 1;
            if let Ok(Some(Value::Int64(ts))) = row.get(0) {
                oldest = oldest.min(ts);
                newest = newest.max(ts);
            }
        }

        Ok(StorageStats {
            total_readings: count,
            oldest_timestamp_us: if count > 0 { oldest } else { 0 },
            newest_timestamp_us: if count > 0 { newest } else { 0 },
            snapshot_id: self.db.current_snapshot_id(),
        })
    }

    async fn count(&mut self) -> Result<u32, StorageError> {
        let stats = self.stats().await?;
        Ok(stats.total_readings)
    }

    async fn scan_all(
        &mut self,
        offset: u32,
    ) -> Result<(Vec<SensorReading, MAX_QUERY_RESULTS>, u32, bool), StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;
        let schema = self
            .db
            .schema(table_id)
            .ok_or(StorageError::SchemaNotFound)?
            .clone();

        let scan = riceberg_core::ScanBuilder::new(self.db.storage(), table_id, &schema)
            .await
            .map_err(|_| StorageError::QueryFailed)?
            .build()
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        let mut results = riceberg_core::ScanResults::new(scan);
        let mut all_readings = AllocVec::new();

        while let Ok(Some(row)) = results.next_row().await {
            if let Some(reading) = Self::row_to_reading(&row) {
                all_readings.push(reading);
            }
        }

        let total = all_readings.len() as u32;
        let offset = offset as usize;
        let start = offset.min(all_readings.len());
        let end = (start + MAX_QUERY_RESULTS).min(all_readings.len());
        let has_more = end < all_readings.len();

        let mut output = Vec::new();
        for reading in all_readings[start..end].iter() {
            let _ = output.push(*reading);
        }

        Ok((output, total, has_more))
    }

    async fn get_by_id(&mut self, id: u64) -> Result<Option<SensorReading>, StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;
        let schema = self
            .db
            .schema(table_id)
            .ok_or(StorageError::SchemaNotFound)?
            .clone();

        // Scan for the reading with matching ID
        let scan = riceberg_core::ScanBuilder::new(self.db.storage(), table_id, &schema)
            .await
            .map_err(|_| StorageError::QueryFailed)?
            .build()
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        let mut results = riceberg_core::ScanResults::new(scan);

        while let Ok(Some(row)) = results.next_row().await {
            if let Ok(Some(Value::Int64(row_id))) = row.get(0) {
                if row_id as u64 == id {
                    return Ok(Self::row_to_reading(&row));
                }
            }
        }

        Ok(None)
    }

    async fn delete(&mut self, id: u64) -> Result<bool, StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;

        // Use EdgeDatabase's delete functionality
        // Delete by the ID field (field 0)
        match self.db.delete(table_id, EntityId(id as i64)).await {
            Ok(_) => Ok(true),
            Err(_) => {
                // Check if it was because the row didn't exist
                // For now, assume any error means not found or delete failed
                Ok(false)
            }
        }
    }

    async fn query_filtered(
        &mut self,
        filters: &[QueryFilter],
        limit: Option<u16>,
        offset: Option<u32>,
    ) -> Result<(Vec<SensorReading, MAX_QUERY_RESULTS>, u32, bool), StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;

        // Build query with predicates using QueryBuilder
        let mut query = QueryBuilder::new(&self.db, table_id);

        // Convert and apply each filter
        for filter in filters {
            let field_idx = filter.field.to_field_idx();
            let op = convert_filter_op(filter.op);
            let predicate_value = convert_filter_value(&filter.value);

            // Apply filter based on operation
            query = match op {
                CompareOp::Eq => query.filter_eq(field_idx, predicate_value),
                CompareOp::Ne => query.filter_ne(field_idx, predicate_value),
                CompareOp::Lt => query.filter_lt(field_idx, predicate_value),
                CompareOp::Le => query.filter_le(field_idx, predicate_value),
                CompareOp::Gt => query.filter_gt(field_idx, predicate_value),
                CompareOp::Ge => query.filter_ge(field_idx, predicate_value),
                _ => query, // Ignore unsupported ops like IsNull/IsNotNull
            };
        }

        // Apply limit and offset
        if let Some(lim) = limit {
            query = query.limit(lim as u32);
        }
        if let Some(off) = offset {
            query = query.offset(off);
        }

        // Execute query using execute_into to convert rows to SensorReading
        let readings_vec = query
            .execute_into(|reader| {
                // Field 0: id
                let id = reader.get_i64(0).ok().flatten().unwrap_or(0) as u64;

                // Field 1: timestamp_us
                let timestamp_us = reader.get_i64(1).ok().flatten().unwrap_or(0);

                // Field 2: temperature_c
                let temperature_c = reader.get_f32(2).ok().flatten().unwrap_or(0.0);

                // Field 3: sensor_id
                let sensor_id = match reader.get_str(3).ok().flatten() {
                    Some(bytes) => {
                        let s = core::str::from_utf8(bytes).unwrap_or("onboard");
                        match s {
                            "onboard" => SensorId::ONBOARD,
                            "external_1" => SensorId::EXTERNAL_1,
                            "external_2" => SensorId::EXTERNAL_2,
                            "external_3" => SensorId::EXTERNAL_3,
                            "test" | "TEST" => SensorId::TEST,
                            _ => SensorId::ONBOARD,
                        }
                    }
                    None => SensorId::ONBOARD,
                };

                Ok(SensorReading::with_id(id, timestamp_us, temperature_c, sensor_id))
            })
            .await
            .map_err(|_| StorageError::QueryFailed)?;

        // Convert to heapless Vec and calculate pagination info
        let total = readings_vec.len() as u32;
        let has_more = limit.map(|l| total >= l as u32).unwrap_or(false);

        let mut output = Vec::new();
        for reading in readings_vec.into_iter().take(MAX_QUERY_RESULTS) {
            let _ = output.push(reading);
        }

        Ok((output, total, has_more))
    }

    async fn compact(&mut self) -> Result<CompactionResult, StorageError> {
        let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;

        // Use default compaction config
        let config = CompactionConfig::default();

        // Check if compaction is needed (lightweight check)
        let needs_compaction = self.db.needs_compaction(table_id, &config).await;

        if !needs_compaction {
            return Ok(CompactionResult {
                files_before: 0,
                files_after: 0,
                rows_compacted: 0,
                was_needed: false,
            });
        }

        // Note: Full compaction execution (plan_compaction + compact) requires
        // significant stack/heap memory that can cause issues on embedded targets.
        // For now, we report that compaction is needed but don't execute it.
        // Use expire() to reclaim space from old snapshots instead.
        Ok(CompactionResult {
            files_before: 1,
            files_after: 0,
            rows_compacted: 0,
            was_needed: true,
        })
    }

    async fn expire(&mut self, keep_last: u32) -> Result<ExpireResult, StorageError> {
        // Use ExpireConfig to keep last N snapshots
        let config = riceberg_core::ExpireConfig::keep_snapshots(keep_last as usize);

        match self.db.expire_with_config(&config).await {
            Ok(result) => Ok(ExpireResult {
                snapshots_expired: result.snapshots_expired as u32,
                pages_freed: result.pages_reclaimed,
            }),
            Err(_) => Err(StorageError::DatabaseError),
        }
    }

    fn capacity(&self) -> CapacityResult {
        let info = self.db.capacity_info();
        CapacityResult {
            total_pages: info.max_pages,
            allocated_pages: info.current_pages,
            free_pages: info.growth_headroom,
        }
    }
}

// ============================================================================
// Helper Functions for Filter Conversion
// ============================================================================

/// Convert FilterOp to riceberg CompareOp
fn convert_filter_op(op: FilterOp) -> CompareOp {
    match op {
        FilterOp::Eq => CompareOp::Eq,
        FilterOp::Ne => CompareOp::Ne,
        FilterOp::Lt => CompareOp::Lt,
        FilterOp::Le => CompareOp::Le,
        FilterOp::Gt => CompareOp::Gt,
        FilterOp::Ge => CompareOp::Ge,
    }
}

/// Convert FilterValue to riceberg PredicateValue
fn convert_filter_value(value: &FilterValue) -> PredicateValue {
    match value {
        FilterValue::Int64(v) => PredicateValue::from_i64(*v),
        FilterValue::Float32(v) => PredicateValue::from_f32(*v),
        FilterValue::String(_s) => {
            // Note: String predicates are complex in riceberg - for now use i64(0)
            // String filtering would require byte-level comparison
            // This is a limitation - recommend using numeric fields for filtering
            PredicateValue::from_i64(0)
        }
    }
}
