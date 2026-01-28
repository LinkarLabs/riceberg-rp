//! Communication port - abstraction for host communication
//!
//! This trait allows the application to communicate with hosts without
//! knowing the specific transport (USB CDC, UART, WiFi, etc.)

use crate::db_protocol::{DbCommand, DbResponse};

/// Error type for communication operations
#[derive(Clone, Copy, Debug, defmt::Format)]
pub enum CommunicationError {
    /// Not connected
    NotConnected,
    /// Connection lost
    Disconnected,
    /// Failed to send response
    SendFailed,
    /// Failed to receive command
    ReceiveFailed,
    /// Message too large
    MessageTooLarge,
    /// Invalid message format
    InvalidFormat,
    /// Timeout
    Timeout,
}

/// Port for communication with host systems
///
/// This trait abstracts the communication channel (USB CDC, UART, WiFi, etc.)
///
/// # Example Implementation
///
/// ```ignore
/// struct UsbCdcAdapter<D: Driver> {
///     class: CdcAcmClass<'static, D>,
/// }
///
/// impl<D: Driver> CommunicationPort for UsbCdcAdapter<D> {
///     async fn wait_connection(&mut self) {
///         self.class.wait_connection().await;
///     }
///
///     fn is_connected(&self) -> bool {
///         self.class.dtr()
///     }
///
///     async fn send_response(&mut self, response: &DbResponse) -> Result<(), CommunicationError> {
///         let encoded = postcard::to_vec_cobs::<_, 8192>(response)?;
///         self.class.write_packet(&encoded).await?;
///         Ok(())
///     }
///
///     async fn receive_command(&mut self) -> Result<Option<DbCommand>, CommunicationError> {
///         // ... COBS decode ...
///     }
/// }
/// ```
pub trait CommunicationPort {
    /// Wait for connection from host
    ///
    /// This blocks until a host connects to the device.
    fn wait_connection(&mut self) -> impl core::future::Future<Output = ()>;

    /// Check if connected
    fn is_connected(&self) -> bool;

    /// Send a response to the host
    fn send_response(
        &mut self,
        response: &DbResponse,
    ) -> impl core::future::Future<Output = Result<(), CommunicationError>>;

    /// Receive a command from the host
    ///
    /// Returns `None` if no command is available (non-blocking) or
    /// if the connection is lost.
    fn receive_command(
        &mut self,
    ) -> impl core::future::Future<Output = Result<Option<DbCommand>, CommunicationError>>;

    /// Send a ready signal to the host
    fn send_ready(&mut self) -> impl core::future::Future<Output = Result<(), CommunicationError>> {
        async { self.send_response(&DbResponse::Ok).await }
    }
}
