//! Hexagonal Temperature Sensor with USB Query Interface
//!
//! This example demonstrates a clean hexagonal architecture for an embedded
//! temperature sensor application using riceberg-edge for storage.
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
//! - **EdgeDatabase handles commit+refresh internally** - No more manual `notify_commit()`!
//! - **Testable** - Ports allow mocking sensors, storage, communication
//! - **Extensible** - Easy to add I2C/SPI sensors by implementing SensorPort
//! - **Maintainable** - Clear separation of concerns

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate alloc;

use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

use riceberg_core::DatabaseConfig;
use riceberg_edge::EdgeDatabase;
use riceberg_storage::{BlockDeviceStorage, NorFlashBlockDevice};

use rp::adapters::{EdgeStorageAdapter, Rp2350TempSensor, UsbCdcAdapter};
use rp::db_protocol::{DbCommand, DbResponse, TemperatureReading};
use rp::domain::SensorReading;
use rp::ports::communication::CommunicationPort;
use rp::ports::sensor::SensorPort;
use rp::ports::storage::StoragePort;

// ============================================================================
// Memory Configuration
// ============================================================================

#[global_allocator]
static HEAP: Heap = Heap::empty();

const HEAP_SIZE: usize = 96 * 1024; // 96KB heap

// ============================================================================
// Flash Storage Configuration
// ============================================================================

/// USB Serial version - increment to force Windows to see device as new
const USB_SERIAL_VERSION: u32 = 80; // Bumped for hexagonal architecture

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

// ============================================================================
// Type Aliases
// ============================================================================

type FlashDevice = Flash<'static, peripherals::FLASH, Async, { 2 * 1024 * 1024 }>;
type FlashStorage = BlockDeviceStorage<NorFlashBlockDevice<FlashDevice>>;

// ============================================================================
// Channels for Inter-Task Communication
// ============================================================================

/// Channel for sending sensor readings to DB task
static READINGS_CHANNEL: Channel<CriticalSectionRawMutex, SensorReading, 32> = Channel::new();

/// Channel for sending DB commands from query handler to DB task
static DB_CMD_CHANNEL: Channel<CriticalSectionRawMutex, DbCommand, 4> = Channel::new();

/// Channel for receiving DB responses from DB task
static DB_RESP_CHANNEL: Channel<CriticalSectionRawMutex, DbResponse, 4> = Channel::new();

// ============================================================================
// Diagnostics Counters
// ============================================================================

static DB_READY: AtomicBool = AtomicBool::new(false);
static SENSOR_READINGS_RECEIVED: AtomicU32 = AtomicU32::new(0);
static SENSOR_READINGS_SENT: AtomicU32 = AtomicU32::new(0);
static SENSOR_TASK_RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_ADC_VALUE: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);
static DB_WRITES_SUCCESS: AtomicU32 = AtomicU32::new(0);
static DB_WRITES_FAILED: AtomicU32 = AtomicU32::new(0);

// ============================================================================
// Interrupt Bindings
// ============================================================================

