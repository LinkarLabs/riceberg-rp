//! Riceberg AS7343 Spectral Sensor with USB Query Interface
//!
//! This example reads the AS7343 14-channel spectral sensor via I2C, stores readings
//! in a Riceberg database on flash, and provides a USB interface for querying the data.
//!
//! # Hardware
//!
//! - Raspberry Pi Pico 2 (RP2350)
//! - AS7343 breakout connected via QWIIC/I2C (GPIO4=SDA, GPIO5=SCL)
//! - USB connection for queries
//!
//! # Usage
//!
//! 1. Flash this firmware:
//!    ```bash
//!    cargo run --example riceberg_as7343 --release
//!    ```
//!
//! 2. Connect via USB and query the database:
//!    ```bash
//!    cargo host -- spectral-latest 5
//!    ```
//!
//! # Architecture
//!
//! - **USB Task**: Handles USB enumeration and communication
//! - **DB Task**: Owns the database, processes all writes and queries sequentially
//! - **Spectral Sensor Task**: Reads AS7343 via I2C, sends readings via channel
//! - **Query Handler**: Receives USB commands, forwards to DB task via channel

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate alloc;

use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use defmt::*;
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

use rp::adapters::As7343Adapter;
use rp::db_protocol::{DbCommand, DbResponse, SpectralReadingProtocol};
use rp::domain::{SensorId, SpectralReading};
use rp::ports::SpectralSensorPort;

// ============================================================================
// Memory Configuration
// ============================================================================

#[global_allocator]
static HEAP: Heap = Heap::empty();

const HEAP_SIZE: usize = 96 * 1024;

// ============================================================================
// Flash Storage Configuration
// ============================================================================

const USB_SERIAL_VERSION: u32 = 80;
const FLASH_OFFSET: u32 = 0x180000;
const FLASH_SIZE: u32 = 512 * 1024;
const FLASH_PAGES_4KB: u32 = 128;

// ============================================================================
// Sensor Configuration
// ============================================================================

/// How often to read the spectral sensor (seconds)
const SPECTRAL_INTERVAL_SECS: u64 = 1;

const MAX_MSG_SIZE: usize = 8192;

// ============================================================================
// Type Aliases
// ============================================================================

type FlashDevice = Flash<'static, peripherals::FLASH, Async, { 2 * 1024 * 1024 }>;
type EdgeStorage = BlockDeviceStorage<NorFlashBlockDevice<FlashDevice>>;
type EdgeDatabase = Database<EdgeStorage>;
type I2cDevice = embassy_rp::i2c::I2c<'static, peripherals::I2C0, embassy_rp::i2c::Async>;

// ============================================================================
// Channels for Inter-Task Communication
// ============================================================================

/// Channel for sending spectral readings to DB task
static SPECTRAL_CHANNEL: Channel<CriticalSectionRawMutex, SpectralReading, 8> = Channel::new();

/// Channel for sending DB commands from query handler to DB task
static DB_CMD_CHANNEL: Channel<CriticalSectionRawMutex, DbCommand, 4> = Channel::new();

/// Channel for receiving DB responses from DB task
static DB_RESP_CHANNEL: Channel<CriticalSectionRawMutex, DbResponse, 4> = Channel::new();

// ============================================================================
// Diagnostics Counters
// ============================================================================

static DB_READY: AtomicBool = AtomicBool::new(false);
static SPECTRAL_TASK_RUNNING: AtomicBool = AtomicBool::new(false);
static SPECTRAL_READINGS_SENT: AtomicU32 = AtomicU32::new(0);
static SPECTRAL_READINGS_RECEIVED: AtomicU32 = AtomicU32::new(0);
static DB_WRITES_SUCCESS: AtomicU32 = AtomicU32::new(0);
static DB_INSERT_FAILED: AtomicU32 = AtomicU32::new(0);
static DB_COMMIT_FAILED: AtomicU32 = AtomicU32::new(0);

