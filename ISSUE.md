# AS7343 Spectral Sensor Query Stability Issues

## Summary

The AS7343 spectral sensor integration with Riceberg database has intermittent crashes when querying spectral readings via USB. The sensor data collection and storage works correctly, but retrieving data via `spectral-latest` is unstable.

## What Works

- **Sensor initialization**: AS7343 initializes correctly via I2C
- **Data collection**: Spectral readings are collected every ~1.1 seconds
- **Database writes**: Readings are successfully stored in Riceberg (verified by increasing counts)
- **spectral-stats**: Works reliably after switching to atomic counters (O(1), no scan)
- **spectral-latest**: Works for first few calls, then crashes

## What Doesn't Work

- **spectral-latest** crashes after 3-7 successful calls
- Pattern: works → works → works → crash (non-deterministic)
- Crash manifests as USB device malfunction (panic halts CPU via panic-probe)

## Root Cause Analysis

### 1. Large DbResponse Enum Size

The `DbResponse::SpectralReadings` variant contains:
```rust
SpectralReadings {
    data: heapless::Vec<SpectralReadingProtocol, N>,
    total: u32,
    has_more: bool,
}
```

Each `SpectralReadingProtocol` is ~46 bytes:
- `id: u64` (8 bytes)
- `timestamp_us: i64` (8 bytes)
- `channels: [u16; 14]` (28 bytes)
- `gain: u8` (1 byte)
- `saturated: bool` (1 byte)

With `MAX_SPECTRAL_READINGS_PER_RESPONSE = 16`, the data alone is ~736 bytes.

This large enum is:
1. Created in `process_db_command()`
2. Returned through async state machine
3. Sent through `DB_RESP_CHANNEL` (capacity 4 = potentially 3KB+ static memory)
4. Serialized by postcard

### 2. Async State Machine Size

Embassy async tasks have limited stack. The `query_spectral_latest` function's async state machine must hold:
- `ScanBuilder` state
- `Scan` state
- `ScanResults` state
- Local variables (buffers, counters)
- All across multiple `.await` points

### 3. Possible Riceberg Resource Leak

After several scans, something in riceberg may not be releasing resources properly:
- Scan handles
- Internal page buffers
- B-tree traversal state

### 4. Heap Fragmentation

Using `alloc::vec::Vec` for collecting readings causes heap allocation/deallocation cycles. The `embedded-alloc` allocator may fragment over time, causing allocation failures that manifest as crashes.

## Attempted Mitigations

| Mitigation | Result |
|------------|--------|
| Reduce `MAX_SPECTRAL_READINGS_PER_RESPONSE` from 16 to 4 | Partial improvement |
| Use `heapless::Vec` instead of `alloc::vec::Vec` | Stack overflow on first call |
| Use atomic counters for stats (avoid scan) | **Fixed spectral-stats** |
| Skip-then-collect pattern (reduce stack) | Still crashes after N calls |
| Circular buffer approach | Still crashes |

## Potential Architectural Changes

### Option 1: Streaming Protocol

Instead of batching readings in one response, stream them individually:

```rust
enum DbResponse {
    StreamStart { total: u32 },
    StreamReading(SpectralReadingProtocol),
    StreamEnd,
}
```

Pros:
- Small fixed response size
- Memory-efficient
- Works with any number of readings

Cons:
- Protocol changes required
- Host must handle streaming
- More complex state management

### Option 2: Separate Query Task with Larger Stack

Spawn a dedicated task for queries with a larger stack allocation:

```rust
#[embassy_executor::task(stack_size = 4096)]
async fn query_task(...) { }
```

Pros:
- More stack space for async state machines
- No protocol changes

Cons:
- Uses more RAM
- May just delay the problem

### Option 3: Synchronous Query Handler

Move query handling out of async context entirely:

```rust
fn process_query_sync(db: &mut Database, cmd: DbCommand) -> DbResponse {
    // No async, predictable stack usage
}
```

Pros:
- Predictable memory usage
- No async state machine overhead

Cons:
- Blocks other tasks during query
- May not work with riceberg's async API

### Option 4: Ring Buffer in Static Memory

Pre-allocate a static ring buffer for recent readings:

```rust
static RECENT_READINGS: Mutex<RingBuffer<SpectralReadingProtocol, 32>> = ...;
```

Query from the ring buffer instead of scanning the database.

Pros:
- O(1) query for recent readings
- No scan needed
- Predictable memory

Cons:
- Limited history (only N recent readings)
- Duplicates data (in DB and ring buffer)
- More static memory usage

### Option 5: Reduce SpectralReadingProtocol Size

Pack the protocol struct more efficiently:

```rust
struct SpectralReadingCompact {
    timestamp_ms: u32,        // 4 bytes (ms precision, ~49 days range)
    channels: [u16; 14],      // 28 bytes
    metadata: u8,             // 1 byte (gain + saturated packed)
}  // Total: 33 bytes vs 46 bytes
```

Pros:
- ~30% smaller responses
- May fit more in same memory budget

Cons:
- Loses precision (ms vs us timestamps)
- Protocol change required

## Recommended Next Steps

1. **Investigate riceberg scan resource usage** - Add instrumentation to track scan creation/destruction

2. **Measure actual async state machine sizes** - Use `core::mem::size_of_val` on futures

3. **Consider Option 4 (Ring Buffer)** - Most practical for "latest N readings" use case

4. **Profile heap usage** - Track allocations during query cycles to find leaks

## Environment

- Target: RP2350 (Cortex-M33, 520KB SRAM)
- Heap: 96KB allocated via `embedded-alloc`
- Framework: Embassy 0.9.x (git main)
- Database: Riceberg with `memory-minimal` profile (MAX_INLINE_FIELDS=16)
- Sensor: AS7343 14-channel spectral sensor via I2C

## Related Files

- `examples/riceberg_as7343.rs` - Standalone spectral sensor example
- `src/db_protocol.rs` - Protocol definitions
- `src/adapters/as7343.rs` - Sensor adapter
- `src/domain/spectral.rs` - Domain types
