//! Adapters - concrete implementations of ports
//!
//! Adapters connect the domain to the outside world by implementing
//! the port traits. Each adapter knows how to work with a specific
//! technology or hardware.
//!
//! # Available Adapters
//!
//! - **rp2350_temp**: RP2350 onboard temperature sensor via ADC
//! - **edge_storage**: Riceberg EdgeDatabase for flash storage
//! - **usb_cdc**: USB CDC serial communication

pub mod edge_storage;
pub mod rp2350_temp;
pub mod usb_cdc;

pub use edge_storage::EdgeStorageAdapter;
pub use rp2350_temp::Rp2350TempSensor;
pub use usb_cdc::UsbCdcAdapter;
