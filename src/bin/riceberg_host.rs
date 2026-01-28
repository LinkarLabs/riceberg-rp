//! Riceberg Database Host CLI
//!
//! This binary runs on your PC and provides an interactive shell to
//! query the Riceberg database on the RP2350 via USB.
//!
//! ## Usage
//!
//! ```bash
//! # List available serial ports
//! cargo run --bin riceberg_host -- --list-ports
//!
//! # Connect to device (auto-detects RP2350)
//! cargo run --bin riceberg_host
//!
//! # Connect to specific port
//! cargo run --bin riceberg_host -- --port COM3
//! ```
//!
//! ## Commands
//!
//! - `stats` - Show database statistics
//! - `latest <n>` - Show latest N readings (default: 10)
//! - `range <start_us> <end_us>` - Show readings in time range
//! - `scan [offset]` - Scan all readings with optional offset
//! - `filter <field> <op> <value> [...]` - Query with predicate filters
//! - `get <id>` - Get a single reading by ID
//! - `delete <id>` - Delete a reading by ID
//! - `compact` - Run compaction to merge data files
//! - `expire [n]` - Expire old snapshots, keep last N (default: 5)
//! - `capacity` - Show storage capacity information
//! - `diag` - Show system diagnostics
//! - `help` - Show help
//! - `exit` - Exit shell
//!
//! ## Filter Examples
//!
//! ```bash
//! filter temp > 25.0              # Readings where temperature > 25°C
//! filter temp >= 20 temp < 30     # Temperature between 20-30°C
//! filter time > 60000000          # Readings after 60 seconds uptime
//! filter temp > 25 --limit 10     # Top 10 readings above 25°C
//! ```
//!
//! ## Maintenance Commands
//!
//! ```bash
//! compact                         # Merge data files for faster scans
//! expire 3                        # Keep only last 3 snapshots
//! capacity                        # Check available storage space
//! ```

use std::io::{self, Write};
use std::time::Duration;

use rp::db_protocol::{DbCommand, DbResponse, FilterField, FilterOp, FilterValue, QueryFilter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Parse arguments
    if args.contains(&"--list-ports".to_string()) {
        list_ports();
        return Ok(());
    }

    let port_name = if let Some(idx) = args.iter().position(|a| a == "--port") {
        args.get(idx + 1).cloned()
    } else {
        find_rp2350_port()
    };

    let port_name = match port_name {
        Some(name) => name,
        None => {
            eprintln!("Error: No Riceberg device found");
            eprintln!("Use --list-ports to see available ports");
            eprintln!("Or specify port with --port <PORT>");
            return Err("No device found".into());
        }
    };

    // On Windows, COM ports >= 10 need the \\.\COMxx format
    #[cfg(target_os = "windows")]
    let port_name = if port_name.starts_with("COM") && !port_name.starts_with(r"\\") {
        format!(r"\\.\{}", port_name)
    } else {
        port_name
    };

    print!("Connecting to {}...", port_name);
    io::stdout().flush()?;

    let mut port = serialport::new(&port_name, 115200)
        .timeout(Duration::from_millis(30000))  // 30 second timeout for database operations
        .flow_control(serialport::FlowControl::None)
        .open()?;

    println!(" opened!");

    // Set DTR (Data Terminal Ready) - some CDC devices wait for this
    print!("Setting DTR...");
    io::stdout().flush()?;
    port.write_data_terminal_ready(true)?;

    println!(" done!");
    println!("Connected!");
    println!("Waiting for device ready...");

    // Wait for ready response (COBS-encoded, ends with 0x00)
    let mut ready = false;
    let mut rx_buf = vec![0u8; 8192];
    let mut rx_pos = 0;

    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(n) = port.read(&mut rx_buf[rx_pos..]) {
            if n > 0 {
                rx_pos += n;
                // Check if we have a complete COBS message (ends with 0x00)
                if rx_buf[..rx_pos].contains(&0x00) {
                    if let Ok(DbResponse::Ok) = postcard::from_bytes_cobs(&mut rx_buf) {
                        ready = true;
                        break;
                    }
                }
            }
        }
    }

    if !ready {
        println!("Warning: Did not receive ready signal from device");
        println!("Proceeding anyway...");
    } else {
        println!("Device ready!");
    }

    println!("\nRiceberg Database Query Shell");
    println!("Type 'help' for commands, 'exit' to quit\n");

    // Interactive shell
    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        if input == "exit" || input == "quit" {
            break;
        }

        if input == "help" {
            print_help();
            continue;
        }

        // Parse and execute command
        match parse_command(input) {
            Ok(cmd) => {
                if let Err(e) = execute_command(&mut port, cmd) {
                    eprintln!("Error: {}", e);
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }

    println!("Goodbye!");
    Ok(())
}

fn list_ports() {
    println!("Available serial ports:");
    match serialport::available_ports() {
        Ok(ports) => {
            if ports.is_empty() {
                println!("  (none)");
            }
            for port in ports {
                print!("  {}", port.port_name);
                match &port.port_type {
                    serialport::SerialPortType::UsbPort(info) => {
                        println!(" - USB (VID: 0x{:04x}, PID: 0x{:04x})", info.vid, info.pid);
                        if let Some(ref manufacturer) = info.manufacturer {
                            println!("      Manufacturer: {}", manufacturer);
                        }
                        if let Some(ref product) = info.product {
                            println!("      Product: {}", product);
                        }
                        if let Some(ref serial) = info.serial_number {
                            println!("      Serial: {}", serial);
                        }
                    }
                    other => println!(" - {}", port_type_name(other)),
                }
            }
        }
        Err(e) => {
            eprintln!("Error listing ports: {}", e);
        }
    }
}

fn port_type_name(port_type: &serialport::SerialPortType) -> &str {
    match port_type {
        serialport::SerialPortType::UsbPort(_) => "USB",
        serialport::SerialPortType::BluetoothPort => "Bluetooth",
        serialport::SerialPortType::PciPort => "PCI",
        serialport::SerialPortType::Unknown => "Unknown",
    }
}

fn find_rp2350_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;

    for port in ports {
        if let serialport::SerialPortType::UsbPort(info) = &port.port_type {
            // Raspberry Pi vendor ID
            if info.vid == 0x2e8a {
                return Some(port.port_name);
            }
        }
    }

    None
}

