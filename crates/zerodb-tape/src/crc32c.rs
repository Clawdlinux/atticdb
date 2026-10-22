//! CRC32C (Castagnoli). The `crc32c` crate uses the CPU's CRC instructions when present.

/// An incremental CRC32C checksum.
#[derive(Clone, Debug, Default)]
pub struct Crc32c(u32);

impl Crc32c {
    /// Starts an empty checksum.
    pub const fn new() -> Self {
        Self(0)
    }

    /// Includes more bytes in the checksum.
    pub fn update(&mut self, data: &[u8]) {
        self.0 = ::crc32c::crc32c_append(self.0, data);
    }

    /// Returns the checksum without changing its state.
    pub const fn finish(&self) -> u32 {
        self.0
    }
}

/// Computes the CRC32C checksum of a byte slice.
pub fn crc32c(data: &[u8]) -> u32 {
    ::crc32c::crc32c(data)
}
