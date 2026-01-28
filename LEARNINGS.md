# Riceberg on RP2350: Comprehensive Learnings

This document captures all the hard-won knowledge from building an embedded temperature sensor database on RP2350 (Raspberry Pi Pico 2) using Riceberg, Embassy, and USB CDC serial communication.

## Project Overview

**Goal**: Read the internal temperature sensor on RP2350, store readings in a Riceberg database on flash, and provide a USB serial interface for querying the data from a host PC.

**Stack**:
- **MCU**: RP2350 (Raspberry Pi Pico 2)
- **Framework**: Embassy (async embedded Rust)
- **Database**: Riceberg (embedded time-series database)
- **Communication**: USB CDC ACM (serial over USB)
- **Protocol**: Postcard with COBS encoding

---

## Critical Learnings

### 1. Riceberg: MUST Call `notify_commit()` After Commits

**The single most important discovery**: After a successful `commit()`, you MUST call `db.notify_commit(snapshot_id)` to update the database's internal snapshot counter.

```rust
// WRONG - subsequent transactions will fail
match txn.commit().await {
    Ok(snap) => {
        // Missing notify_commit!
        info!("Committed snapshot {}", snap);
    }
    Err(e) => { /* handle error */ }
}

// CORRECT
match txn.commit().await {
    Ok(snap) => {
        db.notify_commit(snap);  // <-- CRITICAL!
        info!("Committed snapshot {}", snap);
    }
    Err(e) => { /* handle error */ }
}
```

**Why this happens**: Riceberg uses Optimistic Concurrency Control (OCC). When you begin a transaction, it captures the current `base_snapshot_id`. On commit, it checks if the database has changed since then. Without `notify_commit()`, the database's internal counter doesn't advance, so the next transaction's OCC check fails because it thinks it's working with a stale snapshot.

**Symptoms**: First write succeeds, all subsequent writes fail with commit errors.

### 2. USB CDC on RP2350: Spawn USB as Dedicated Task

USB enumeration on Windows is timing-sensitive. If the USB task doesn't get enough CPU time during enumeration, Windows shows "device malfunctioned".

**Solution**: Spawn USB as a dedicated task, not joined with other heavy tasks:

```rust
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // ... setup ...

    // Spawn USB as dedicated task for reliable enumeration
    spawner.spawn(usb_task(usb)).unwrap();

    // Other tasks can run in main or be spawned
    join(query_handler(class), db_task(storage)).await;
}

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, Driver<'static, USB>>) {
    usb.run().await;
}
```

**Why this matters**: Embassy's cooperative scheduler gives equal priority to all joined tasks. During USB enumeration, the host sends rapid control transfers that must be answered within milliseconds. If the USB task is competing with CPU-heavy database operations, it may miss these deadlines.

### 3. USB CDC: Use 64-Byte Read Buffer

USB CDC ACM has a maximum packet size of 64 bytes. Reading into a 1-byte buffer causes packet fragmentation issues.

```rust
// WRONG
let mut buf = [0u8; 1];
class.read_packet(&mut buf).await?;

// CORRECT
let mut buf = [0u8; 64];
let n = class.read_packet(&mut buf).await?;
// Process buf[..n]
```

### 4. Database Scans: Isolate Scan Objects in Scopes

Riceberg scan objects hold references to database internals. If they're not dropped before the next operation, you get borrow checker issues or runtime hangs.

```rust
// WRONG - scan lives too long
let results = {
    let scan = ScanBuilder::new(&db, table_id)
        .with_snapshot(snap)
        .build()
        .await?;
    scan.collect::<Vec<_>>().await
};
// scan might still be borrowed here

// CORRECT - explicit scope ensures scan is dropped
let results = {
    let scan = ScanBuilder::new(&db, table_id)
        .with_snapshot(snap)
        .build()
        .await?;
    let data: Vec<_> = scan.collect().await;
    data  // scan dropped here before leaving scope
};
```

### 5. Serialize All Database Operations

Riceberg is not thread-safe for concurrent writes. Use a channel-based architecture to funnel all DB operations through a single task:

```rust
// Channel for sensor readings
static READINGS_CHANNEL: Channel<CriticalSectionRawMutex, SensorReading, 32> = Channel::new();

// Channel for query commands/responses
static DB_CMD_CHANNEL: Channel<CriticalSectionRawMutex, DbCommand, 4> = Channel::new();
static DB_RESP_CHANNEL: Channel<CriticalSectionRawMutex, DbResponse, 4> = Channel::new();

// Single DB task handles ALL database operations
async fn db_task(storage: EdgeStorage) {
    let db = Database::create_or_open(storage, config).await.unwrap();

    loop {
        // Check for sensor readings (non-blocking)
        if let Ok(reading) = READINGS_CHANNEL.try_receive() {
            // Write to DB
        }

        // Check for query commands (non-blocking)
        if let Ok(cmd) = DB_CMD_CHANNEL.try_receive() {
            let response = process_query(&db, cmd).await;
            DB_RESP_CHANNEL.send(response).await;
        }

        Timer::after(Duration::from_millis(10)).await;
    }
}
```

