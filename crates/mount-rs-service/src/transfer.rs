//! Server-wide transfer admission. Charges cover wire buffers, decoded JSON,
//! transient request copies and queued response bytes. Provider caches and
//! provider-returned metadata are separate from this bound, as is QUIC's
//! explicitly configured receive window.
use bytes::Bytes;
use mount_rs_remote_protocol::{
    FrameError, Message, OperationName,
    binary::{self, Header, Kind},
};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::AsyncWrite,
    sync::{OwnedSemaphorePermit, Semaphore},
};

const SMALL_CONTROL_BYTES: usize = 64 * 1024;
const JSON_EXPANSION: usize = 64;
const IO_METADATA_OVERHEAD: usize = 256;

/// Independent ingress and egress capacities, shared across all connections.
/// Small control envelopes have reserved capacity so renewal can progress
/// while raw I/O or large generic control transfers saturate data admission.
#[derive(Clone, Copy, Debug)]
pub struct RemoteTransferLimits {
    pub data_bytes: usize,
    pub control_bytes: usize,
    pub active_data_operations: usize,
    pub active_control_operations: usize,
}
impl Default for RemoteTransferLimits {
    fn default() -> Self {
        Self {
            data_bytes: 1024 * 1024 * 1024,
            control_bytes: 64 * 1024 * 1024,
            active_data_operations: 64,
            active_control_operations: 32,
        }
    }
}
impl RemoteTransferLimits {
    pub fn validate(self) -> Result<(), &'static str> {
        if self.data_bytes < json_charge(binary::MAX_CONTROL_BYTES)
            || self.data_bytes > u32::MAX as usize
        {
            return Err("data_bytes must admit the maximum generic control charge and fit u32");
        }
        if self.control_bytes < json_charge(SMALL_CONTROL_BYTES)
            || self.control_bytes > u32::MAX as usize
        {
            return Err("control_bytes must admit a 64KiB control charge and fit u32");
        }
        if !(1..=16_384).contains(&self.active_data_operations)
            || !(1..=16_384).contains(&self.active_control_operations)
        {
            return Err("active transfer operation limits must be in 1..=16384");
        }
        Ok(())
    }
}

// Value is 32 bytes on supported 64-bit targets. The factor also covers
// Vec growth, object nodes, a decoded request clone, serialized Vec growth,
// and the wire body. Numeric arrays cost much more than raw byte payloads.
// The additional raw allowance covers the generic JSON operation
// dispatch's transient decoded byte vector (generic controls remain supported).
fn json_charge(length: usize) -> usize {
    length
        .saturating_mul(JSON_EXPANSION)
        .saturating_add(binary::MAX_IO_BYTES + binary::HEADER_BYTES)
}
fn saturated() -> FrameError {
    FrameError::Io(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "transfer admission saturated",
    ))
}
fn take(pool: &Arc<Semaphore>, bytes: usize) -> Result<OwnedSemaphorePermit, FrameError> {
    let count = u32::try_from(bytes).map_err(|_| FrameError::InvalidLength)?;
    pool.clone()
        .try_acquire_many_owned(count)
        .map_err(|_| saturated())
}

