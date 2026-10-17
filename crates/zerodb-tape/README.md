# Tape primitives

This crate provides CRC32C, file I/O, clocks, deterministic disk faults, and frame codecs.
It does not implement the tape writer or recovery.

## I/O and simulation

`Io` is object-safe and works through `Arc<dyn Io>`.
`StdIo` uses real files. Its parent directories must already exist.
`SimDisk` uses logical paths with implicit parent directories.
Both return sorted full paths from `list`.
Handles remain valid across rename and removal. Exclusive locks follow file identity.
Call `close` to release a handle. Closed handles return errors.
Open handles retain removed files until closed.
Simulated files remain available while durable directory entries or pending directory operations still reference them.

`SimClock` advances manually and can be shared through `Arc`.
`SystemClock` returns wall-clock nanoseconds and can move backward after clock adjustments.

`SimDisk::crash` returns an independent disk without changing the original.
The same seed and operations produce the same image.
Synced file bytes and synced directory entries survive.
Pending writes can disappear, reorder, or persist only a prefix.
Pending directory operations can disappear. Each rename is atomic.
Cross-directory rename syncs persist both connected directories.
This is stronger than real filesystems that require both directories to be synced separately.

`fail_next_sync` fails the next file or directory sync.
Failed file syncs doom their pending writes. Cached bytes remain readable, but crashes drop those writes.
Retrying sync returns success without healing doomed writes.
Later writes can sync independently without persisting earlier cached bytes.
`enospc_after_bytes` sets a remaining write budget. Exhaustion returns `ErrorKind::StorageFull`, possibly after writing a prefix.
`flip_bit` damages a durable byte. New crash images reset fault knobs and release locks.

## Frames

Frames use the 104-byte header and little-endian fields from the architecture document.
`encode` derives the CRC and total length.
It panics on invalid structure or total sizes above 4 MiB.
Callers must validate batch sizes before encoding.
Normal record payloads start with a 16-byte salt.
Footer payloads contain exactly 44 bytes. Husks have no payload.

`decode` returns the first frame and its consumed byte count.
Trailing bytes belong to subsequent frames or preallocated space.
An incomplete fixed header returns `Truncated`.
A declared length beyond the supplied bytes returns `BadLength`.
This keeps single-bit length corruption classified as corruption, rather than an incomplete header.
End markers require both zero length and zero CRC.
Without that check, flipping a power-of-2 frame length could hide a damaged frame.
Unknown kinds, flags, and invalid structures return `BadLength`.
Unknown versions return `UnknownVersion` only after CRC validation.

`canon` contains exactly 30 bytes. Storage flags and payload length never enter the tenant chain.
Content digests retain the salt commitment after a payload becomes a husk.
Batch digests hash ordered content digests, not raw payloads.
The caller generates salts. This crate does not provide random salt generation.

## Validation

Run from the workspace root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree
```

Crash tests exercise 1,000 seeds per seeded scenario.
`proptest` runs without fork or timeout features. Tests need no external directory.
