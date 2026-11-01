use crate::{Status, metadata::MetadataValue};
use bytes::{Buf, BufMut, BytesMut};
#[cfg(feature = "deflate")]
use flate2::read::ZlibEncoder;
#[cfg(feature = "gzip")]
use flate2::read::{GzEncoder, MultiGzDecoder};
use std::{fmt, sync::Arc};
#[cfg(feature = "zstd")]
use zstd::stream::read::{Decoder, Encoder};

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";
pub(crate) const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

/// Why a received compression header was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressionErrorReason {
    /// The encoding is unknown or is not enabled for this call.
    Unsupported,
}

/// A compression-header refusal, available through [`Status`]'s error source.
///
/// The codec alone constructs this error after applying the call's enabled
/// encodings. Consumers can read the wire bytes without parsing status text.
#[derive(Clone, Debug)]
pub struct CompressionError {
    received: http::HeaderValue,
    reason: CompressionErrorReason,
}

impl CompressionError {
    /// The first received `grpc-encoding` header value, as wire bytes.
    pub fn received(&self) -> &[u8] {
        self.received.as_bytes()
    }

    /// The codec's reason for refusing the header.
    pub fn reason(&self) -> CompressionErrorReason {
        self.reason
    }
}

impl fmt::Display for CompressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Compression rejected")
    }
}

impl std::error::Error for CompressionError {}

/// Struct used to configure which encodings are enabled on a server or channel.
///
/// Represents an ordered list of compression encodings that are enabled.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnabledCompressionEncodings {
    inner: [Option<CompressionEncoding>; 3],
}

impl EnabledCompressionEncodings {
    /// Enable a [`CompressionEncoding`].
    ///
    /// Adds the new encoding to the end of the encoding list.
    pub fn enable(&mut self, encoding: CompressionEncoding) {
        for e in self.inner.iter_mut() {
            match e {
                Some(e) if *e == encoding => return,
                None => {
                    *e = Some(encoding);
                    return;
                }
                _ => continue,
            }
        }
    }

    /// Remove the last [`CompressionEncoding`].
    pub fn pop(&mut self) -> Option<CompressionEncoding> {
        self.inner
            .iter_mut()
            .rev()
            .find(|entry| entry.is_some())?
            .take()
    }

    pub(crate) fn into_accept_encoding_header_value(self) -> Option<http::HeaderValue> {
        let mut value = BytesMut::new();
        for encoding in self.inner.into_iter().flatten() {
            value.put_slice(encoding.as_str().as_bytes());
            value.put_u8(b',');
        }

        if value.is_empty() {
            return None;
        }

        value.put_slice(b"identity");
        Some(http::HeaderValue::from_maybe_shared(value).unwrap())
    }

    /// Check if a [`CompressionEncoding`] is enabled.
    pub fn is_enabled(&self, encoding: CompressionEncoding) -> bool {
        self.inner.contains(&Some(encoding))
    }

    /// Check if any [`CompressionEncoding`]s are enabled.
    pub fn is_empty(&self) -> bool {
        self.inner.iter().all(|e| e.is_none())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompressionSettings {
    pub(crate) encoding: CompressionEncoding,
    /// buffer_growth_interval controls memory growth for internal buffers to balance resizing cost against memory waste.
    /// The default buffer growth interval is 8 kilobytes.
    pub(crate) buffer_growth_interval: usize,
}

/// The compression encodings Tonic supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompressionEncoding {
    #[allow(missing_docs)]
    #[cfg(feature = "gzip")]
    Gzip,
    #[allow(missing_docs)]
    #[cfg(feature = "deflate")]
    Deflate,
    #[allow(missing_docs)]
    #[cfg(feature = "zstd")]
    Zstd,
}

impl CompressionEncoding {
    pub(crate) const ENCODINGS: &'static [CompressionEncoding] = &[
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip,
        #[cfg(feature = "deflate")]
        CompressionEncoding::Deflate,
        #[cfg(feature = "zstd")]
        CompressionEncoding::Zstd,
    ];

