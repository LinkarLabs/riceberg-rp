//! Riceberg Temperature Sensor with USB Query Interface
//!
//! This example reads the onboard temperature sensor, stores readings in a Riceberg database,
//! and provides a USB interface for querying the data.
//!
//! # Hardware
//!
//! - Raspberry Pi Pico 2 (RP2350)
//! - USB connection for queries
//!
//! # Usage
//!
//! 1. Flash this firmware:
//!    ```bash
//!    cargo run --example riceberg_temperature_usb --release
//!    ```
//!
//! 2. Connect via USB and query the database:
//!    ```bash
//!    cargo run --bin riceberg_host
//!    ```
//!
//! # Architecture
//!
//! This firmware uses a channel-based architecture with dedicated tasks:
//!
//! - **USB Task**: Handles USB enumeration and communication (spawned separately for reliability)
//! - **DB Task**: Owns the database, processes all reads and writes sequentially
//! - **Sensor Task**: Reads ADC temperature sensor, sends readings via channel to DB task
//! - **Query Handler**: Receives USB commands, forwards to DB task via channel
//!
//! All database operations are serialized through the DB task to avoid concurrency issues.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate alloc;

use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, Ordering};
use defmt::*;
use embassy_rp::adc::{Adc, Channel as AdcChannel};
use embassy_rp::flash::{Async, Flash};
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_rp::{bind_interrupts, peripherals};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::{Builder, Config};
use embedded_alloc::Heap;
use {defmt_rtt as _, panic_probe as _};

use riceberg_core::{
    Database, DatabaseConfig, FieldType, ScanBuilder, ScanResults, Schema, SchemaBuilder, Value,
};
use riceberg_storage::{BlockDeviceStorage, NorFlashBlockDevice};

use rp::db_protocol::{DbCommand, DbResponse, TemperatureReading};

// ============================================================================
// Memory Configuration
// ============================================================================

#[global_allocator]
static HEAP: Heap = Heap::empty();

const HEAP_SIZE: usize = 96 * 1024; // 96KB heap

// ============================================================================
// Flash Storage Configuration
// ============================================================================

/// USB Serial version - increment this to force Windows to see device as new
/// (Windows caches USB descriptors by VID/PID/Serial)
const USB_SERIAL_VERSION: u32 = 70;

/// Flash offset where database starts (1.5MB)
const FLASH_OFFSET: u32 = 0x180000;

/// Database size in flash (512KB)
const FLASH_SIZE: u32 = 512 * 1024;

/// Number of 4KB pages available (512KB / 4KB = 128)
const FLASH_PAGES_4KB: u32 = 128;

// ============================================================================
// Sensor Configuration
// ============================================================================

/// How often to read the temperature sensor (seconds)
const READING_INTERVAL_SECS: u64 = 5;

/// Maximum message size for USB
const MAX_MSG_SIZE: usize = 8192;

// ============================================================================
// Type Aliases
// ============================================================================

type FlashDevice = Flash<'static, peripherals::FLASH, Async, { 2 * 1024 * 1024 }>;
type EdgeStorage = BlockDeviceStorage<NorFlashBlockDevice<FlashDevice>>;
type EdgeDatabase = Database<EdgeStorage>;

// ============================================================================
// Channels for Inter-Task Communication
// ============================================================================

/// A sensor reading to be written to the database
#[derive(Clone, Copy)]
struct SensorReading {
    timestamp_us: i64,
    temperature_c: f32,
}

/// Channel for sending sensor readings to DB task
static READINGS_CHANNEL: Channel<CriticalSectionRawMutex, SensorReading, 32> = Channel::new();

/// Channel for sending DB commands from query handler to DB task
static DB_CMD_CHANNEL: Channel<CriticalSectionRawMutex, DbCommand, 4> = Channel::new();

/// Channel for receiving DB responses from DB task
static DB_RESP_CHANNEL: Channel<CriticalSectionRawMutex, DbResponse, 4> = Channel::new();

// ============================================================================
// Diagnostics Counters
// ============================================================================