pub(crate) struct Budgets {
    ingress_data: Arc<Semaphore>,
    ingress_control: Arc<Semaphore>,
    egress_data: Arc<Semaphore>,
    egress_control: Arc<Semaphore>,
    data_operations: Arc<Semaphore>,
    control_operations: Arc<Semaphore>,
}
pub(crate) struct Admission {
    _bytes: OwnedSemaphorePermit,
    _operation: OwnedSemaphorePermit,
    _connection_operation: Option<OwnedSemaphorePermit>,
    pub(crate) control: bool,
}
impl Budgets {
    pub(crate) fn new(limits: RemoteTransferLimits) -> Self {
        Self {
            ingress_data: Arc::new(Semaphore::new(limits.data_bytes)),
            ingress_control: Arc::new(Semaphore::new(limits.control_bytes)),
            egress_data: Arc::new(Semaphore::new(limits.data_bytes)),
            egress_control: Arc::new(Semaphore::new(limits.control_bytes)),
            data_operations: Arc::new(Semaphore::new(limits.active_data_operations)),
            control_operations: Arc::new(Semaphore::new(limits.active_control_operations)),
        }
    }
    pub(crate) fn ingress(&self, header: Header) -> Result<Admission, FrameError> {
        Header::new(
            header.kind,
            header.request_id,
            header.control_len,
            header.payload_len,
            header.count,
        )?;
        if !matches!(header.kind, Kind::Control | Kind::Read | Kind::Write) {
            return Err(FrameError::InvalidLength);
        }
        let control = header.kind == Kind::Control && header.control_len <= SMALL_CONTROL_BYTES;
        let operation = take(
            if control {
                &self.control_operations
            } else {
                &self.data_operations
            },
            1,
        )?;
        let bytes = if header.kind == Kind::Control {
            json_charge(header.control_len)
        } else {
            // IoRequest contains fixed scalar fields and a drive-id string;
            // retain metadata wire plus decoded strings and the raw body.
            header
                .control_len
                .saturating_mul(2)
                .saturating_add(header.payload_len)
                .saturating_add(IO_METADATA_OVERHEAD)
        };
        let permit = take(
            if control {
                &self.ingress_control
            } else {
                &self.ingress_data
            },
            bytes,
        )?;
        Ok(Admission {
            _bytes: permit,
            _operation: operation,
            _connection_operation: None,
            control,
        })
    }
    // Classify small generic envelopes after parsing, then move their ingress
    // bytes and operation into the data lane before dispatch. Completed data
    // requests therefore never retain renewal's reserved control capacity.
    pub(crate) fn request_operation(&self, admission: &mut Admission) -> Result<(), FrameError> {
        if admission.control {
            let operation = take(&self.data_operations, 1)?;
            let bytes = take(&self.ingress_data, admission._bytes.num_permits())?;
            admission._bytes = bytes;
            admission._operation = operation;
            admission.control = false;
        }
        Ok(())
    }
    // Reserve data work only after its header is validated or a small generic
    // envelope is parsed as a data Request. A rejected extra data stream never
    // queues ahead of renewal; this permit remains owned through dispatch.
    pub(crate) fn connection_data_operation(
        &self,
        admission: &mut Admission,
        operations: &Arc<Semaphore>,
    ) -> Result<(), FrameError> {
        if !admission.control && admission._connection_operation.is_none() {
            admission._connection_operation = Some(take(operations, 1)?);
        }
        Ok(())
    }
    pub(crate) fn egress(
        &self,
        control: bool,
        bytes: usize,
    ) -> Result<OwnedSemaphorePermit, FrameError> {
        take(
            if control {
                &self.egress_control
            } else {
                &self.egress_data
            },
            bytes,
        )
    }
    #[cfg(test)]
    async fn acquire(
        &self,
        control: bool,
        bytes: usize,
    ) -> Result<OwnedSemaphorePermit, FrameError> {
        take(
            if control {
                &self.ingress_control
            } else {
                &self.ingress_data
            },
            bytes,
        )
    }
}

pub(crate) struct ResponseReservation {
    pub(crate) wire_limit: usize,
    pub(crate) charge: usize,
    pub(crate) control: bool,
}
impl ResponseReservation {
    pub(crate) fn io(length: usize) -> Self {
        Self {
            wire_limit: length.saturating_add(128),
            charge: length.saturating_mul(3).saturating_add(256),
            control: false,
        }
    }
    pub(crate) fn message(message: &Message) -> Self {
        let mut wire = SMALL_CONTROL_BYTES;
        let mut charge = json_charge(wire);
        let control = !matches!(message, Message::Request { .. });
        if let Message::Request { operation, .. } = message {
            match operation.name {
                OperationName::ReaddirBounded
                | OperationName::Readlink
                | OperationName::GuardedRead => {
                    wire = binary::MAX_CONTROL_BYTES;
                    charge = json_charge(wire);
                }
                OperationName::HandleRead => {
                    let length = operation
                        .body
                        .get("length")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(0)
                        .min(binary::MAX_IO_BYTES);
                    wire = length.saturating_mul(4).saturating_add(1024);
                    charge = length.saturating_mul(80).saturating_add(json_charge(1024));
                }
                _ => {}
            }
        }
        Self {
            wire_limit: wire.saturating_add(binary::HEADER_BYTES),
            charge,
            control,
        }
    }
}

struct ChargedBytes {
    bytes: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}
impl AsRef<[u8]> for ChargedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
pub(crate) fn charged_bytes(bytes: Vec<u8>, permit: OwnedSemaphorePermit) -> Bytes {
    // Quinn retains owned Bytes chunks for queued/retransmitted data. Every
    // slice/clone shares this owner, releasing the permit only with the last
    // chunk, including cancellation and connection teardown.
    Bytes::from_owner(ChargedBytes {
        bytes,
        _permit: permit,
    })
}