    /// Based on the `grpc-accept-encoding` header, pick an encoding to use.
    pub(crate) fn from_accept_encoding_header(
        map: &http::HeaderMap,
        enabled_encodings: EnabledCompressionEncodings,
    ) -> Option<Self> {
        if enabled_encodings.is_empty() {
            return None;
        }

        let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        split_by_comma(header_value_str).find_map(|value| match value {
            #[cfg(feature = "gzip")]
            "gzip" => Some(CompressionEncoding::Gzip),
            #[cfg(feature = "deflate")]
            "deflate" => Some(CompressionEncoding::Deflate),
            #[cfg(feature = "zstd")]
            "zstd" => Some(CompressionEncoding::Zstd),
            _ => None,
        })
    }

    /// Get the value of `grpc-encoding` header. Returns an error if the encoding isn't supported.
    pub(crate) fn from_encoding_header(
        map: &http::HeaderMap,
        enabled_encodings: EnabledCompressionEncodings,
    ) -> Result<Option<Self>, Status> {
        let Some(header_value) = map.get(ENCODING_HEADER) else {
            return Ok(None);
        };

        match header_value.as_bytes() {
            #[cfg(feature = "gzip")]
            b"gzip" if enabled_encodings.is_enabled(CompressionEncoding::Gzip) => {
                Ok(Some(CompressionEncoding::Gzip))
            }
            #[cfg(feature = "deflate")]
            b"deflate" if enabled_encodings.is_enabled(CompressionEncoding::Deflate) => {
                Ok(Some(CompressionEncoding::Deflate))
            }
            #[cfg(feature = "zstd")]
            b"zstd" if enabled_encodings.is_enabled(CompressionEncoding::Zstd) => {
                Ok(Some(CompressionEncoding::Zstd))
            }
            b"identity" => Ok(None),
            _ => {
                let error = CompressionError {
                    received: header_value.clone(),
                    reason: CompressionErrorReason::Unsupported,
                };
                let message = match std::str::from_utf8(error.received()) {
                    Ok(value) => {
                        format!("Content is compressed with `{value}` which isn't supported")
                    }
                    Err(_) => error.to_string(),
                };
                let mut status = Status::unimplemented(message);
                status.set_source(Arc::new(error));

                let header_value = enabled_encodings
                    .into_accept_encoding_header_value()
                    .map(MetadataValue::unchecked_from_header_value)
                    .unwrap_or_else(|| MetadataValue::from_static("identity"));
                status
                    .metadata_mut()
                    .insert(ACCEPT_ENCODING_HEADER, header_value);

                Err(status)
            }
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip => "gzip",
            #[cfg(feature = "deflate")]
            CompressionEncoding::Deflate => "deflate",
            #[cfg(feature = "zstd")]
            CompressionEncoding::Zstd => "zstd",
        }
    }

    #[cfg(any(feature = "gzip", feature = "deflate", feature = "zstd"))]
    pub(crate) fn into_header_value(self) -> http::HeaderValue {
        http::HeaderValue::from_static(self.as_str())
    }
}

impl fmt::Display for CompressionEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn split_by_comma(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(|s| s.trim())
}

