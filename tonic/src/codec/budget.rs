//! Capacity ownership for codecs that opt into staged buffer admission.
use crate::Status;
use bytes::{Buf, Bytes, BytesMut};
use std::{fmt, io, ops::Deref, sync::Arc};

/// A reusable codec buffer.
#[derive(Clone, Copy, Debug)]
pub enum BufferKind {
    /// Coded receive data.
    Receive,
    /// Decompressed receive data.
    Decompressed,
    /// Encoded batch.
    Send,
    /// Uncompressed input to the encoder.
    Uncompressed,
}

/// Optional capacity admission at the native codec's allocation sites.
pub trait BufferBudget: fmt::Debug + Send + Sync {
    /// Current prepaid capacity ceiling for a send buffer.
    fn send_limit(&self, kind: BufferKind) -> usize;

    /// Admit old and new storage, run the move once, then settle to new storage.
    fn move_buffer(
        &self,
        kind: BufferKind,
        old: usize,
        new: usize,
        allocate: &mut dyn FnMut(),
    ) -> Result<(), Status>;

    /// Hold decoder state funding until the synchronous decoder returns.
    fn decoder_state(&self, encoding: super::CompressionEncoding) -> Result<Box<dyn Send>, Status>;
}

#[derive(Debug)]
pub(super) struct Buffer {
    pub(super) bytes: BytesMut,
    budget: Option<Arc<dyn BufferBudget>>,
    kind: BufferKind,
    capacity: usize,
    floor: usize,
    limit: usize,
}

impl Buffer {
    pub(super) fn new(
        initial: usize,
        floor: usize,
        kind: BufferKind,
        budget: Option<Arc<dyn BufferBudget>>,
    ) -> Self {
        Self {
            bytes: if budget.is_some() {
                BytesMut::new()
            } else {
                BytesMut::with_capacity(initial)
            },
            budget,
            kind,
            capacity: 0,
            floor,
            limit: 0,
        }
    }

    pub(super) fn floor(&self) -> usize {
        self.floor
    }

    pub(super) fn guarded(&self) -> bool {
        self.budget.is_some()
    }

    pub(super) fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    pub(super) fn budget(&self) -> Option<&dyn BufferBudget> {
        self.budget.as_deref()
    }

    pub(super) fn prepare(&mut self, additional: usize) -> Result<(), Status> {
        let Some(budget) = &self.budget else {
            self.bytes.reserve(additional);
            return Ok(());
        };
        if self.bytes.try_reclaim(additional) {
            return Ok(());
        }
        let limit = match self.kind {
            BufferKind::Receive | BufferKind::Decompressed => self.limit,
            BufferKind::Send | BufferKind::Uncompressed => budget.send_limit(self.kind),
        };
        let needed = self
            .len()
            .checked_add(additional)
            .filter(|n| *n <= limit)
            .ok_or_else(|| Status::internal("codec storage exceeds its bound"))?;
        let next = needed
            .max(self.floor)
            .max(self.capacity.saturating_mul(2))
            .min(limit);
        budget.move_buffer(self.kind, self.capacity, next, &mut || {
            let mut fresh = BytesMut::with_capacity(next);
            fresh.extend_from_slice(&self.bytes);
            self.bytes = fresh;
            self.capacity = next;
        })
    }

    pub(super) fn take_batch(&mut self) -> Bytes {
        if self.guarded() {
            let exact = Bytes::copy_from_slice(&self.bytes);
            self.bytes.clear();
            exact
        } else {
            self.bytes.split_to(self.bytes.len()).freeze()
        }
    }
}

impl Deref for Buffer {
    type Target = BytesMut;

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

pub(super) struct Output<'a>(pub(super) &'a mut Buffer, pub(super) usize);
impl io::Write for Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let len = self.1.min(bytes.len());
        self.0.prepare(len).map_err(io::Error::other)?;
        self.0.bytes.extend_from_slice(&bytes[..len]);
        self.1 -= len;
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn append(buffer: &mut Buffer, mut data: Bytes) -> Result<(), Status> {
    use bytes::BufMut;
    buffer.prepare(data.remaining())?;
    buffer.bytes.put(&mut data);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Debug, Default)]
    struct Admission {
        refuse: AtomicBool,
    }

    impl BufferBudget for Admission {
        fn send_limit(&self, _: BufferKind) -> usize {
            32
        }

        fn move_buffer(
            &self,
            _: BufferKind,
            old: usize,
            new: usize,
            allocate: &mut dyn FnMut(),
        ) -> Result<(), Status> {
            if self.refuse.load(Ordering::Relaxed) {
                assert_eq!((old, new), (8, 16));
                return Err(Status::resource_exhausted("fixture budget"));
            }
            allocate();
            Ok(())
        }

        fn decoder_state(
            &self,
            _: super::super::CompressionEncoding,
        ) -> Result<Box<dyn Send>, Status> {
            unreachable!()
        }
    }

    #[test]
    fn a_refused_move_preserves_capacity_and_unread_bytes() {
        let admission = Arc::new(Admission::default());
        let mut buffer = Buffer::new(8, 8, BufferKind::Send, Some(admission.clone()));
        append(&mut buffer, Bytes::from_static(b"olddata")).unwrap();
        buffer.bytes.advance(3);
        admission.refuse.store(true, Ordering::Relaxed);
        assert_eq!(
            append(&mut buffer, Bytes::from_static(b"newer"))
                .unwrap_err()
                .code(),
            crate::Code::ResourceExhausted
        );
        assert_eq!(buffer.capacity, 8);
        assert_eq!(&buffer[..], b"data");
        admission.refuse.store(false, Ordering::Relaxed);
        append(&mut buffer, Bytes::from_static(b"newer")).unwrap();
        assert_eq!(buffer.capacity, 16);
        let first = buffer.take_batch();
        append(&mut buffer, Bytes::from_static(b"second")).unwrap();
        assert_eq!(&first[..], b"datanewer");
        assert_eq!(&buffer[..], b"second");
    }
}
