use super::budget::Buffer;
use crate::Status;
use bytes::buf::UninitSlice;
use bytes::{Buf, BufMut, Bytes};

/// A specialized buffer to decode gRPC messages from.
#[derive(Debug)]
pub struct DecodeBuf<'a> {
    buf: &'a mut Buffer,
    len: usize,
}

/// A specialized buffer to encode gRPC messages into.
#[derive(Debug)]
pub struct EncodeBuf<'a> {
    buf: &'a mut Buffer,
}

impl<'a> DecodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut Buffer, len: usize) -> Self {
        DecodeBuf { buf, len }
    }
}

impl Buf for DecodeBuf<'_> {
    #[inline]
    fn remaining(&self) -> usize {
        self.len
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        let ret = self.buf.chunk();

        if ret.len() > self.len {
            &ret[..self.len]
        } else {
            ret
        }
    }

    #[inline]
    fn advance(&mut self, cnt: usize) {
        assert!(cnt <= self.len);
        self.buf.bytes.advance(cnt);
        self.len -= cnt;
    }

    #[inline]
    fn copy_to_bytes(&mut self, len: usize) -> Bytes {
        assert!(len <= self.len);
        self.len -= len;
        assert!(!self.buf.guarded(), "guarded decoder owns message copying");
        self.buf.bytes.copy_to_bytes(len)
    }
}

impl<'a> EncodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut Buffer) -> Self {
        EncodeBuf { buf }
    }
}

impl EncodeBuf<'_> {
    /// Reserves capacity for at least `additional` more bytes to be inserted
    /// into the buffer.
    ///
    /// More than `additional` bytes may be reserved in order to avoid frequent
    /// reallocations. A call to `reserve` may result in an allocation.
    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.require_room(additional);
        self.buf.bytes.reserve(additional);
    }

    /// Prepare space before writing in a codec that opted into buffer admission.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), Status> {
        self.buf.prepare(additional)
    }

    fn require_room(&self, additional: usize) {
        assert!(
            !self.buf.guarded() || additional <= self.buf.capacity() - self.buf.len(),
            "guarded encoder prepares its writes"
        );
    }
}

unsafe impl BufMut for EncodeBuf<'_> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        if self.buf.guarded() {
            self.buf.capacity() - self.buf.len()
        } else {
            self.buf.bytes.remaining_mut()
        }
    }

    #[inline]
    unsafe fn advance_mut(&mut self, cnt: usize) {
        unsafe { self.buf.bytes.advance_mut(cnt) }
    }

    #[inline]
    fn chunk_mut(&mut self) -> &mut UninitSlice {
        if self.buf.guarded() && self.remaining_mut() == 0 {
            return UninitSlice::new(&mut []);
        }
        self.buf.bytes.chunk_mut()
    }

    #[inline]
    fn put<T: Buf>(&mut self, src: T)
    where
        Self: Sized,
    {
        self.require_room(src.remaining());
        self.buf.bytes.put(src)
    }

    #[inline]
    fn put_slice(&mut self, src: &[u8]) {
        self.require_room(src.len());
        self.buf.bytes.put_slice(src)
    }

    #[inline]
    fn put_bytes(&mut self, val: u8, cnt: usize) {
        self.require_room(cnt);
        self.buf.bytes.put_bytes(val, cnt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::BufferKind;

    #[test]
    fn decode_buf() {
        let mut payload = Buffer::new(100, 100, BufferKind::Receive, None);
        payload.bytes.put(&vec![0u8; 50][..]);
        let mut buf = DecodeBuf::new(&mut payload, 20);

        assert_eq!(buf.len, 20);
        assert_eq!(buf.remaining(), 20);
        assert_eq!(buf.chunk().len(), 20);

        buf.advance(10);
        assert_eq!(buf.remaining(), 10);

        let mut out = [0; 5];
        buf.copy_to_slice(&mut out);
        assert_eq!(buf.remaining(), 5);
        assert_eq!(buf.chunk().len(), 5);

        assert_eq!(buf.copy_to_bytes(5).len(), 5);
        assert!(!buf.has_remaining());
    }

    #[test]
    fn encode_buf() {
        let mut bytes = Buffer::new(100, 100, BufferKind::Send, None);
        let mut buf = EncodeBuf::new(&mut bytes);

        let initial = buf.remaining_mut();
        unsafe { buf.advance_mut(20) };
        assert_eq!(buf.remaining_mut(), initial - 20);

        buf.put_u8(b'a');
        assert_eq!(buf.remaining_mut(), initial - 20 - 1);
    }
}