bind_interrupts!(struct Irqs {
    ADC_IRQ_FIFO => embassy_rp::adc::InterruptHandler;
    USBCTRL_IRQ => UsbInterruptHandler<peripherals::USB>;
});

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

    info!("=== Hexagonal Temperature Sensor ===");
    info!("Architecture: Domain -> Ports -> Adapters");

    // Initialize peripherals
    let p = embassy_rp::init(Default::default());

    // ========================================================================
    // Create Adapters
    // ========================================================================

    // Sensor Adapter: RP2350 onboard temperature sensor
    let adc = Adc::new_blocking(p.ADC, embassy_rp::adc::Config::default());
    let temp_channel = AdcChannel::new_temp_sensor(p.ADC_TEMP_SENSOR);
    let sensor = Rp2350TempSensor::new(adc, temp_channel);
    info!("Sensor adapter created (RP2350 onboard)");

    // Storage Adapter: Flash-based EdgeDatabase
    info!("Initializing flash storage...");
    let flash = Flash::<_, Async, { 2 * 1024 * 1024 }>::new(p.FLASH, p.DMA_CH0);
    let block_device = NorFlashBlockDevice::new(flash, FLASH_OFFSET, FLASH_SIZE);
    let storage = BlockDeviceStorage::new(block_device, FLASH_PAGES_4KB);

    let db_config = DatabaseConfig {
        initial_pages: FLASH_PAGES_4KB,
        ..Default::default()
    };

    let edge_db = EdgeDatabase::create(storage, db_config)
        .await
        .expect("Failed to create database");
    let storage_adapter = EdgeStorageAdapter::new(edge_db);
    info!("Storage adapter created (EdgeDatabase)");

    // Communication Adapter: USB CDC
    info!("Setting up USB...");
    let usb_class = setup_usb(&spawner, p.USB);
    // Note: UsbCdcAdapter is created inside query_handler since it needs the class
    info!("USB setup complete");

    // ========================================================================
    // Spawn Tasks
    // ========================================================================

    info!("Spawning tasks...");

    // Spawn DB task (owns storage adapter)
    spawner.spawn(db_task(storage_adapter).expect("db task"));

    // Spawn sensor task (owns sensor adapter)
    spawner.spawn(sensor_task(sensor).expect("sensor task"));

    info!("All tasks spawned!");

    // Query handler runs in main context
    query_handler(usb_class).await;
}

// ============================================================================
// USB Setup
// ============================================================================

