//! Spectral reading domain entity
//!
//! This module defines the core domain entity for spectral readings
//! from the AS7343 14-channel spectral sensor.

use crate::domain::SensorId;

/// A spectral reading from the AS7343 14-channel spectral sensor.
///
/// The AS7343 measures light across 14 channels covering violet to near-infrared,
/// plus clear and flicker detection channels.
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct SpectralReading {
    /// Unique reading ID (0 means not yet assigned)
    pub id: u64,
    /// Timestamp in microseconds since boot
    pub timestamp_us: i64,
    /// Sensor identifier
    pub sensor_id: SensorId,

    // 12 spectral channels (wavelength ordered, violet to NIR)
    /// F1: 405nm - Violet
    pub f1_405nm: u16,
    /// F2: 425nm - Violet-Blue
    pub f2_425nm: u16,
    /// FZ: 450nm - Blue (CIE Z reference)
    pub fz_450nm: u16,
    /// F3: 475nm - Blue
    pub f3_475nm: u16,
    /// F4: 515nm - Cyan-Green
    pub f4_515nm: u16,
    /// F5: 550nm - Green
    pub f5_550nm: u16,
    /// FY: 555nm - Yellow (CIE Y reference)
    pub fy_555nm: u16,
    /// FXL: 600nm - Orange (CIE X reference)
    pub fxl_600nm: u16,
    /// F6: 640nm - Red
    pub f6_640nm: u16,
    /// F7: 690nm - Deep Red
    pub f7_690nm: u16,
    /// F8: 745nm - Near-IR edge
    pub f8_745nm: u16,
    /// NIR: 855nm - Near-Infrared
    pub nir_855nm: u16,

    // Utility channels
    /// Clear channel (broadband visible light)
    pub clear: u16,
    /// Flicker detection channel
    pub flicker: u16,

    // Metadata
    /// Gain setting used for this reading (0-10, maps to AS7343 gain values)
    pub gain: u8,
    /// Whether any channel was saturated during measurement
    pub saturated: bool,
}

impl SpectralReading {
    /// Create a new spectral reading (ID will be assigned on storage)
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        timestamp_us: i64,
        sensor_id: SensorId,
        f1_405nm: u16,
        f2_425nm: u16,
        fz_450nm: u16,
        f3_475nm: u16,
        f4_515nm: u16,
        f5_550nm: u16,
        fy_555nm: u16,
        fxl_600nm: u16,
        f6_640nm: u16,
        f7_690nm: u16,
        f8_745nm: u16,
        nir_855nm: u16,
        clear: u16,
        flicker: u16,
        gain: u8,
        saturated: bool,
    ) -> Self {
        Self {
            id: 0,
            timestamp_us,
            sensor_id,
            f1_405nm,
            f2_425nm,
            fz_450nm,
            f3_475nm,
            f4_515nm,
            f5_550nm,
            fy_555nm,
            fxl_600nm,
            f6_640nm,
            f7_690nm,
            f8_745nm,
            nir_855nm,
            clear,
            flicker,
            gain,
            saturated,
        }
    }

    /// Create a spectral reading with a known ID (from database)
    #[allow(clippy::too_many_arguments)]
    pub const fn with_id(
        id: u64,
        timestamp_us: i64,
        sensor_id: SensorId,
        f1_405nm: u16,
        f2_425nm: u16,
        fz_450nm: u16,
        f3_475nm: u16,
        f4_515nm: u16,
        f5_550nm: u16,
        fy_555nm: u16,
        fxl_600nm: u16,
        f6_640nm: u16,
        f7_690nm: u16,
        f8_745nm: u16,
        nir_855nm: u16,
        clear: u16,
        flicker: u16,
        gain: u8,
        saturated: bool,
    ) -> Self {
        Self {
            id,
            timestamp_us,
            sensor_id,
            f1_405nm,
            f2_425nm,
            fz_450nm,
            f3_475nm,
            f4_515nm,
            f5_550nm,
            fy_555nm,
            fxl_600nm,
            f6_640nm,
            f7_690nm,
            f8_745nm,
            nir_855nm,
            clear,
            flicker,
            gain,
            saturated,
        }
    }

    /// Get all spectral channels as an array (wavelength ordered)
    ///
    /// Order: F1, F2, FZ, F3, F4, F5, FY, FXL, F6, F7, F8, NIR
    pub const fn spectral_channels(&self) -> [u16; 12] {
        [
            self.f1_405nm,
            self.f2_425nm,
            self.fz_450nm,
            self.f3_475nm,
            self.f4_515nm,
            self.f5_550nm,
            self.fy_555nm,
            self.fxl_600nm,
            self.f6_640nm,
            self.f7_690nm,
            self.f8_745nm,
            self.nir_855nm,
        ]
    }

    /// Get all channels as an array (including clear and flicker)
    ///
    /// Order: F1, F2, FZ, F3, F4, F5, FY, FXL, F6, F7, F8, NIR, Clear, Flicker
    pub const fn all_channels(&self) -> [u16; 14] {
        [
            self.f1_405nm,
            self.f2_425nm,
            self.fz_450nm,
            self.f3_475nm,
            self.f4_515nm,
            self.f5_550nm,
            self.fy_555nm,
            self.fxl_600nm,
            self.f6_640nm,
            self.f7_690nm,
            self.f8_745nm,
            self.nir_855nm,
            self.clear,
            self.flicker,
        ]
    }

    /// Channel names for display/logging
    pub const CHANNEL_NAMES: [&'static str; 14] = [
        "F1_405nm",
        "F2_425nm",
        "FZ_450nm",
        "F3_475nm",
        "F4_515nm",
        "F5_550nm",
        "FY_555nm",
        "FXL_600nm",
        "F6_640nm",
        "F7_690nm",
        "F8_745nm",
        "NIR_855nm",
        "Clear",
        "Flicker",
    ];

    /// Channel wavelengths in nm (0 for clear/flicker)
    pub const CHANNEL_WAVELENGTHS: [u16; 14] = [
        405, 425, 450, 475, 515, 550, 555, 600, 640, 690, 745, 855, 0, 0,
    ];
}