/// Signal that database is initialized and ready for queries
static DB_READY: AtomicBool = AtomicBool::new(false);

/// Counter for sensor readings received by DB task
static SENSOR_READINGS_RECEIVED: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Counter for sensor readings sent by sensor task
static SENSOR_READINGS_SENT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Flag to indicate sensor task is running
static SENSOR_TASK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Last raw ADC value (for diagnostics)
static LAST_ADC_VALUE: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);

/// Counter for successful DB writes
static DB_WRITES_SUCCESS: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Counter for failed DB writes (insert failed)
static DB_INSERT_FAILED: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Counter for failed DB writes (commit failed)
static DB_COMMIT_FAILED: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

// ============================================================================
// Interrupt Bindings
// ============================================================================

bind_interrupts!(struct Irqs {
    ADC_IRQ_FIFO => embassy_rp::adc::InterruptHandler;
    USBCTRL_IRQ => UsbInterruptHandler<peripherals::USB>;
});

// ============================================================================
// Schema Definition
// ============================================================================

/// Create schema for temperature readings table
fn create_temperature_schema() -> Schema {
    SchemaBuilder::new(1)
        .required("timestamp_us", FieldType::Int64)
        .unwrap()
        .required("temperature_c", FieldType::Float32)
        .unwrap()
        .optional("sensor_id", FieldType::String)
        .unwrap()
        .build()
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert ADC reading to temperature in Celsius (RP2350)
///
/// NOTE: The standard RP2040 formula `temp = 27 - (voltage - 0.706) / 0.001721`
/// does NOT work on RP2350. The ADC returns values around 50-60 at room temp
/// instead of the expected ~876. This is a known issue with RP2350.
///
/// This empirical calibration is based on observed ADC values:
/// - ADC ≈ 57 at room temperature (~27°C)
/// - Linear scale factor: 0.474 (so temp ≈ ADC * 0.474)
fn adc_to_temperature(adc_value: u16) -> f32 {
    // Empirical calibration: ADC 57 ≈ 27°C → scale = 27/57 ≈ 0.474
    adc_value as f32 * 0.474
}

/// Get current timestamp in microseconds
fn get_timestamp_us() -> i64 {
    embassy_time::Instant::now().as_micros() as i64
}

// ============================================================================
// Main Entry Point
// ============================================================================

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    // Initialize heap
    {
        use core::mem::MaybeUninit;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
    }

    info!("=== Riceberg Temperature Sensor with USB ===");

    // Initialize peripherals
    let p = embassy_rp::init(Default::default());

    // Hold ADC peripherals for sensor task
    let adc_peripheral = p.ADC;
    let temp_sensor = p.ADC_TEMP_SENSOR;

    // Initialize flash storage
    info!("Initializing flash storage...");
    let flash = Flash::<_, Async, { 2 * 1024 * 1024 }>::new(p.FLASH, p.DMA_CH0);
    let block_device = NorFlashBlockDevice::new(flash, FLASH_OFFSET, FLASH_SIZE);
    let storage = BlockDeviceStorage::new(block_device, FLASH_PAGES_4KB);

    // Setup USB
    info!("Setting up USB...");

    // Build serial number with version (forces Windows to recognize new firmware)
    static mut SERIAL_NUMBER: [u8; 16] = [0u8; 16];
    let serial_str = unsafe {
        SERIAL_NUMBER[0] = b'R';
        SERIAL_NUMBER[1] = b'I';
        SERIAL_NUMBER[2] = b'C';
        SERIAL_NUMBER[3] = b'E';

        let mut ver = USB_SERIAL_VERSION;
        let mut digits = [0u8; 10];
        let mut num_digits = 0;

        loop {
            digits[num_digits] = b'0' + (ver % 10) as u8;
            num_digits += 1;
            ver /= 10;
            if ver == 0 {
                break;
            }
        }

        for i in 0..num_digits {
            SERIAL_NUMBER[4 + i] = digits[num_digits - 1 - i];
        }

        let len = 4 + num_digits;
        core::str::from_utf8_unchecked(&SERIAL_NUMBER[..len])
    };

    info!("USB Serial: {}", serial_str);

    let driver = Driver::new(p.USB, Irqs);

    let mut config = Config::new(0x2e8a, 0x000b);
    config.manufacturer = Some("Raspberry Pi");
    config.product = Some("Riceberg Temperature Sensor");
    config.serial_number = Some(serial_str);
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    static mut CONFIG_DESCRIPTOR: [u8; 256] = [0; 256];
    static mut BOS_DESCRIPTOR: [u8; 256] = [0; 256];
    static mut MSOS_DESCRIPTOR: [u8; 256] = [0; 256];
    static mut CONTROL_BUF: [u8; 64] = [0; 64];
    static mut STATE: State = State::new();

    let mut builder = unsafe {
        Builder::new(
            driver,
            config,
            &mut CONFIG_DESCRIPTOR,
            &mut BOS_DESCRIPTOR,
            &mut MSOS_DESCRIPTOR,
            &mut CONTROL_BUF,
        )
    };

    let class = unsafe { CdcAcmClass::new(&mut builder, &mut STATE, 64) };
    let usb = builder.build();

    info!("Spawning tasks...");

    // Spawn USB as dedicated task for reliable enumeration
    spawner.spawn(usb_device_task(usb).expect("usb task"));

    // Spawn DB task (owns database, handles all DB operations)
    spawner.spawn(db_task(storage).expect("db task"));

    // Spawn sensor task (reads ADC, sends to channel)
    spawner.spawn(sensor_task(adc_peripheral, temp_sensor).expect("sensor task"));

    info!("All tasks spawned!");

    // Query handler runs in main context, forwards commands to DB task
    query_handler(class).await;
}