fn setup_usb(
    spawner: &embassy_executor::Spawner,
    usb: embassy_rp::Peri<'static, peripherals::USB>,
) -> CdcAcmClass<'static, Driver<'static, peripherals::USB>> {
    // Build serial number with version
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

    let driver = Driver::new(usb, Irqs);

    let mut config = Config::new(0x2e8a, 0x000b);
    config.manufacturer = Some("Raspberry Pi");
    config.product = Some("Hexagonal Temperature Sensor");
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

    // Spawn USB device task (separate for reliable enumeration)
    spawner.spawn(usb_device_task(usb).expect("usb device task"));

    class
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
// Database Task (uses StoragePort)
// ============================================================================

#[embassy_executor::task]
async fn db_task(mut storage: EdgeStorageAdapter<FlashStorage>) {
    info!("DB task started - initializing storage...");

    // Initialize storage (creates table)
    if let Err(e) = storage.initialize().await {
        error!("Failed to initialize storage: {:?}", e);
        loop {
            Timer::after(Duration::from_secs(3600)).await;
        }
    }
    info!("Storage initialized");

    // Write test readings using StoragePort
    for i in 0..3 {
        info!("Writing TEST reading #{}...", i + 1);
        let reading = SensorReading::new(
            embassy_time::Instant::now().as_micros() as i64,
            25.0 + i as f32,
            rp::domain::SensorId::TEST,
        );

        match storage.store(&reading).await {
            Ok(()) => {
                info!("TEST #{} SUCCESS", i + 1);
                DB_WRITES_SUCCESS.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                info!("TEST #{} FAILED: {:?}", i + 1, e);
                DB_WRITES_FAILED.fetch_add(1, Ordering::Relaxed);
            }
        }
        Timer::after(Duration::from_millis(100)).await;
    }

    DB_READY.store(true, Ordering::Release);
    info!("DB task ready");

    // Main loop: handle sensor writes and query requests
    loop {
        // Check for query commands (priority)
        if let Ok(cmd) = DB_CMD_CHANNEL.try_receive() {
            let response = process_command(&mut storage, cmd).await;
            DB_RESP_CHANNEL.send(response).await;
            continue;
        }

        // Check for sensor readings
        if let Ok(reading) = READINGS_CHANNEL.try_receive() {
            SENSOR_READINGS_RECEIVED.fetch_add(1, Ordering::Relaxed);

            match storage.store(&reading).await {
                Ok(()) => {
                    DB_WRITES_SUCCESS.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    DB_WRITES_FAILED.fetch_add(1, Ordering::Relaxed);
                }
            }
            continue;
        }

        // Nothing to do, yield briefly
        Timer::after(Duration::from_millis(10)).await;
    }
}

/// Process a database command using StoragePort
async fn process_command(
    storage: &mut EdgeStorageAdapter<FlashStorage>,
    cmd: DbCommand,
) -> DbResponse {
    match cmd {
        DbCommand::Stats => match storage.stats().await {
            Ok(stats) => DbResponse::Stats {
                total_readings: stats.total_readings,
                oldest_timestamp_us: stats.oldest_timestamp_us,
                newest_timestamp_us: stats.newest_timestamp_us,
                snapshot_id: stats.snapshot_id,
            },
            Err(_) => DbResponse::error("Failed to get stats"),
        },
        DbCommand::QueryLatest { count } => match storage.get_latest(count).await {
            Ok(readings) => {
                let total = readings.len() as u32;
                let temp_readings: heapless::Vec<TemperatureReading, 32> = readings
                    .iter()
                    .filter_map(|r| TemperatureReading::new(r.id, r.timestamp_us, r.temperature_c, r.sensor_id.as_str()))
                    .collect();
                DbResponse::readings(&temp_readings, total, false)
            }
            Err(_) => DbResponse::error("Query failed"),
        },
        DbCommand::QueryRange { start_us, end_us } => {
            match storage.get_range(start_us, end_us).await {
                Ok(readings) => {
                    let total = readings.len() as u32;
                    let temp_readings: heapless::Vec<TemperatureReading, 32> = readings
                        .iter()
                        .filter_map(|r| TemperatureReading::new(r.id, r.timestamp_us, r.temperature_c, r.sensor_id.as_str()))
                        .collect();
                    DbResponse::readings(&temp_readings, total, false)
                }
                Err(_) => DbResponse::error("Range query failed"),
            }
        }
        DbCommand::ScanAll { offset } => match storage.scan_all(offset).await {
            Ok((readings, total, has_more)) => {
                let temp_readings: heapless::Vec<TemperatureReading, 32> = readings
                    .iter()
                    .filter_map(|r| TemperatureReading::new(r.id, r.timestamp_us, r.temperature_c, r.sensor_id.as_str()))
                    .collect();
                DbResponse::readings(&temp_readings, total, has_more)
            }
            Err(_) => DbResponse::error("Scan failed"),
        },
        DbCommand::Delete { id } => match storage.delete(id).await {
            Ok(success) => DbResponse::Deleted { id, success },
            Err(_) => DbResponse::error("Delete failed"),
        },
        DbCommand::GetById { id } => match storage.get_by_id(id).await {
            Ok(Some(r)) => {
                let reading = TemperatureReading::new(r.id, r.timestamp_us, r.temperature_c, r.sensor_id.as_str());
                DbResponse::SingleReading { reading }
            }
            Ok(None) => DbResponse::SingleReading { reading: None },
            Err(_) => DbResponse::error("Get by ID failed"),
        },
        DbCommand::Diagnostics => DbResponse::Diagnostics {
            db_ready: DB_READY.load(Ordering::Relaxed),
            sensor_task_running: SENSOR_TASK_RUNNING.load(Ordering::Relaxed),
            sensor_readings_sent: SENSOR_READINGS_SENT.load(Ordering::Relaxed),
            sensor_readings_received: SENSOR_READINGS_RECEIVED.load(Ordering::Relaxed),
            db_writes_success: DB_WRITES_SUCCESS.load(Ordering::Relaxed),
            db_insert_failed: DB_WRITES_FAILED.load(Ordering::Relaxed),
            db_commit_failed: 0, // EdgeDatabase handles commits internally
            last_adc_value: LAST_ADC_VALUE.load(Ordering::Relaxed),
            uptime_ms: embassy_time::Instant::now().as_millis(),
        },
        DbCommand::QueryFiltered { filters, limit, offset } => {
            match storage.query_filtered(&filters, limit, offset).await {
                Ok((readings, total, has_more)) => {
                    let temp_readings: heapless::Vec<TemperatureReading, 32> = readings
                        .iter()
                        .filter_map(|r| TemperatureReading::new(r.id, r.timestamp_us, r.temperature_c, r.sensor_id.as_str()))
                        .collect();
                    DbResponse::readings(&temp_readings, total, has_more)
                }
                Err(_) => DbResponse::error("Filtered query failed"),
            }
        }
        DbCommand::Compact => {
            match storage.compact().await {
                Ok(result) => DbResponse::Compacted {
                    files_before: result.files_before,
                    files_after: result.files_after,
                    rows_compacted: result.rows_compacted,
                    was_needed: result.was_needed,
                },
                Err(_) => DbResponse::error("Compaction failed"),
            }
        }
        DbCommand::Expire { keep_last } => {
            match storage.expire(keep_last).await {
                Ok(result) => DbResponse::Expired {
                    snapshots_expired: result.snapshots_expired,
                    pages_freed: result.pages_freed,
                },
                Err(_) => DbResponse::error("Expire failed"),
            }
        }
        DbCommand::Capacity => {
            let cap = storage.capacity();
            const PAGE_SIZE: u64 = 4096;
            DbResponse::Capacity {
                total_pages: cap.total_pages,
                allocated_pages: cap.allocated_pages,
                free_pages: cap.free_pages,
                used_bytes: cap.allocated_pages as u64 * PAGE_SIZE,
                free_bytes: cap.free_pages as u64 * PAGE_SIZE,
            }
        }
    }
}

// ============================================================================
// Sensor Task (uses SensorPort)
// ============================================================================

#[embassy_executor::task]
async fn sensor_task(mut sensor: Rp2350TempSensor<'static>) {
    SENSOR_TASK_RUNNING.store(true, Ordering::Release);
    info!("Sensor task started");

    // Wait for database to be ready
    while !DB_READY.load(Ordering::Acquire) {
        Timer::after(Duration::from_millis(500)).await;
    }
    info!("Sensor task: DB ready, starting readings");

    loop {
        // Use SensorPort trait to read
        match sensor.read().await {
            Ok(reading) => {
                // Store last raw value for diagnostics
                if let Some(raw) = sensor.last_raw_value() {
                    LAST_ADC_VALUE.store(raw, Ordering::Relaxed);
                }

                // Send to DB task via channel
                if READINGS_CHANNEL.try_send(reading).is_ok() {
                    SENSOR_READINGS_SENT.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(e) => {
                warn!("Sensor read failed: {:?}", e);
            }
        }

        Timer::after(Duration::from_secs(READING_INTERVAL_SECS)).await;
    }
}

// ============================================================================
// Query Handler (uses CommunicationPort)
// ============================================================================

async fn query_handler(class: CdcAcmClass<'static, Driver<'static, peripherals::USB>>) {
    info!("Query handler started");

    let mut comm = UsbCdcAdapter::new(class);

    loop {
        comm.wait_connection().await;
        info!("USB connected");

        // Wait for database to be ready
        while !DB_READY.load(Ordering::Acquire) {
            Timer::after(Duration::from_millis(100)).await;
        }

        // Send ready signal
        if let Err(e) = comm.send_ready().await {
            warn!("Failed to send ready: {:?}", e);
            continue;
        }
        info!("Ready sent");

        // Command loop
        loop {
            match comm.receive_command().await {
                Ok(Some(cmd)) => {
                    // Forward to DB task
                    DB_CMD_CHANNEL.send(cmd).await;
                    let response = DB_RESP_CHANNEL.receive().await;

                    // Send response
                    if let Err(e) = comm.send_response(&response).await {
                        warn!("Failed to send response: {:?}", e);
                        break;
                    }
                }
                Ok(None) => {
                    // No command, connection might be lost
                    break;
                }
                Err(e) => {
                    warn!("Command receive error: {:?}", e);
                    break;
                }
            }
        }

        info!("USB disconnected");
    }
}