// Stats tracking (to avoid table scans)
static SPECTRAL_COUNT: AtomicU32 = AtomicU32::new(0);
static SPECTRAL_OLDEST_TS_MS: AtomicU32 = AtomicU32::new(u32::MAX);
static SPECTRAL_NEWEST_TS_MS: AtomicU32 = AtomicU32::new(0);

// ============================================================================
// Interrupt Bindings
// ============================================================================

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<peripherals::USB>;
    I2C0_IRQ => embassy_rp::i2c::InterruptHandler<peripherals::I2C0>;
});

// ============================================================================
// Schema Definition
// ============================================================================

/// Create schema for spectral readings table
///
/// Schema fields (16 total - matches riceberg memory-minimal MAX_INLINE_FIELDS):
/// - 0: timestamp_us (Int64)
/// - 1-12: spectral channels F1, F2, FZ, F3, F4, F5, FY, FXL, F6, F7, F8, NIR (Int64)
/// - 13: clear (Int64)
/// - 14: flicker (Int64)
/// - 15: metadata (Int64) - low byte = gain, bit 8 = saturated
fn create_spectral_schema() -> Schema {
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

    info!("=== Riceberg AS7343 Spectral Sensor ===");

    let p = embassy_rp::init(Default::default());

    // Initialize flash storage
    info!("Initializing flash storage...");
    let flash = Flash::<_, Async, { 2 * 1024 * 1024 }>::new(p.FLASH, p.DMA_CH0);
    let block_device = NorFlashBlockDevice::new(flash, FLASH_OFFSET, FLASH_SIZE);
    let storage = BlockDeviceStorage::new(block_device, FLASH_PAGES_4KB);

    // Setup USB
    info!("Setting up USB...");

    static mut SERIAL_NUMBER: [u8; 16] = [0u8; 16];
    let serial_str = unsafe {
        SERIAL_NUMBER[0] = b'S';
        SERIAL_NUMBER[1] = b'P';
        SERIAL_NUMBER[2] = b'E';
        SERIAL_NUMBER[3] = b'C';

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
    config.product = Some("Riceberg Spectral Sensor");
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

    // Initialize I2C for AS7343 (QWIIC: GPIO4=SDA, GPIO5=SCL)
    // NOTE: I2C setup is done after USB so that USB enumeration is not blocked
    // if the sensor is missing or the I2C bus hangs. Sensor init happens in the task.
    let i2c = embassy_rp::i2c::I2c::new_async(
        p.I2C0,
        p.PIN_5, // SCL
        p.PIN_4, // SDA
        Irqs,
        embassy_rp::i2c::Config::default(),
    );
    let spectral_sensor = As7343Adapter::new(i2c, SensorId::AS7343);

    info!("Spawning tasks...");

    // Spawn USB first so enumeration works even if sensor init fails
    spawner.spawn(usb_device_task(usb).expect("usb task"));
    spawner.spawn(db_task(storage).expect("db task"));
    spawner.spawn(spectral_sensor_task(spectral_sensor).expect("spectral task"));

    info!("All tasks spawned!");

    // Query handler runs in main context
    query_handler(class).await;
}

// ============================================================================
// USB Device Task
// ============================================================================

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

#[embassy_executor::task]
async fn db_task(storage: EdgeStorage) {
    info!("DB task started - initializing database...");

    let db_config = DatabaseConfig {
        initial_pages: FLASH_PAGES_4KB,
        ..Default::default()
    };

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

    // Create spectral table
    let schema = create_spectral_schema();
    let table_id = match db.get_table("spectral") {
        Some(id) => {
            info!("Spectral table exists (ID: {})", id);
            id
        }
        None => match db.create_table("spectral", schema.clone()).await {
            Ok(id) => {
                info!("Spectral table created (ID: {})", id);
                id
            }
            Err(_) => {
                error!("Failed to create spectral table");
                loop {
                    Timer::after(Duration::from_secs(3600)).await;
                }
            }
        },
    };

    DB_READY.store(true, Ordering::Release);
    info!("DB task ready - spectral table initialized");

    // Main loop: handle spectral writes and query requests
    loop {
        // Check for query commands (priority)
        if let Ok(cmd) = DB_CMD_CHANNEL.try_receive() {
            let response = process_db_command(&mut db, table_id, &schema, cmd).await;
            DB_RESP_CHANNEL.send(response).await;
            continue;
        }

        // Check for spectral readings
        if let Ok(reading) = SPECTRAL_CHANNEL.try_receive() {
            SPECTRAL_READINGS_RECEIVED.fetch_add(1, Ordering::Relaxed);
            let channels = reading.all_channels();
            let metadata = pack_metadata(reading.gain, reading.saturated);
            let mut txn = db.begin_write();
            match txn
                .insert(table_id, &schema, |row| {
                    row.set(0, Value::Int64(reading.timestamp_us))?;
                    for i in 0..14 {
                        row.set(1 + i, Value::Int64(channels[i] as i64))?;
                    }
                    row.set(15, Value::Int64(metadata))?;
                    Ok(())
                })
                .await
            {
                Ok(()) => match txn.commit().await {
                    Ok(snap) => {
                        db.notify_commit(snap);
                        DB_WRITES_SUCCESS.fetch_add(1, Ordering::Relaxed);

                        // Update stats atomics (avoid scan for stats)
                        SPECTRAL_COUNT.fetch_add(1, Ordering::Relaxed);
                        let ts_ms = (reading.timestamp_us / 1000) as u32;
                        SPECTRAL_OLDEST_TS_MS.fetch_min(ts_ms, Ordering::Relaxed);
                        SPECTRAL_NEWEST_TS_MS.fetch_max(ts_ms, Ordering::Relaxed);
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

        Timer::after(Duration::from_millis(10)).await;
    }
}

/// Process a database command
async fn process_db_command(
    db: &mut EdgeDatabase,
    table_id: u32,
    schema: &Schema,
    command: DbCommand,
) -> DbResponse {
    match command {
        DbCommand::QuerySpectralLatest { count } | DbCommand::QueryLatest { count } => {
            query_spectral_latest(db, table_id, schema, count as usize).await
        }
        DbCommand::QuerySpectralRange { start_us, end_us }
        | DbCommand::QueryRange { start_us, end_us } => {
            query_spectral_range(db, table_id, schema, start_us, end_us).await
        }
        DbCommand::SpectralStats | DbCommand::Stats => {
            // Use atomics instead of scanning - O(1) instead of O(n)
            let count = SPECTRAL_COUNT.load(Ordering::Relaxed);
            let oldest_ms = SPECTRAL_OLDEST_TS_MS.load(Ordering::Relaxed);
            let newest_ms = SPECTRAL_NEWEST_TS_MS.load(Ordering::Relaxed);

            DbResponse::SpectralStats {
                total_readings: count,
                oldest_timestamp_us: if count > 0 { oldest_ms as i64 * 1000 } else { 0 },
                newest_timestamp_us: if count > 0 { newest_ms as i64 * 1000 } else { 0 },
                snapshot_id: db.current_snapshot_id(),
            }
        }
        DbCommand::ScanAll { offset } => {
            query_spectral_scan(db, table_id, schema, offset as usize).await
        }
        DbCommand::Diagnostics => DbResponse::Diagnostics {
            db_ready: DB_READY.load(Ordering::Relaxed),
            sensor_task_running: SPECTRAL_TASK_RUNNING.load(Ordering::Relaxed),
            sensor_readings_sent: SPECTRAL_READINGS_SENT.load(Ordering::Relaxed),
            sensor_readings_received: SPECTRAL_READINGS_RECEIVED.load(Ordering::Relaxed),
            db_writes_success: DB_WRITES_SUCCESS.load(Ordering::Relaxed),
            db_insert_failed: DB_INSERT_FAILED.load(Ordering::Relaxed),
            db_commit_failed: DB_COMMIT_FAILED.load(Ordering::Relaxed),
            last_adc_value: 0,
            uptime_ms: embassy_time::Instant::now().as_millis(),
        },
        _ => DbResponse::error("Command not supported in spectral-only mode"),
    }
}

// ============================================================================
// Spectral Sensor Task
// ============================================================================

#[embassy_executor::task]
async fn spectral_sensor_task(mut sensor: As7343Adapter<I2cDevice>) {
    SPECTRAL_TASK_RUNNING.store(true, Ordering::Release);
    info!("Spectral sensor task started");

    // Initialize sensor (retry up to 5 times)
    let mut initialized = false;
    for attempt in 1..=5 {
        info!("AS7343 init attempt {}/5...", attempt);
        match sensor.init().await {
            Ok(()) => {
                info!("AS7343 initialized successfully");
                initialized = true;
                break;
            }
            Err(e) => {
                warn!("AS7343 init attempt {} failed: {}", attempt, e);
                Timer::after(Duration::from_millis(500)).await;
            }
        }
    }

    if !initialized {
        error!("AS7343 init failed after 5 attempts - task stopping");
        SPECTRAL_TASK_RUNNING.store(false, Ordering::Release);
        return;
    }

    // Wait for database to be ready
    while !DB_READY.load(Ordering::Acquire) {
        Timer::after(Duration::from_millis(500)).await;
    }
    info!("Spectral task: DB ready, starting readings");

    let mut reading_count: u32 = 0;

    loop {
        match sensor.read().await {
            Ok(reading) => {
                reading_count += 1;

                // Log every 10th reading to avoid flooding defmt
                if reading_count % 10 == 1 {
                    let ch = reading.spectral_channels();
                    info!(
                        "Spectral #{}: F1={} FZ={} FY={} F6={} NIR={} gain={}",
                        reading_count, ch[0], ch[2], ch[6], ch[8], ch[11], reading.gain
                    );
                }

                match SPECTRAL_CHANNEL.try_send(reading) {
                    Ok(()) => {
                        SPECTRAL_READINGS_SENT.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        warn!("Spectral channel full, reading dropped");
                    }
                }
            }
            Err(e) => {
                warn!("Spectral read failed: {}", e);
            }
        }

        Timer::after(Duration::from_secs(SPECTRAL_INTERVAL_SECS)).await;
    }
}

// ============================================================================
// Query Handler
// ============================================================================

async fn query_handler<D>(mut class: CdcAcmClass<'static, D>)
where
    D: embassy_usb::driver::Driver<'static>,
{
    info!("Query handler started");

    loop {
        class.wait_connection().await;
        info!("USB connected");

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
            let mut packet_buf = [0u8; 64];

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
                break;
            }

            if let Ok(cmd) = postcard::from_bytes_cobs::<DbCommand>(&mut rx_buf) {
                DB_CMD_CHANNEL.send(cmd).await;
                let response = DB_RESP_CHANNEL.receive().await;

                if let Ok(resp) = postcard::to_vec_cobs::<_, MAX_MSG_SIZE>(&response) {
                    let _ = send_cobs_message(&mut class, &resp).await;
                }
            }
        }
        info!("USB disconnected");
    }
}

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
// Spectral Query Functions
// ============================================================================

fn row_to_spectral(row: &riceberg_core::ScannedRow<'_>) -> Option<SpectralReadingProtocol> {
    let timestamp_us = match row.get(0) {
        Ok(Some(Value::Int64(v))) => v,
        _ => return None,
    };

    let mut channels = [0u16; 14];
    for i in 0..12 {
        channels[i] = match row.get(1 + i) {
            Ok(Some(Value::Int64(v))) => v as u16,
            _ => 0,
        };
    }
    channels[12] = match row.get(13) {
        Ok(Some(Value::Int64(v))) => v as u16,
        _ => 0,
    };
    channels[13] = match row.get(14) {
        Ok(Some(Value::Int64(v))) => v as u16,
        _ => 0,
    };

    let (gain, saturated) = match row.get(15) {
        Ok(Some(Value::Int64(v))) => unpack_metadata(v),
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

async fn query_spectral_latest(
    db: &mut EdgeDatabase,
    table_id: u32,
    schema: &Schema,
    count: usize,
) -> DbResponse {
    // Limit to 4 readings max to keep response small
    let count = count.min(4);

    // Get total count from atomic (O(1), no scan needed)
    let total_count = SPECTRAL_COUNT.load(Ordering::Relaxed);

    if total_count == 0 {
        return DbResponse::spectral_readings(&[], 0, false);
    }

    // Calculate how many rows to skip
    let skip = (total_count as usize).saturating_sub(count);

    let builder = match ScanBuilder::new(db.storage(), table_id, schema).await {
        Ok(b) => b,
        Err(_) => return DbResponse::error("SL@1"),
    };

    let scan = match builder.build().await {
        Ok(s) => s,
        Err(_) => return DbResponse::error("SL@2"),
    };

    let mut results = ScanResults::new(scan);

    // Skip rows first (don't convert them - saves stack space)
    let mut skipped = 0usize;
    while skipped < skip {
        match results.next_row().await {
            Ok(Some(_)) => skipped += 1,
            Ok(None) => break,
            Err(_) => return DbResponse::error("SL@3"),
        }
    }

    // Now collect only the rows we need (max 4)
    let mut readings: heapless::Vec<SpectralReadingProtocol, 4> = heapless::Vec::new();
    loop {
        match results.next_row().await {
            Ok(Some(row)) => {
                if !readings.is_full() {
                    if let Some(r) = row_to_spectral(&row) {
                        let _ = readings.push(r);
                    }
                }
            }
            Ok(None) => break,
            Err(_) => return DbResponse::error("SL@4"),
        }
    }

    let has_more = total_count as usize > count;
    DbResponse::spectral_readings(&readings, total_count, has_more)
}

async fn query_spectral_range(
    db: &mut EdgeDatabase,
    table_id: u32,
    schema: &Schema,
    start_us: i64,
    end_us: i64,
) -> DbResponse {
    let builder = match ScanBuilder::new(db.storage(), table_id, schema).await {
        Ok(b) => b,
        Err(_) => return DbResponse::error("SR@1:ScanBuilder"),
    };

    let scan = match builder.build().await {
        Ok(s) => s,
        Err(_) => return DbResponse::error("SR@2:build"),
    };

    let mut results = ScanResults::new(scan);

    // Small fixed buffer - only return first 4 matching readings
    let mut readings: heapless::Vec<SpectralReadingProtocol, 4> = heapless::Vec::new();
    let mut total_in_range = 0u32;

    loop {
        match results.next_row().await {
            Ok(Some(row)) => {
                if let Some(reading) = row_to_spectral(&row) {
                    if reading.timestamp_us >= start_us && reading.timestamp_us <= end_us {
                        total_in_range += 1;
                        if !readings.is_full() {
                            let _ = readings.push(reading);
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(_) => return DbResponse::error("SR@3:next_row"),
        }
    }

    let has_more = total_in_range > readings.len() as u32;
    DbResponse::spectral_readings(&readings, total_in_range, has_more)
}

async fn query_spectral_scan(
    db: &mut EdgeDatabase,
    table_id: u32,
    schema: &Schema,
    offset: usize,
) -> DbResponse {
    let builder = match ScanBuilder::new(db.storage(), table_id, schema).await {
        Ok(b) => b,
        Err(_) => return DbResponse::error("SS@1:ScanBuilder"),
    };

    let scan = match builder.build().await {
        Ok(s) => s,
        Err(_) => return DbResponse::error("SS@2:build"),
    };

    let mut results = ScanResults::new(scan);

    // Skip `offset` rows, then collect up to 4 readings
    let mut readings: heapless::Vec<SpectralReadingProtocol, 4> = heapless::Vec::new();
    let mut total_count = 0u32;
    let mut skipped = 0usize;

    loop {
        match results.next_row().await {
            Ok(Some(row)) => {
                total_count += 1;
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if !readings.is_full() {
                    if let Some(reading) = row_to_spectral(&row) {
                        let _ = readings.push(reading);
                    }
                }
            }
            Ok(None) => break,
            Err(_) => return DbResponse::error("SS@3:next_row"),
        }
    }

    let has_more = (offset + readings.len()) < total_count as usize;
    DbResponse::spectral_readings(&readings, total_count, has_more)
}
