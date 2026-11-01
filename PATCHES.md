# Boundless tonic patches

Based on upstream `tonic-v0.14.6`
(`6cb6056b5a748bc5a29bd48f4602dbc4e552bb7d`). Keep upstream history and licenses.
The default `master` branch mirrors grpc/grpc-rust; Boundless pins patches
from the `boundless` branch after review.

## Fixed zstd compression dependencies (Boundless #370)

Raise tonic's optional zstd dependency from 0.13 to at least 0.14.3.
RUSTSEC-2026-0391 affects zstd before 0.14.3; the old line also retains
zstd-safe 7.3.0, affected by RUSTSEC-2026-0395 through RUSTSEC-2026-0406.
The fixed line permits zstd-safe 8.2.0 and zstd-sys 2.1.2, which fixes
RUSTSEC-2026-0410. Boundless uses tonic's zstd request and response compression,
so disabling the feature would remove an existing capability.

Upstream's latest stable release is still 0.14.6. Upstream `master` at
`dccf644db0d90ebcbb503804050cdfd1166c2bd6` still requires zstd 0.13.0.
Retire this patch when a compatible upstream release requires the fixed line.

The existing codec remains the single framing/compression checkpoint. Its
private compression functions use zstd's stream Encoder and Decoder; no API
adaptation or alternative compression path is introduced. Cargo's minimum
version requirement applies to every consumer enabling tonic's zstd feature.

Regression evidence: `cargo test -p compression -- zstd` exercises request and
response compression, negotiation, unary and streaming calls with the existing
upstream fixtures: 21 passed on macOS arm64. Boundless's advisory gate checks the resolved dependency
versions, and its independent Python peer exercises zstd on the wire.

## Typed compression-header refusal (Boundless #375)

`CompressionEncoding::from_encoding_header` remains the single acceptance
checkpoint for request and response compression. Its private construction of
`codec::CompressionError` keeps the first received header's bytes and an
`Unsupported` reason in the Status error source. Unknown and disabled encodings
retain Code::Unimplemented and the accepted-encoding response metadata. UTF-8 header values retain upstream's existing refusal message. Only non-UTF-8
values use a fixed safe message; both cases attach the same typed source.

Boundless reads this source instead of repeating Tonic's acceptance rules.
Regression: `codec::compression::tests::refused_encoding_retains_typed_bytes_and_reason`
checks unknown, disabled and non-UTF-8 values, identity and enabled gzip;
`cargo test -p tonic --lib --features gzip,deflate,zstd -- codec::` and
`cargo test -p compression` exercise the existing codec and compression paths
(7 codec and 63 compression tests passed on macOS arm64). The compression
response fixture also reads the public typed source; request and client-stream
fixtures retain upstream's UTF-8 refusal text after its wire round trip.
The checkpoint regression checks both message paths and their typed sources.
Retire when upstream exposes equivalent structured refusal data.

## A compressed message decodes only as complete coded data (Boundless #280)

`codec::compression::decompress`, called only by the decoder's
`decode_chunk`, is the single checkpoint for compressed messages in both
directions. Upstream reads the message's `len` bytes with flate2's
single-stream `GzDecoder` or `ZlibDecoder` through `std::io::copy`, then
advances past all `len` bytes whether the decoder used them or not (upstream
`master` `dccf644d` is unchanged). A gzip message holding a second member or
trailing bytes therefore decodes as its first member only, and a deflate
message decodes as whatever its zlib stream produced before the bytes ended:
trailing bytes are dropped, and a stream cut short, even an empty one, is
read as a complete, shorter message. The call then ends with the peer's
`grpc-status: 0`.

The patch keeps the signature, framing, size-limited writer and status
conversion. gzip reads members in sequence (`MultiGzDecoder`), as RFC 1952
defines a gzip file and grpc-go's multistream `gzip.Reader` reads to EOF;
anything after the last member is an error. deflate inflates one zlib stream
with `flate2::Decompress` and requires its stream end (Adler-32 checked) with
every input byte used, as gRPC core's `ZlibBody` refuses "not all input
consumed" and any end other than `Z_STREAM_END`. zstd is unchanged: its
decoder already reads frames in sequence and refuses trailing or cut data.
The decompressed-size limit still fails with `WriteZero`, which the decoder
maps to `resource_exhausted`; every other failure stays `Status::internal`.

Regression: `codec::compression::tests::a_message_decodes_only_as_complete_coded_data`
decodes, for each encoding, one stream, two streams, trailing garbage,
trailing zeros, a cut checksum, a stream cut in half, an empty message and a
message larger than one inflate chunk, and checks the size limit.
`cargo test -p tonic --lib --features gzip,deflate,zstd -- codec::` (8 passed)
and `cargo test -p compression` (63 passed) pass on macOS arm64. Retire when
upstream checks that decoding used the whole message.