// ============================================================================
// USB Device Task
// ============================================================================

/// USB device task - runs USB stack (spawned separately for reliable enumeration)
#[embassy_executor::task]
async fn usb_device_task(
    mut usb: embassy_usb::UsbDevice<'static, Driver<'static, peripherals::USB>>,
) -> ! {
    info!("USB device task started");
    usb.run().await
}

// ============================================================================
// Database Task
// ============================================================================

/// Dedicated DB task - ALL database operations happen here
/// Receives sensor readings and query commands via channels
#[embassy_executor::task]
async fn db_task(storage: EdgeStorage) {
    info!("DB task started - initializing database...");

    let db_config = DatabaseConfig {
        initial_pages: FLASH_PAGES_4KB,
        ..Default::default()
    };

    // Create fresh database
    let mut db = match Database::create(storage, db_config).await {
        Ok(db) => {
            info!("Database created");
            db
        }
        Err(_) => {
            error!("Failed to create database");
            loop {
                Timer::after(Duration::from_secs(3600)).await;
            }
        }
    };

    // Create/get temperature table
    let schema = create_temperature_schema();
    let table_id = match db.get_table("temperature") {
        Some(id) => {
            info!("Table exists (ID: {})", id);
            id
        }
        None => match db.create_table("temperature", schema.clone()).await {
            Ok(id) => {
                info!("Table created (ID: {})", id);
                id
            }
            Err(_) => {
                error!("Failed to create table");
                loop {
                    Timer::after(Duration::from_secs(3600)).await;
                }
            }
        },
    };

    // Write test readings to verify database is working
    for i in 0..3 {
        info!("Writing TEST reading #{}...", i + 1);
        let test_ts = get_timestamp_us();
        let mut txn = db.begin_write();
        match txn
            .insert(table_id, &schema, |row| {
                row.set(0, Value::Int64(test_ts))?;
                row.set(1, Value::Float32(25.0 + i as f32))?;
                row.set(2, Value::String("TEST".as_bytes()))?;
                Ok(())
            })
            .await
        {
            Ok(()) => match txn.commit().await {
                Ok(snap) => {
                    // CRITICAL: Must notify database of commit to update snapshot counter!
                    // Without this, subsequent transactions fail OCC (Optimistic Concurrency Control)
                    db.notify_commit(snap);
                    info!("TEST #{} write SUCCESS (snapshot {})", i + 1, snap);
                    DB_WRITES_SUCCESS.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    info!("TEST #{} commit FAILED", i + 1);
                    DB_COMMIT_FAILED.fetch_add(1, Ordering::Relaxed);
                }
            },
            Err(_) => {
                info!("TEST #{} insert FAILED", i + 1);
                DB_INSERT_FAILED.fetch_add(1, Ordering::Relaxed);
                txn.abort().await;
            }
        }
        Timer::after(Duration::from_millis(100)).await;
    }

    // Signal that DB is ready
    DB_READY.store(true, Ordering::Release);
    info!("DB task ready");

    // Main loop: handle sensor writes and query requests sequentially
    loop {
        // Check for query commands (priority)
        if let Ok(cmd) = DB_CMD_CHANNEL.try_receive() {
            let response = process_db_command(&mut db, table_id, &schema, cmd).await;
            DB_RESP_CHANNEL.send(response).await;
            continue;
        }

        // Check for sensor readings
        if let Ok(reading) = READINGS_CHANNEL.try_receive() {
            SENSOR_READINGS_RECEIVED.fetch_add(1, Ordering::Relaxed);
            let mut txn = db.begin_write();
            match txn
                .insert(table_id, &schema, |row| {
                    row.set(0, Value::Int64(reading.timestamp_us))?;
                    row.set(1, Value::Float32(reading.temperature_c))?;
                    row.set(2, Value::String("sensor".as_bytes()))?;
                    Ok(())
                })
                .await
            {
                Ok(()) => match txn.commit().await {
                    Ok(snap) => {
                        // CRITICAL: Must notify database of commit!
                        db.notify_commit(snap);
                        DB_WRITES_SUCCESS.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        DB_COMMIT_FAILED.fetch_add(1, Ordering::Relaxed);
                    }
                },
                Err(_) => {
                    DB_INSERT_FAILED.fetch_add(1, Ordering::Relaxed);
                    txn.abort().await;
                }
            }
            continue;
        }

        // Nothing to do, yield briefly
        Timer::after(Duration::from_millis(10)).await;
    }
}