fn parse_command(input: &str) -> Result<DbCommand, String> {
    let parts: Vec<&str> = input.split_whitespace().collect();

    if parts.is_empty() {
        return Err("Empty command".to_string());
    }

    match parts[0] {
        "stats" | "info" => Ok(DbCommand::stats()),

        "latest" => {
            let count = if parts.len() > 1 {
                parts[1]
                    .parse::<u16>()
                    .map_err(|_| "Invalid count (must be a positive number)".to_string())?
            } else {
                10
            };
            Ok(DbCommand::query_latest(count))
        }

        "range" => {
            if parts.len() < 3 {
                return Err("Usage: range <start_us> <end_us>".to_string());
            }
            let start_us = parts[1]
                .parse::<i64>()
                .map_err(|_| "Invalid start timestamp".to_string())?;
            let end_us = parts[2]
                .parse::<i64>()
                .map_err(|_| "Invalid end timestamp".to_string())?;
            Ok(DbCommand::query_range(start_us, end_us))
        }

        "scan" | "all" => {
            let offset = if parts.len() > 1 {
                parts[1]
                    .parse::<u32>()
                    .map_err(|_| "Invalid offset".to_string())?
            } else {
                0
            };
            Ok(DbCommand::scan_all(offset))
        }

        "diag" | "diagnostics" => Ok(DbCommand::diagnostics()),

        "delete" | "rm" => {
            if parts.len() < 2 {
                return Err("Usage: delete <id>".to_string());
            }
            let id = parts[1]
                .parse::<u64>()
                .map_err(|_| "Invalid ID (must be a positive number)".to_string())?;
            Ok(DbCommand::delete(id))
        }

        "get" => {
            if parts.len() < 2 {
                return Err("Usage: get <id>".to_string());
            }
            let id = parts[1]
                .parse::<u64>()
                .map_err(|_| "Invalid ID (must be a positive number)".to_string())?;
            Ok(DbCommand::get_by_id(id))
        }

        "filter" | "where" | "query" => {
            parse_filter_command(&parts[1..])
        }

        "compact" => Ok(DbCommand::compact()),

        "expire" => {
            let keep_last = if parts.len() > 1 {
                parts[1]
                    .parse::<u32>()
                    .map_err(|_| "Invalid count (must be a positive number)".to_string())?
            } else {
                5 // Default: keep last 5 snapshots
            };
            Ok(DbCommand::expire(keep_last))
        }

        "capacity" | "cap" | "space" => Ok(DbCommand::capacity()),

        cmd => Err(format!("Unknown command: {}", cmd)),
    }
}