/// Compress `len` bytes from `decompressed_buf` into `out_buf`.
/// buffer_size_increment is a hint to control the growth of out_buf versus the cost of resizing it.
#[allow(unused_variables, unreachable_code)]
pub(crate) fn compress(
    settings: CompressionSettings,
    decompressed_buf: &mut BytesMut,
    out_buf: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error> {
    let buffer_growth_interval = settings.buffer_growth_interval;
    let capacity = ((len / buffer_growth_interval) + 1) * buffer_growth_interval;
    out_buf.reserve(capacity);

    #[cfg(any(feature = "gzip", feature = "deflate", feature = "zstd"))]
    let mut out_writer = out_buf.writer();

    match settings.encoding {
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip => {
            let mut gzip_encoder = GzEncoder::new(
                &decompressed_buf[0..len],
                // FIXME: support customizing the compression level
                flate2::Compression::new(6),
            );
            std::io::copy(&mut gzip_encoder, &mut out_writer)?;
        }
        #[cfg(feature = "deflate")]
        CompressionEncoding::Deflate => {
            let mut deflate_encoder = ZlibEncoder::new(
                &decompressed_buf[0..len],
                // FIXME: support customizing the compression level
                flate2::Compression::new(6),
            );
            std::io::copy(&mut deflate_encoder, &mut out_writer)?;
        }
        #[cfg(feature = "zstd")]
        CompressionEncoding::Zstd => {
            let mut zstd_encoder = Encoder::new(
                &decompressed_buf[0..len],
                // FIXME: support customizing the compression level
                zstd::DEFAULT_COMPRESSION_LEVEL,
            )?;
            std::io::copy(&mut zstd_encoder, &mut out_writer)?;
        }
    }

    decompressed_buf.advance(len);

    Ok(())
}

/// Decompress `len` bytes from `compressed_buf` into `out_buf`.
///
/// The `len` bytes must be complete coded data, used exactly: gzip members or
/// zstd frames in sequence, or one zlib stream for deflate. Bytes after the
/// end, a stream cut short and an empty message are errors, never a message
/// made of what was decoded before them.
#[allow(unused_variables, unreachable_code)]
pub(crate) fn decompress(
    settings: CompressionSettings,
    compressed_buf: &mut BytesMut,
    mut out_buf: bytes::buf::Limit<&mut BytesMut>,
    len: usize,
) -> Result<(), std::io::Error> {
    let buffer_growth_interval = settings.buffer_growth_interval;
    let estimate_decompressed_len = len * 2;
    let capacity = std::cmp::min(
        bytes::buf::Limit::limit(&out_buf),
        ((estimate_decompressed_len / buffer_growth_interval) + 1) * buffer_growth_interval,
    );

    out_buf.get_mut().reserve(capacity);

    #[cfg(any(feature = "gzip", feature = "deflate", feature = "zstd"))]
    let mut out_writer = out_buf.writer();

    match settings.encoding {
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip => {
            // RFC 1952: a gzip file is a series of members. The multi-member
            // reader reads each to its trailer and refuses what is not one.
            let mut gzip_decoder = MultiGzDecoder::new(&compressed_buf[0..len]);
            std::io::copy(&mut gzip_decoder, &mut out_writer)?;
        }
        #[cfg(feature = "deflate")]
        CompressionEncoding::Deflate => {
            inflate_exactly(&compressed_buf[0..len], &mut out_writer)?;
        }
        #[cfg(feature = "zstd")]
        CompressionEncoding::Zstd => {
            let mut zstd_decoder = Decoder::new(&compressed_buf[0..len])?;
            std::io::copy(&mut zstd_decoder, &mut out_writer)?;
        }
    }

    compressed_buf.advance(len);

    Ok(())
}

/// One zlib stream, which must reach its end, Adler-32 checked, at the end of
/// `input`, as gRPC core's `ZlibBody` requires. flate2's read decoder ends
/// at the end of its input whether or not the stream did. Each step flushes
/// nothing: miniz_oxide's `Finish` expects the whole output in one call.
#[cfg(feature = "deflate")]
fn inflate_exactly(input: &[u8], out_writer: &mut impl std::io::Write) -> std::io::Result<()> {
    use flate2::{Decompress, FlushDecompress, Status};
    use std::io::{Error, ErrorKind};
    let mut inflater = Decompress::new(true);
    let mut chunk = vec![0; 32 * 1024];
    loop {
        let (read, written) = (inflater.total_in(), inflater.total_out());
        let used = usize::try_from(read).expect("input length fits usize");
        let status = inflater
            .decompress(&input[used..], &mut chunk, FlushDecompress::None)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let produced =
            usize::try_from(inflater.total_out() - written).expect("produced bytes fit the chunk");
        out_writer.write_all(&chunk[..produced])?;
        match status {
            Status::StreamEnd => break,
            _ if inflater.total_in() == read && produced == 0 => {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "deflate stream ends before its end",
                ));
            }
            _ => {}
        }
    }
    if inflater.total_in() != input.len() as u64 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "data follows the end of the deflate stream",
        ));
    }
    Ok(())
}

