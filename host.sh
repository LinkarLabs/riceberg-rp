#!/bin/bash
# Convenience script to run the host CLI on Linux/Mac
cargo run --bin rp_filesystem_host --target x86_64-unknown-linux-gnu --features std -- "$@"