/// Parse a filter command with the syntax:
/// `filter <field> <op> <value> [<field> <op> <value>...] [--limit N] [--offset N]`
///
/// Fields: id, time/timestamp, temp/temperature, sensor
/// Operators: =, !=, <, <=, >, >=, eq, ne, lt, le, gt, ge
fn parse_filter_command(args: &[&str]) -> Result<DbCommand, String> {
    if args.is_empty() {
        return Err("Usage: filter <field> <op> <value> [--limit N] [--offset N]\n\
                    Fields: id, time, temp, sensor\n\
                    Operators: =, !=, <, <=, >, >=".to_string());
    }

    let mut filters: Vec<QueryFilter> = Vec::new();
    let mut limit: Option<u16> = None;
    let mut offset: Option<u32> = None;

    let mut i = 0;
    while i < args.len() {
        // Check for options
        if args[i] == "--limit" || args[i] == "-l" {
            i += 1;
            if i >= args.len() {
                return Err("--limit requires a value".to_string());
            }
            limit = Some(args[i].parse().map_err(|_| "Invalid limit value")?);
            i += 1;
            continue;
        }

        if args[i] == "--offset" || args[i] == "-o" {
            i += 1;
            if i >= args.len() {
                return Err("--offset requires a value".to_string());
            }
            offset = Some(args[i].parse().map_err(|_| "Invalid offset value")?);
            i += 1;
            continue;
        }

        // Parse filter triplet: field op value
        if i + 2 >= args.len() {
            return Err(format!(
                "Incomplete filter at position {}. Expected: <field> <op> <value>",
                i
            ));
        }

        let field = parse_filter_field(args[i])?;
        let op = parse_filter_op(args[i + 1])?;
        let value = parse_filter_value(field, args[i + 2])?;

        filters.push(QueryFilter::new(field, op, value));
        i += 3;
    }

    if filters.is_empty() {
        return Err("At least one filter is required".to_string());
    }

    if filters.len() > 4 {
        return Err("Maximum 4 filters allowed".to_string());
    }

    Ok(DbCommand::query_filtered(filters, limit, offset))
}

fn parse_filter_field(s: &str) -> Result<FilterField, String> {
    match s.to_lowercase().as_str() {
        "id" => Ok(FilterField::Id),
        "time" | "timestamp" | "ts" => Ok(FilterField::TimestampUs),
        "temp" | "temperature" | "t" => Ok(FilterField::TemperatureC),
        "sensor" | "sensor_id" | "sid" => Ok(FilterField::SensorId),
        _ => Err(format!(
            "Unknown field '{}'. Valid fields: id, time, temp, sensor",
            s
        )),
    }
}

fn parse_filter_op(s: &str) -> Result<FilterOp, String> {
    match s {
        "=" | "==" | "eq" => Ok(FilterOp::Eq),
        "!=" | "<>" | "ne" => Ok(FilterOp::Ne),
        "<" | "lt" => Ok(FilterOp::Lt),
        "<=" | "le" => Ok(FilterOp::Le),
        ">" | "gt" => Ok(FilterOp::Gt),
        ">=" | "ge" => Ok(FilterOp::Ge),
        _ => Err(format!(
            "Unknown operator '{}'. Valid operators: =, !=, <, <=, >, >=",
            s
        )),
    }
}

fn parse_filter_value(field: FilterField, s: &str) -> Result<FilterValue, String> {
    match field {
        FilterField::Id | FilterField::TimestampUs => {
            let v = s.parse::<i64>().map_err(|_| {
                format!("Invalid integer value '{}' for field {:?}", s, field)
            })?;
            Ok(FilterValue::Int64(v))
        }
        FilterField::TemperatureC => {
            let v = s.parse::<f32>().map_err(|_| {
                format!("Invalid float value '{}' for temperature", s)
            })?;
            Ok(FilterValue::Float32(v))
        }
        FilterField::SensorId => {
            FilterValue::string(s).ok_or_else(|| "Sensor ID too long".to_string())
        }
    }
}