/// Controls compression behavior for individual messages within a stream.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SingleMessageCompressionOverride {
    /// Inherit whatever compression is already configured. If the stream is compressed this
    /// message will also be configured.
    ///
    /// This is the default.
    #[default]
    Inherit,
    /// Don't compress this message, even if compression is enabled on the stream.
    Disable,
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "gzip", feature = "deflate", feature = "zstd"))]
    use http::HeaderValue;

    use super::*;

    #[test]
    fn refused_encoding_retains_typed_bytes_and_reason() {
        use std::error::Error as _;

        for value in [b"unknown".as_slice(), b"gzip", b"\xffzip"] {
            let mut headers = http::HeaderMap::new();
            headers.insert(
                ENCODING_HEADER,
                http::HeaderValue::from_bytes(value).unwrap(),
            );
            let status = CompressionEncoding::from_encoding_header(
                &headers,
                EnabledCompressionEncodings::default(),
            )
            .unwrap_err();
            assert_eq!(status.code(), crate::Code::Unimplemented);
            let expected = match std::str::from_utf8(value) {
                Ok(value) => format!("Content is compressed with `{value}` which isn't supported"),
                Err(_) => "Compression rejected".to_owned(),
            };
            assert_eq!(status.message(), expected);
            let error = status
                .source()
                .unwrap()
                .downcast_ref::<CompressionError>()
                .unwrap();
            assert_eq!(error.received(), value);
            assert_eq!(error.reason(), CompressionErrorReason::Unsupported);
            assert_eq!(
                status.metadata().get(ACCEPT_ENCODING_HEADER).unwrap(),
                "identity"
            );
        }

        let mut headers = http::HeaderMap::new();
        headers.insert(ENCODING_HEADER, http::HeaderValue::from_static("identity"));
        assert_eq!(
            CompressionEncoding::from_encoding_header(
                &headers,
                EnabledCompressionEncodings::default(),
            )
            .unwrap(),
            None
        );
        #[cfg(feature = "gzip")]
        {
            headers.insert(ENCODING_HEADER, http::HeaderValue::from_static("gzip"));
            let mut enabled = EnabledCompressionEncodings::default();
            enabled.enable(CompressionEncoding::Gzip);
            assert_eq!(
                CompressionEncoding::from_encoding_header(&headers, enabled).unwrap(),
                Some(CompressionEncoding::Gzip)
            );
        }
    }

    /// A message decodes only as complete coded data its bytes hold
    /// exactly (Boundless #280).
    #[cfg(all(feature = "gzip", feature = "deflate", feature = "zstd"))]
    #[test]
    fn a_message_decodes_only_as_complete_coded_data() {
        use std::io::Write as _;

        fn gzip(data: &[u8]) -> Vec<u8> {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        fn zlib(data: &[u8]) -> Vec<u8> {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        fn zstd(data: &[u8]) -> Vec<u8> {
            zstd::encode_all(data, 3).unwrap()
        }
        fn decode(encoding: CompressionEncoding, input: &[u8]) -> Result<Vec<u8>, std::io::Error> {
            let mut compressed = BytesMut::from(input);
            let mut out = BytesMut::new();
            decompress(
                CompressionSettings {
                    encoding,
                    buffer_growth_interval: 8 * 1024,
                },
                &mut compressed,
                (&mut out).limit(1024 * 1024),
                input.len(),
            )?;
            assert!(compressed.is_empty(), "the message's bytes are used");
            Ok(out.to_vec())
        }

        // A message larger than one inflate chunk decodes whole.
        let large: Vec<u8> = (0..200_000u32).flat_map(|n| n.to_le_bytes()).collect();
        type Encode = fn(&[u8]) -> Vec<u8>;
        let encoders: [(CompressionEncoding, Encode, bool); 3] = [
            (CompressionEncoding::Gzip, gzip, true),
            (CompressionEncoding::Deflate, zlib, false),
            (CompressionEncoding::Zstd, zstd, true),
        ];
        for (encoding, encode, continues) in encoders {
            let first = encode(b"first");
            let second = encode(b"second");
            assert_eq!(decode(encoding, &first).unwrap(), b"first", "{encoding:?}");
            assert_eq!(
                decode(encoding, &encode(&large)).unwrap(),
                large,
                "{encoding:?}"
            );
            let two = decode(encoding, &[first.clone(), second].concat());
            if continues {
                assert_eq!(two.unwrap(), b"firstsecond", "{encoding:?}");
            } else {
                assert!(two.is_err(), "{encoding:?} two streams: {two:?}");
            }
            for (case, input) in [
                (
                    "trailing garbage",
                    [first.clone(), b"garbage".to_vec()].concat(),
                ),
                ("trailing zeros", [first.clone(), vec![0; 8]].concat()),
                ("checksum cut", first[..first.len() - 4].to_vec()),
                ("cut to half", first[..first.len() / 2].to_vec()),
                ("empty", Vec::new()),
            ] {
                let error = decode(encoding, &input).unwrap_err();
                assert_ne!(
                    error.kind(),
                    std::io::ErrorKind::WriteZero,
                    "{encoding:?} {case}"
                );
            }
        }
        // The decompressed-size limit still refuses as the limit.
        let mut compressed = BytesMut::from(&zlib(&large)[..]);
        let len = compressed.len();
        let mut out = BytesMut::new();
        let error = decompress(
            CompressionSettings {
                encoding: CompressionEncoding::Deflate,
                buffer_growth_interval: 8 * 1024,
            },
            &mut compressed,
            (&mut out).limit(1024),
            len,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WriteZero);
    }

    #[test]
    fn convert_none_into_header_value() {
        let encodings = EnabledCompressionEncodings::default();

        assert!(encodings.into_accept_encoding_header_value().is_none());
    }

    #[test]
    #[cfg(feature = "gzip")]
    fn convert_gzip_into_header_value() {
        const GZIP: HeaderValue = HeaderValue::from_static("gzip,identity");

        let encodings = EnabledCompressionEncodings {
            inner: [Some(CompressionEncoding::Gzip), None, None],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), GZIP);

        let encodings = EnabledCompressionEncodings {
            inner: [None, None, Some(CompressionEncoding::Gzip)],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), GZIP);
    }

    #[test]
    #[cfg(feature = "zstd")]
    fn convert_zstd_into_header_value() {
        const ZSTD: HeaderValue = HeaderValue::from_static("zstd,identity");

        let encodings = EnabledCompressionEncodings {
            inner: [Some(CompressionEncoding::Zstd), None, None],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), ZSTD);

        let encodings = EnabledCompressionEncodings {
            inner: [None, None, Some(CompressionEncoding::Zstd)],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), ZSTD);
    }

    #[test]
    #[cfg(all(feature = "gzip", feature = "deflate", feature = "zstd"))]
    fn convert_compression_encodings_into_header_value() {
        let encodings = EnabledCompressionEncodings {
            inner: [
                Some(CompressionEncoding::Gzip),
                Some(CompressionEncoding::Deflate),
                Some(CompressionEncoding::Zstd),
            ],
        };

        assert_eq!(
            encodings.into_accept_encoding_header_value().unwrap(),
            HeaderValue::from_static("gzip,deflate,zstd,identity"),
        );

        let encodings = EnabledCompressionEncodings {
            inner: [
                Some(CompressionEncoding::Zstd),
                Some(CompressionEncoding::Deflate),
                Some(CompressionEncoding::Gzip),
            ],
        };

        assert_eq!(
            encodings.into_accept_encoding_header_value().unwrap(),
            HeaderValue::from_static("zstd,deflate,gzip,identity"),
        );
    }
}