/// Process a database command (runs in db_task context)
async fn process_db_command(
    db: &mut EdgeDatabase,
    table_id: u32,
    schema: &Schema,
    command: DbCommand,
) -> DbResponse {
    match command {
        DbCommand::Stats => {
            // Scan in isolated scope to ensure scan is dropped before returning
            let scan_result: Result<(u32, i64, i64), &'static str> = async {
                let builder = match ScanBuilder::new(db.storage(), table_id, schema).await {
                    Ok(b) => b,
                    Err(_) => return Err("Failed to create scan"),
                };
                let scan = match builder.build().await {
                    Ok(s) => s,
                    Err(_) => return Err("Failed to build scan"),
                };
                let mut results = ScanResults::new(scan);
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
                Ok((count, oldest, newest))
            }
            .await;

            match scan_result {
                Ok((count, oldest, newest)) => DbResponse::Stats {
                    total_readings: count,
                    oldest_timestamp_us: if count > 0 { oldest } else { 0 },
                    newest_timestamp_us: if count > 0 { newest } else { 0 },
                    snapshot_id: db.current_snapshot_id(),
                },
                Err(msg) => DbResponse::error(msg),
            }
        }
        DbCommand::QueryLatest { count } => {
            query_latest(db, table_id, schema, count as usize).await
        }
        DbCommand::QueryRange { start_us, end_us } => {
            query_range(db, table_id, schema, start_us, end_us).await
        }
        DbCommand::ScanAll { offset } => query_scan(db, table_id, schema, offset as usize).await,
        DbCommand::Diagnostics => DbResponse::Diagnostics {
            db_ready: DB_READY.load(Ordering::Relaxed),
            sensor_task_running: SENSOR_TASK_RUNNING.load(Ordering::Relaxed),
            sensor_readings_sent: SENSOR_READINGS_SENT.load(Ordering::Relaxed),
            sensor_readings_received: SENSOR_READINGS_RECEIVED.load(Ordering::Relaxed),
            db_writes_success: DB_WRITES_SUCCESS.load(Ordering::Relaxed),
            db_insert_failed: DB_INSERT_FAILED.load(Ordering::Relaxed),
            db_commit_failed: DB_COMMIT_FAILED.load(Ordering::Relaxed),
            last_adc_value: LAST_ADC_VALUE.load(Ordering::Relaxed),
            uptime_ms: embassy_time::Instant::now().as_millis(),
        },
    }
}

