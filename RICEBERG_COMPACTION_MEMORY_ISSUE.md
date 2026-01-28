# Riceberg Compaction Memory Issue on RP2350

## Summary

Full compaction execution in riceberg-edge causes the RP2350 firmware to crash during USB initialization. The device fails to enumerate as a USB device when the `plan_compaction()` + `compact()` code path is compiled in.

## Symptoms

- Device does not appear as USB serial port after flashing
- Windows reports "USB device not recognized"
- Device requires BOOTSEL mode recovery to reflash

## Root Cause

The compaction execution path in riceberg-edge requires significant stack and/or heap memory that exceeds what's available on the RP2350 during the async runtime initialization. Even though the compaction code is not called at startup, its presence in the binary appears to affect memory layout or static initialization in a way that causes the crash.

Specifically, this code causes the issue:

```rust
// In EdgeStorageAdapter::compact()
match self.db.plan_compaction(table_id, &config).await {
    Ok(Some(task)) => {
        let files_before = task.file_count() as u32;
        match self.db.compact(task).await {  // <-- This causes issues
            Ok(result) => /* ... */,
            Err(_) => /* ... */,
        }
    }
    // ...
}
```

## Working Workaround

The `needs_compaction()` check works fine - it's a lightweight operation:

```rust
async fn compact(&mut self) -> Result<CompactionResult, StorageError> {
    let table_id = self.table_id.ok_or(StorageError::NotInitialized)?;
    let config = CompactionConfig::default();

    // This lightweight check works fine
    let needs_compaction = self.db.needs_compaction(table_id, &config).await;

    if !needs_compaction {
        return Ok(CompactionResult {
            files_before: 0,
            files_after: 0,
            rows_compacted: 0,
            was_needed: false,
        });
    }

    // Report that compaction is needed but don't execute
    Ok(CompactionResult {
        files_before: 1,
        files_after: 0,
        rows_compacted: 0,
        was_needed: true,
    })
}
```

## Alternative: Use `expire` Instead

The `expire` command works correctly and can reclaim significant space:

```
> expire 3

Snapshot Expiration Result:
--------------------------------------------------
Snapshots expired:  28
Pages freed:        32
Space freed:        128 KB
--------------------------------------------------
```

This is often sufficient for embedded use cases where you don't need to keep historical snapshots.

## Potential Solutions

1. **Increase stack size** - The embassy executor stack may need to be larger for compaction
2. **Use a separate compaction task** - Run compaction in a dedicated task with its own stack
3. **Streaming compaction** - Modify riceberg-core to support lower-memory compaction
4. **External compaction** - Perform compaction on a host PC by reading/writing the flash

## Environment

- Target: RP2350 (thumbv8m.main-none-eabihf)
- Framework: Embassy async runtime
- Storage: 512KB flash allocated for database
- riceberg-edge version: local development

## Related Files

- `src/adapters/edge_storage.rs` - StoragePort implementation with workaround
- `examples/hexagonal_temperature.rs` - Firmware that uses the storage adapter
