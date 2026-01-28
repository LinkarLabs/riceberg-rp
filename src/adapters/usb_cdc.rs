//! USB CDC communication adapter
//!
//! This adapter implements the CommunicationPort trait for USB CDC ACM
//! (serial over USB) communication.

use crate::db_protocol::{DbCommand, DbResponse};
use crate::ports::communication::{CommunicationError, CommunicationPort};
use embassy_usb::class::cdc_acm::CdcAcmClass;
use heapless::Vec as HeaplessVec;

/// Maximum message size for USB communication
const MAX_MSG_SIZE: usize = 8192;

/// USB packet size (CDC ACM max)
const USB_PACKET_SIZE: usize = 64;

/// USB CDC communication adapter
///
/// Implements COBS-encoded postcard serialization over USB CDC ACM.
pub struct UsbCdcAdapter<'a, D: embassy_usb::driver::Driver<'a>> {
    /// USB CDC ACM class instance
    class: CdcAcmClass<'a, D>,
}

impl<'a, D: embassy_usb::driver::Driver<'a>> UsbCdcAdapter<'a, D> {
    /// Create a new USB CDC adapter
    pub fn new(class: CdcAcmClass<'a, D>) -> Self {
        Self { class }
    }

    /// Get the underlying CdcAcmClass (for advanced operations)
    pub fn class(&self) -> &CdcAcmClass<'a, D> {
        &self.class
    }

    /// Get mutable access to the underlying CdcAcmClass
    pub fn class_mut(&mut self) -> &mut CdcAcmClass<'a, D> {
        &mut self.class
    }

    /// Send COBS-encoded message over USB CDC (in 64-byte chunks)
    async fn send_cobs_message(&mut self, data: &[u8]) -> Result<(), CommunicationError> {
        for chunk in data.chunks(USB_PACKET_SIZE) {
            self.class
                .write_packet(chunk)
                .await
                .map_err(|_| CommunicationError::SendFailed)?;
        }
        Ok(())
    }

    /// Read COBS-encoded message from USB CDC
    ///
    /// Reads until COBS sentinel (0x00) is received.
    async fn read_cobs_message(
        &mut self,
    ) -> Result<HeaplessVec<u8, MAX_MSG_SIZE>, CommunicationError> {
        let mut rx_buf = HeaplessVec::<u8, MAX_MSG_SIZE>::new();
        let mut packet_buf = [0u8; USB_PACKET_SIZE];

        loop {
            match self.class.read_packet(&mut packet_buf).await {
                Ok(n) if n > 0 => {
                    for i in 0..n {
                        let _ = rx_buf.push(packet_buf[i]);
                        if packet_buf[i] == 0x00 {
                            return Ok(rx_buf);
                        }
                        if rx_buf.is_full() {
                            return Err(CommunicationError::MessageTooLarge);
                        }
                    }
                }
                Ok(_) => {
                    // Zero bytes read, connection might be lost
                    if rx_buf.is_empty() {
                        return Err(CommunicationError::Disconnected);
                    }
                }
                Err(_) => {
                    return Err(CommunicationError::ReceiveFailed);
                }
            }
        }
    }
}

impl<'a, D: embassy_usb::driver::Driver<'a>> CommunicationPort for UsbCdcAdapter<'a, D> {
    async fn wait_connection(&mut self) {
        self.class.wait_connection().await;
    }

    fn is_connected(&self) -> bool {
        self.class.dtr()
    }

    async fn send_response(&mut self, response: &DbResponse) -> Result<(), CommunicationError> {
        let encoded = postcard::to_vec_cobs::<_, MAX_MSG_SIZE>(response)
            .map_err(|_| CommunicationError::InvalidFormat)?;

        self.send_cobs_message(&encoded).await
    }

    async fn receive_command(&mut self) -> Result<Option<DbCommand>, CommunicationError> {
        let mut rx_buf = self.read_cobs_message().await?;

        if rx_buf.is_empty() {
            return Ok(None);
        }

        let cmd = postcard::from_bytes_cobs::<DbCommand>(&mut rx_buf)
            .map_err(|_| CommunicationError::InvalidFormat)?;

        Ok(Some(cmd))
    }
}