// ============================================================================
// Sensor Task
// ============================================================================

/// Sensor task - reads ADC temperature sensor and sends to channel
#[embassy_executor::task]
async fn sensor_task(
    adc_peripheral: embassy_rp::Peri<'static, peripherals::ADC>,
    temp_sensor: embassy_rp::Peri<'static, peripherals::ADC_TEMP_SENSOR>,
) {
    // Mark that sensor task has started
    SENSOR_TASK_RUNNING.store(true, Ordering::Release);
    info!("Sensor task started");

    // Wait for database to be ready
    while !DB_READY.load(Ordering::Acquire) {
        Timer::after(Duration::from_millis(500)).await;
    }
    info!("Sensor task: DB ready, starting readings");

    // Use blocking ADC reads (no DMA, avoids conflict with flash DMA)
    let mut adc = Adc::new_blocking(adc_peripheral, embassy_rp::adc::Config::default());
    let mut ts = AdcChannel::new_temp_sensor(temp_sensor);

    loop {
        let adc_value = adc.blocking_read(&mut ts).unwrap();
        LAST_ADC_VALUE.store(adc_value, Ordering::Relaxed);
        let temperature = adc_to_temperature(adc_value);
        let timestamp = get_timestamp_us();

        // Send to DB task via channel
        let reading = SensorReading {
            timestamp_us: timestamp,
            temperature_c: temperature,
        };
        match READINGS_CHANNEL.try_send(reading) {
            Ok(()) => {
                SENSOR_READINGS_SENT.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                // Channel full - reading lost (shouldn't happen with 32-slot channel)
            }
        }

        Timer::after(Duration::from_secs(READING_INTERVAL_SECS)).await;
    }
}

// ============================================================================
// Query Handler
// ============================================================================

/// Query handler - receives USB commands and forwards to DB task via channel
async fn query_handler<D>(mut class: CdcAcmClass<'static, D>)
where
    D: embassy_usb::driver::Driver<'static>,
{
    info!("Query handler started");

    loop {
        class.wait_connection().await;
        info!("USB connected");

        // Wait for database to be ready
        while !DB_READY.load(Ordering::Acquire) {
            Timer::after(Duration::from_millis(100)).await;
        }

        // Send ready message
        if let Ok(msg) = postcard::to_vec_cobs::<_, MAX_MSG_SIZE>(&DbResponse::Ok) {
            let _ = send_cobs_message(&mut class, &msg).await;
        }
        info!("Ready sent");

        // Command loop
        loop {
            let mut rx_buf = heapless::Vec::<u8, MAX_MSG_SIZE>::new();
            let mut packet_buf = [0u8; 64]; // USB CDC max packet size

            // Read until COBS sentinel (0x00)
            'read: loop {
                match class.read_packet(&mut packet_buf).await {
                    Ok(n) if n > 0 => {
                        for i in 0..n {
                            let _ = rx_buf.push(packet_buf[i]);
                            if packet_buf[i] == 0x00 {
                                break 'read;
                            }
                            if rx_buf.is_full() {
                                break 'read;
                            }
                        }
                    }
                    _ => break 'read,
                }
            }

            if rx_buf.is_empty() {
                break; // Connection lost
            }

            // Decode and process command
            if let Ok(cmd) = postcard::from_bytes_cobs::<DbCommand>(&mut rx_buf) {
                // Forward to DB task
                DB_CMD_CHANNEL.send(cmd).await;
                let response = DB_RESP_CHANNEL.receive().await;

                // Send response
                if let Ok(resp) = postcard::to_vec_cobs::<_, MAX_MSG_SIZE>(&response) {
                    let _ = send_cobs_message(&mut class, &resp).await;
                }
            }
        }
        info!("USB disconnected");
    }
}