fn execute_command(
    port: &mut Box<dyn serialport::SerialPort>,
    cmd: DbCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    // Serialize command with COBS encoding (includes 0x00 terminator)
    let cmd_bytes = postcard::to_allocvec_cobs(&cmd)?;

    // Send command
    port.write_all(&cmd_bytes)?;
    port.flush()?;

    // Receive COBS-encoded response (read until 0x00 sentinel byte)
    let mut rx_buf = vec![];
    let mut byte = [0u8; 1];

    loop {
        match port.read(&mut byte) {
            Ok(n) if n == 1 => {
                rx_buf.push(byte[0]);
                // Check for COBS sentinel (message terminator)
                if byte[0] == 0x00 {
                    break;
                }
                // Safety limit to prevent infinite growth
                if rx_buf.len() > 65536 {
                    return Err("Response too large".into());
                }
            }
            Ok(_) => {
                // No data or connection lost
                if !rx_buf.is_empty() {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {
                // Timeout
                if !rx_buf.is_empty() {
                    break;
                }
            }
            Err(e) => {
                return Err(e.into());
            }
        }
    }

    // Decode COBS-encoded response
    let response: DbResponse = postcard::from_bytes_cobs(&mut rx_buf)?;

    // Display response
    display_response(&response);

    Ok(())
}

fn display_response(response: &DbResponse) {
    match response {
        DbResponse::Ok => {
            println!("OK");
        }

        DbResponse::Error { message } => {
            eprintln!("Error: {}", message);
        }

        DbResponse::Readings {
            data,
            total,
            has_more,
        } => {
            println!(
                "\nTemperature Readings ({} of {} total):",
                data.len(),
                total
            );
            println!("{:-<75}", "");
            println!(
                "{:>6} {:<18} {:>12} {:>15}",
                "ID", "Time (since boot)", "Temp (°C)", "Sensor ID"
            );
            println!("{:-<75}", "");

            for reading in data {
                let time_str = format_duration_us(reading.timestamp_us);
                println!(
                    "{:>6} {:<18} {:>12.2} {:>15}",
                    reading.id, time_str, reading.temperature_c, reading.sensor_id
                );
            }

            println!("{:-<75}", "");

            if *has_more {
                println!("(More readings available - use 'scan <offset>' to continue)");
            }
        }

        DbResponse::SingleReading { reading } => {
            match reading {
                Some(r) => {
                    println!("\nReading ID {}:", r.id);
                    println!("{:-<50}", "");
                    println!("Time:        {}", format_duration_us(r.timestamp_us));
                    println!("Temperature: {:.2} °C", r.temperature_c);
                    println!("Sensor:      {}", r.sensor_id);
                    println!("{:-<50}", "");
                }
                None => {
                    println!("Reading not found");
                }
            }
        }

        DbResponse::Deleted { id, success } => {
            if *success {
                println!("Deleted reading ID {}", id);
            } else {
                println!("Failed to delete reading ID {} (not found?)", id);
            }
        }

        DbResponse::Stats {
            total_readings,
            oldest_timestamp_us,
            newest_timestamp_us,
            snapshot_id,
        } => {
            println!("\nDatabase Statistics:");
            println!("{:-<50}", "");
            println!("Total readings:     {}", total_readings);
            println!("Oldest reading:     {}", format_duration_us(*oldest_timestamp_us));
            println!("Newest reading:     {}", format_duration_us(*newest_timestamp_us));
            println!("Snapshot ID:        {}", snapshot_id);

            if *total_readings > 0 {
                let duration_us = newest_timestamp_us - oldest_timestamp_us;
                let duration_sec = duration_us as f64 / 1_000_000.0;
                println!("Time span:          {:.2} seconds", duration_sec);
            }

            println!("{:-<50}", "");
        }

        DbResponse::Diagnostics {
            db_ready,
            sensor_task_running,
            sensor_readings_sent,
            sensor_readings_received,
            db_writes_success,
            db_insert_failed,
            db_commit_failed,
            last_adc_value,
            uptime_ms,
        } => {
            println!("\nSystem Diagnostics:");
            println!("{:-<50}", "");
            println!("Uptime:                 {:.2} seconds", *uptime_ms as f64 / 1000.0);
            println!("DB_READY:               {}", if *db_ready { "YES" } else { "NO" });
            println!("Sensor task running:    {}", if *sensor_task_running { "YES" } else { "NO" });
            println!("Sensor readings sent:   {}", sensor_readings_sent);
            println!("Sensor readings recv:   {}", sensor_readings_received);
            println!("DB writes success:      {}", db_writes_success);
            println!("DB insert failed:       {}", db_insert_failed);
            println!("DB commit failed:       {}", db_commit_failed);
            println!("Last ADC value:         {} (raw)", last_adc_value);
            println!("{:-<50}", "");

            if !*sensor_task_running {
                println!("WARNING: Sensor task is not running!");
            } else if *sensor_readings_sent == 0 {
                println!("WARNING: Sensor task running but no readings sent yet");
            } else if *db_insert_failed > 0 {
                println!("WARNING: {} DB inserts have failed!", db_insert_failed);
            } else if *db_commit_failed > 0 {
                println!("WARNING: {} DB commits have failed!", db_commit_failed);
            } else if sensor_readings_received < sensor_readings_sent {
                println!("NOTE: {} readings pending in channel", sensor_readings_sent - sensor_readings_received);
            }
        }

        DbResponse::Compacted {
            files_before,
            files_after,
            rows_compacted,
            was_needed,
        } => {
            println!("\nCompaction Result:");
            println!("{:-<50}", "");
            if *was_needed && *rows_compacted > 0 {
                println!("Files before:       {}", files_before);
                println!("Files after:        {}", files_after);
                println!("Rows compacted:     {}", rows_compacted);
                println!("Files reduced:      {}", files_before.saturating_sub(*files_after));
            } else if *was_needed {
                println!("Compaction recommended but skipped on embedded");
                println!("(Use 'expire' to reclaim space from old snapshots)");
            } else {
                println!("Compaction not needed (data files below threshold)");
            }
            println!("{:-<50}", "");
        }

        DbResponse::Expired {
            snapshots_expired,
            pages_freed,
        } => {
            println!("\nSnapshot Expiration Result:");
            println!("{:-<50}", "");
            println!("Snapshots expired:  {}", snapshots_expired);
            println!("Pages freed:        {}", pages_freed);
            if *pages_freed > 0 {
                println!("Space freed:        {} KB", pages_freed * 4); // Assuming 4KB pages
            }
            println!("{:-<50}", "");
        }

        DbResponse::Capacity {
            total_pages,
            allocated_pages,
            free_pages,
            used_bytes,
            free_bytes,
        } => {
            println!("\nStorage Capacity:");
            println!("{:-<50}", "");
            println!("Total pages:        {}", total_pages);
            println!("Allocated pages:    {}", allocated_pages);
            println!("Free pages:         {}", free_pages);
            println!("Used storage:       {} KB ({:.1}%)",
                     used_bytes / 1024,
                     (*allocated_pages as f64 / *total_pages as f64) * 100.0);
            println!("Free storage:       {} KB ({:.1}%)",
                     free_bytes / 1024,
                     (*free_pages as f64 / *total_pages as f64) * 100.0);
            println!("{:-<50}", "");
        }
    }
}

/// Format microseconds since boot as human-readable duration (HH:MM:SS.mmm)
fn format_duration_us(us: i64) -> String {
    if us <= 0 {
        return "00:00:00.000".to_string();
    }

    let total_ms = us / 1000;
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let secs = total_secs % 60;
    let total_mins = total_secs / 60;
    let mins = total_mins % 60;
    let hours = total_mins / 60;

    if hours > 0 {
        format!("{:02}:{:02}:{:02}.{:03}", hours, mins, secs, ms)
    } else {
        format!("{:02}:{:02}.{:03}", mins, secs, ms)
    }
}

fn print_help() {
    println!("Commands:");
    println!("  stats                    - Show database statistics");
    println!("  latest [n]               - Show latest N readings (default: 10)");
    println!("  range <start> <end>      - Show readings in time range (microseconds)");
    println!("  scan [offset]            - Scan all readings with optional offset");
    println!("  get <id>                 - Get a single reading by ID");
    println!("  delete <id>              - Delete a reading by ID");
    println!("  filter <conditions>      - Query with predicate filters (see below)");
    println!("  compact                  - Run compaction to merge data files");
    println!("  expire [n]               - Expire old snapshots, keep last N (default: 5)");
    println!("  capacity                 - Show storage capacity information");
    println!("  diag                     - Show system diagnostics (sensor task status)");
    println!("  help                     - Show this help");
    println!("  exit                     - Exit shell");
    println!();
    println!("Filter Syntax:");
    println!("  filter <field> <op> <value> [<field> <op> <value>...] [--limit N] [--offset N]");
    println!();
    println!("  Fields:");
    println!("    id, time (or timestamp, ts), temp (or temperature, t), sensor (or sid)");
    println!();
    println!("  Operators:");
    println!("    =, !=, <, <=, >, >=  (or: eq, ne, lt, le, gt, ge)");
    println!();
    println!("Examples:");
    println!("  stats                           - Show how many readings are stored");
    println!("  latest 5                        - Show last 5 temperature readings");
    println!("  scan 0                          - Show first batch of all readings");
    println!("  get 42                          - Get reading with ID 42");
    println!("  delete 42                       - Delete reading with ID 42");
    println!("  filter temp > 25.0              - Readings where temperature > 25°C");
    println!("  filter temp >= 20 temp < 30     - Temperature between 20-30°C");
    println!("  filter id = 5                   - Get reading with ID 5");
    println!("  filter time > 60000000          - Readings after 60 seconds uptime");
    println!("  filter temp > 25 --limit 5      - Top 5 readings above 25°C");
    println!("  compact                         - Merge small data files for faster scans");
    println!("  expire 3                        - Keep only the last 3 snapshots");
    println!("  capacity                        - Check available storage space");
    println!("  diag                            - Check if sensor task is running");
}