pub(crate) struct ResponseBuffer {
    pub(crate) bytes: Vec<u8>,
    limit: usize,
}
impl ResponseBuffer {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
}
impl AsyncWrite for ResponseBuffer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if data.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Poll::Ready(Err(std::io::Error::other("response reservation limit")));
        }
        self.bytes.extend_from_slice(data);
        Poll::Ready(Ok(data.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn saturation_keeps_control_credit_and_cancellation_returns_credit() {
        let budgets = Budgets::new(RemoteTransferLimits::default());
        let full = budgets
            .ingress_data
            .clone()
            .acquire_many_owned(RemoteTransferLimits::default().data_bytes as u32)
            .await
            .unwrap();
        assert!(budgets.ingress_data.clone().try_acquire_owned().is_err());
        let control = budgets.acquire(true, 1024).await.unwrap();
        drop(control);
        drop(full);
        assert!(budgets.ingress_data.clone().try_acquire_owned().is_ok());
    }
    #[tokio::test]
    async fn queued_bytes_retain_credit_until_last_slice_is_dropped() {
        let budgets = Budgets::new(RemoteTransferLimits::default());
        let before = budgets.egress_control.available_permits();
        let permit = budgets
            .egress_control
            .clone()
            .try_acquire_many_owned(1024)
            .unwrap();
        let bytes = charged_bytes(vec![1; 1024], permit);
        let queued = bytes.slice(512..);
        drop(bytes);
        assert_eq!(budgets.egress_control.available_permits(), before - 1024);
        drop(queued);
        assert_eq!(budgets.egress_control.available_permits(), before);
    }
    #[test]
    fn generic_numeric_array_charge_accounts_decoded_values_and_transients() {
        let length = 1024 * 1024;
        assert!(json_charge(2 * length + 2) >= 32 * length + 2 * length + length);
        assert!(json_charge(usize::MAX) == usize::MAX);
    }
    #[test]
    fn raw_body_and_generic_value_admission_charge_distinct_representations() {
        let budgets = Budgets::new(RemoteTransferLimits::default());
        let raw = Header::new(
            Kind::Write,
            1,
            binary::MAX_IO_CONTROL_BYTES,
            binary::MAX_IO_BYTES,
            0,
        )
        .unwrap();
        let before = budgets.ingress_data.available_permits();
        let permit = budgets.ingress(raw).unwrap();
        assert_eq!(
            before - budgets.ingress_data.available_permits(),
            binary::MAX_IO_BYTES + 2 * binary::MAX_IO_CONTROL_BYTES + IO_METADATA_OVERHEAD
        );
        drop(permit);
        let generic = Header::new(Kind::Control, 0, binary::MAX_CONTROL_BYTES, 0, 0).unwrap();
        let permit = budgets.ingress(generic).unwrap();
        assert_eq!(
            before - budgets.ingress_data.available_permits(),
            json_charge(binary::MAX_CONTROL_BYTES)
        );
        drop(permit);
        assert_eq!(budgets.ingress_data.available_permits(), before);
    }
    #[test]
    fn global_operation_and_egress_saturation_leave_control_lane_available() {
        let budgets = Budgets::new(RemoteTransferLimits {
            active_data_operations: 1,
            ..RemoteTransferLimits::default()
        });
        let data = Header::new(
            Kind::Read,
            1,
            binary::MAX_IO_CONTROL_BYTES,
            0,
            binary::MAX_IO_BYTES,
        )
        .unwrap();
        let held = budgets.ingress(data).unwrap();
        assert!(budgets.ingress(data).is_err());
        let control = Header::new(Kind::Control, 0, 128, 0, 0).unwrap();
        let renewal = budgets.ingress(control).unwrap();
        let egress = budgets
            .egress(false, RemoteTransferLimits::default().data_bytes)
            .unwrap();
        assert!(budgets.egress(false, 1).is_err());
        let hello = budgets.egress(true, 1024).unwrap();
        drop((held, renewal, egress, hello));
        assert!(budgets.ingress(data).is_ok());
    }
    #[test]
    fn forged_oversized_header_never_acquires_a_charge() {
        let budgets = Budgets::new(RemoteTransferLimits::default());
        let before = budgets.ingress_data.available_permits();
        let header = Header {
            kind: Kind::Write,
            request_id: 1,
            control_len: 128,
            payload_len: binary::MAX_IO_BYTES + 1,
            count: 0,
        };
        assert!(matches!(
            budgets.ingress(header),
            Err(FrameError::InvalidLength)
        ));
        assert_eq!(budgets.ingress_data.available_permits(), before);
    }
    #[test]
    fn default_accepts_largest_generic_frame_charge() {
        assert!(
            json_charge(mount_rs_remote_protocol::MAX_FRAME_BYTES)
                <= RemoteTransferLimits::default().data_bytes
        );
        assert!(RemoteTransferLimits::default().validate().is_ok());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;
    #[kani::proof]
    fn generic_control_charge_cannot_wrap_or_exceed_default_admission() {
        let length: usize = kani::any();
        kani::assume(length <= binary::MAX_CONTROL_BYTES);
        let charge = json_charge(length);
        assert!(charge >= length);
        assert!(charge <= 1024 * 1024 * 1024);
        assert_eq!(
            charge,
            length * JSON_EXPANSION + binary::MAX_IO_BYTES + binary::HEADER_BYTES
        );
        kani::cover!(length == binary::MAX_CONTROL_BYTES);
    }
}
