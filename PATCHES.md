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
retain Code::Unimplemented and the accepted-encoding response metadata. Status
text describes the refusal without formatting received bytes.

Boundless reads this source instead of repeating Tonic's acceptance rules.
Regression: `codec::compression::tests::refused_encoding_retains_typed_bytes_and_reason`
checks unknown, disabled and non-UTF-8 values, identity and enabled gzip;
`cargo test -p tonic --lib --features gzip,deflate,zstd -- codec::` and
`cargo test -p compression` exercise the existing codec and compression paths.
Retire when upstream exposes equivalent structured refusal data.
