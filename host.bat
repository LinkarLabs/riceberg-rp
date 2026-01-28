@echo off
REM Convenience script to run the host CLI on Windows
cargo run --bin rp_filesystem_host --target x86_64-pc-windows-msvc --features std -- %*