/// Send COBS-encoded message over USB CDC (in 64-byte chunks)
async fn send_cobs_message<D>(class: &mut CdcAcmClass<'static, D>, data: &[u8]) -> Result<(), ()>
where
    D: embassy_usb::driver::Driver<'static>,
{
    for chunk in data.chunks(64) {
        if class.write_packet(chunk).await.is_err() {
            return Err(());
        }
    }
    Ok(())
}

// ============================================================================
// Query Functions
// ============================================================================

async fn query_latest(
    db: &mut EdgeDatabase,
    table_id: u32,
    schema: &Schema,
    count: usize,
) -> DbResponse {
    match ScanBuilder::new(db.storage(), table_id, schema).await {
        Ok(builder) => match builder.build().await {
            Ok(scan) => {
                let mut results = ScanResults::new(scan);
                let mut all_readings = alloc::vec::Vec::new();

                while let Ok(Some(row)) = results.next_row().await {
                    if let (Ok(Some(Value::Int64(ts))), Ok(Some(Value::Float32(temp)))) =
                        (row.get(0), row.get(1))
                    {
                        if let Some(reading) = TemperatureReading::new(ts, temp, "onboard") {
                            all_readings.push(reading);
                        }
                    }
                }

                // Take last N readings
                let start_idx = if all_readings.len() > count {
                    all_readings.len() - count
                } else {
                    0
                };

                let readings = &all_readings[start_idx..];
                DbResponse::readings(readings, all_readings.len() as u32, false)
            }
            Err(_) => DbResponse::error("Failed to build scan"),
        },
        Err(_) => DbResponse::error("Failed to create scan"),
    }
}

async fn query_range(
    db: &mut EdgeDatabase,
    table_id: u32,
    schema: &Schema,
    start_us: i64,
    end_us: i64,
) -> DbResponse {
    match ScanBuilder::new(db.storage(), table_id, schema).await {
        Ok(builder) => match builder.build().await {
            Ok(scan) => {
                let mut results = ScanResults::new(scan);
                let mut readings = alloc::vec::Vec::new();

                while let Ok(Some(row)) = results.next_row().await {
                    if let (Ok(Some(Value::Int64(ts))), Ok(Some(Value::Float32(temp)))) =
                        (row.get(0), row.get(1))
                    {
                        if ts >= start_us && ts <= end_us {
                            if let Some(reading) = TemperatureReading::new(ts, temp, "onboard") {
                                readings.push(reading);
                            }
                        }
                    }
                }

                let total = readings.len();
                let has_more = total > 32;
                DbResponse::readings(&readings[..total.min(32)], total as u32, has_more)
            }
            Err(_) => DbResponse::error("Failed to build scan"),
        },
        Err(_) => DbResponse::error("Failed to create scan"),
    }
}

async fn query_scan(
    db: &mut EdgeDatabase,
    table_id: u32,
    schema: &Schema,
    offset: usize,
) -> DbResponse {
    match ScanBuilder::new(db.storage(), table_id, schema).await {
        Ok(builder) => match builder.build().await {
            Ok(scan) => {
                let mut results = ScanResults::new(scan);
                let mut all_readings = alloc::vec::Vec::new();

                while let Ok(Some(row)) = results.next_row().await {
                    if let (Ok(Some(Value::Int64(ts))), Ok(Some(Value::Float32(temp)))) =
                        (row.get(0), row.get(1))
                    {
                        if let Some(reading) = TemperatureReading::new(ts, temp, "onboard") {
                            all_readings.push(reading);
                        }
                    }
                }

                let total = all_readings.len();
                let start = offset.min(total);
                let end = (start + 32).min(total);
                let has_more = end < total;

                DbResponse::readings(&all_readings[start..end], total as u32, has_more)
            }
            Err(_) => DbResponse::error("Failed to build scan"),
        },
        Err(_) => DbResponse::error("Failed to create scan"),
    }
}