### 6. RP2350 Temperature Sensor: Empirical Calibration Required

The standard RP2040 temperature formula does NOT work on RP2350:

```rust
// RP2040 formula (DOES NOT WORK on RP2350)
let voltage = adc_value as f32 * 3.3 / 4096.0;
let temp = 27.0 - (voltage - 0.706) / 0.001721;
// Gives ~410°C at room temperature!

// RP2350 empirical formula (WORKS)
let temp = adc_value as f32 * 0.474;
// ADC ~57 at room temperature → ~27°C
```

**Observation**: On RP2350, the internal temperature sensor ADC returns values around 50-60 at room temperature, not the expected ~876. This is likely due to different ADC reference voltage or sensor characteristics on RP2350 vs RP2040.

### 7. COBS Encoding for Reliable Framing

Use COBS (Consistent Overhead Byte Stuffing) encoding for USB serial messages. COBS guarantees no zero bytes in the payload, using 0x00 as the message delimiter.

```rust
// Device side: encode response
let response_bytes = postcard::to_vec_cobs::<_, 8192>(&response)?;
class.write_packet(&response_bytes).await?;

// Host side: read until 0x00 sentinel
let mut rx_buf = vec![];
loop {
    let n = port.read(&mut byte)?;
    rx_buf.push(byte[0]);
    if byte[0] == 0x00 {
        break;  // Complete message received
    }
}
let response: DbResponse = postcard::from_bytes_cobs(&mut rx_buf)?;
```

### 8. Windows COM Port Quirks

Windows caches USB device descriptors by VID/PID/Serial. To force Windows to recognize firmware changes:

1. Use a version number in the USB serial string:
   ```rust
   const USB_SERIAL_VERSION: u32 = 70;
   // ...
   config.serial_number = Some("RICE70");
   ```

2. COM ports >= 10 need special path format:
   ```rust
   let port_name = if port_name.starts_with("COM") {
       format!(r"\\.\{}", port_name)  // \\.\COM10
   } else {
       port_name
   };
   ```

### 9. Debugging Without RTT Probe

When you don't have an RTT probe, add USB-based diagnostics:

```rust
// Atomic counters for diagnostics
static SENSOR_READINGS_SENT: AtomicU32 = AtomicU32::new(0);
static SENSOR_READINGS_RECEIVED: AtomicU32 = AtomicU32::new(0);
static DB_WRITES_SUCCESS: AtomicU32 = AtomicU32::new(0);
static DB_INSERT_FAILED: AtomicU32 = AtomicU32::new(0);
static DB_COMMIT_FAILED: AtomicU32 = AtomicU32::new(0);
static LAST_ADC_VALUE: AtomicU16 = AtomicU16::new(0);

// Add Diagnostics command to protocol
pub enum DbCommand {
    // ... other commands ...
    Diagnostics,
}

pub enum DbResponse {
    // ... other responses ...
    Diagnostics {
        db_ready: bool,
        sensor_task_running: bool,
        sensor_readings_sent: u32,
        sensor_readings_received: u32,
        db_writes_success: u32,
        db_insert_failed: u32,
        db_commit_failed: u32,
        last_adc_value: u16,
        uptime_ms: u64,
    },
}
```

This lets you query system state via USB when you can't see RTT logs.

### 10. Embassy Task Spawning

Embassy task functions must be `'static` and take owned parameters:

```rust
// Tasks are defined with the macro
#[embassy_executor::task]
async fn sensor_task(mut adc: Adc<'static, Async>, mut ts: AdcChannel<'static>) {
    loop {
        let value = adc.read(&mut ts).await.unwrap();
        // ...
    }
}

// Spawning returns Result, but the task token is consumed
spawner.spawn(sensor_task(adc, ts)).unwrap();
```

### 11. Flash Storage Configuration

For RP2350 with 2MB flash, reserve space for code and use the upper portion for the database:

```rust
// Flash layout:
// 0x000000 - 0x17FFFF: Code (1.5MB)
// 0x180000 - 0x1FFFFF: Database (512KB)

const FLASH_OFFSET: u32 = 0x180000;  // 1.5MB offset
const FLASH_SIZE: u32 = 512 * 1024;  // 512KB for DB
```

### 12. Heap Size for USB + Database

USB buffers and database operations need significant heap space:

```rust
const HEAP_SIZE: usize = 96 * 1024;  // 96KB heap

#[global_allocator]
static HEAP: Heap = Heap::empty();

// Initialize early in main()
{
    static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
    unsafe { HEAP.init(HEAP_MEM.as_ptr() as usize, HEAP_SIZE) }
}
```

