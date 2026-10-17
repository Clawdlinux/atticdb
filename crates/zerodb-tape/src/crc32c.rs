//! Portable Castagnoli CRC32C with slicing-by-8 tables.

const fn tables() -> [[u32; 256]; 8] {
    let mut tables = [[0; 256]; 8];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0x82F6_3B78 & 0u32.wrapping_sub(crc & 1));
            bit += 1;
        }
        tables[0][byte] = crc;
        byte += 1;
    }
    let mut slice = 1;
    while slice < 8 {
        byte = 0;
        while byte < 256 {
            let crc = tables[slice - 1][byte];
            let crc = (crc >> 8) ^ tables[0][(crc & 0xff) as usize];
            tables[slice][byte] = crc;
            byte += 1;
        }
        slice += 1;
    }
    tables
}

const TABLES: [[u32; 256]; 8] = tables();

/// An incremental CRC32C checksum.
#[derive(Clone, Debug)]
pub struct Crc32c(u32);

impl Crc32c {
    /// Starts an empty checksum.
    pub const fn new() -> Self {
        Self(!0)
    }

    /// Includes more bytes in the checksum.
    pub fn update(&mut self, data: &[u8]) {
        let (chunks, remainder) = data.as_chunks::<8>();
        for bytes in chunks {
            let low = self.0 ^ u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            self.0 = TABLES[7][(low & 0xff) as usize]
                ^ TABLES[6][((low >> 8) & 0xff) as usize]
                ^ TABLES[5][((low >> 16) & 0xff) as usize]
                ^ TABLES[4][(low >> 24) as usize]
                ^ TABLES[3][usize::from(bytes[4])]
                ^ TABLES[2][usize::from(bytes[5])]
                ^ TABLES[1][usize::from(bytes[6])]
                ^ TABLES[0][usize::from(bytes[7])];
        }
        for byte in remainder {
            self.0 = (self.0 >> 8) ^ TABLES[0][((self.0 ^ u32::from(*byte)) & 0xff) as usize];
        }
    }

    /// Returns the checksum without changing its state.
    pub const fn finish(&self) -> u32 {
        !self.0
    }
}

impl Default for Crc32c {
    fn default() -> Self {
        Self::new()
    }
}

/// Computes the CRC32C checksum of a byte slice.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = Crc32c::new();
    crc.update(data);
    crc.finish()
}
