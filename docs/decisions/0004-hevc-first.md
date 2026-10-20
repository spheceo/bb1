# 0004 — HEVC first, H.264 fallback

**Status:** accepted, pending M0 encoder check

## Context

Image quality like Parsec's matters, especially sharp text. Both host and the main client are Macs with hardware HEVC encode/decode.

## Decision

Use HEVC (H.265) by default between Macs. Fall back to H.264 when a client can't decode HEVC, or if M0 shows that low-latency mode only works with H.264 on the host.

## Why

At the same bitrate, HEVC keeps text and fine detail sharper than H.264. Both run on dedicated hardware, so the codec choice doesn't change CPU load or latency much.

## Consequences

- `HELLO` lists codecs each side supports; host picks the best common one.
- Windows clients need hardware HEVC decode (most GPUs since ~2016 have it); otherwise H.264.