---

## Architecture Summary

```
┌─────────────────────────────────────────────────────────────────┐
│                         RP2350 Firmware                          │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐     ┌─────────────────────────────────────┐  │
│  │  USB Task    │     │           DB Task                    │  │
│  │  (spawned)   │     │  - Owns database instance            │  │
│  │              │     │  - Processes sensor readings         │  │
│  │  usb.run()   │     │  - Handles query commands            │  │
│  └──────────────┘     │  - Calls notify_commit() !           │  │
│                       └─────────────────────────────────────┘  │
│         ▲                        ▲           │                  │
│         │                        │           │                  │
│  ┌──────┴──────┐          ┌──────┴───┐  ┌───▼────┐             │
│  │Query Handler│◄────────►│CMD_CHAN  │  │RESP_CH │             │
│  │             │          └──────────┘  └────────┘             │
│  │ CDC Class   │                                                │
│  │ COBS decode │          ┌──────────────────────┐             │
│  │ COBS encode │          │   READINGS_CHANNEL   │             │
│  └─────────────┘          └──────────▲───────────┘             │
│                                      │                          │
│                           ┌──────────┴───────────┐             │
│                           │    Sensor Task       │             │
│                           │  - Reads ADC         │             │
│                           │  - Sends to channel  │             │
│                           └──────────────────────┘             │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ USB Serial
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                         Host CLI                                 │
│  - Connects to COM port                                          │
│  - COBS encode commands                                          │
│  - COBS decode responses                                         │
│  - Interactive shell: stats, latest, scan, diag                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## Common Pitfalls

| Problem | Symptom | Solution |
|---------|---------|----------|
| Missing `notify_commit()` | First write works, rest fail | Always call `db.notify_commit(snap)` after commit |
| USB enumeration fails | "Device malfunctioned" on Windows | Spawn USB as dedicated task |
| 1-byte USB buffer | Packet fragmentation, data corruption | Use 64-byte buffer for `read_packet()` |
| Scan object lives too long | Hang on second query | Use explicit scope to drop scan before next operation |
| Concurrent DB access | Random failures, corruption | Use channels to serialize all DB operations |
| Wrong temp formula | Readings show 400°C | Use empirical formula: `adc * 0.474` |
| Windows caches device | Old firmware behavior persists | Change USB serial number with each version |

---

## Protocol Reference

### Commands (Host → Device)

| Command | Description |
|---------|-------------|
| `QueryLatest { count }` | Get last N readings |
| `QueryRange { start_us, end_us }` | Get readings in time range |
| `Stats` | Get database statistics |
| `ScanAll { offset }` | Paginated scan of all readings |
| `Diagnostics` | Get system health info |

### Responses (Device → Host)

| Response | Description |
|----------|-------------|
| `Ok` | Success (no data) |
| `Error { message }` | Operation failed |
| `Readings { data, total, has_more }` | Query results |
| `Stats { total_readings, oldest_timestamp_us, newest_timestamp_us, snapshot_id }` | DB statistics |
| `Diagnostics { ... }` | System health counters |

---

## File Structure

```
rp/
├── Cargo.toml              # Project configuration
├── LEARNINGS.md            # This file
├── examples/
│   └── riceberg_temperature_usb.rs  # Main firmware
└── src/
    ├── lib.rs              # Library exports
    ├── db_protocol.rs      # Shared protocol definitions
    └── bin/
        └── riceberg_host.rs  # Host CLI tool
```

---

## Building and Flashing

```bash
# Build and flash firmware (requires probe-rs)
cargo run --example riceberg_temperature_usb --release

# Build host CLI
cargo build --bin riceberg_host --release

# Run host CLI
cargo run --bin riceberg_host -- --port COM35
# or auto-detect RP2350:
cargo run --bin riceberg_host
```

---

## Version History

| Version | Changes |
|---------|---------|
| RICE67 | Fixed commit failures by adding `notify_commit()` |
| RICE68 | Added `last_adc_value` to diagnostics |
| RICE69 | Started fixing temperature formula |
| RICE70 | Final temperature formula: `adc * 0.474` |

---

## Dependencies

- **riceberg-core**: Embedded database engine
- **riceberg-storage**: Flash storage adapter
- **embassy-rp**: RP2350 HAL with async support
- **embassy-usb**: USB device stack
- **embassy-sync**: Async primitives (Mutex, Channel)
- **postcard**: Compact serialization with COBS support
- **heapless**: Stack-allocated collections for no_std

---

## Future Improvements

1. **Calibration storage**: Store temperature calibration in flash
2. **Multiple sensors**: Support external I2C/SPI temperature sensors
3. **Data export**: Add command to export all readings as CSV
4. **Compression**: Compress old readings to save flash space
5. **Time sync**: Accept host timestamp for real-time correlation
