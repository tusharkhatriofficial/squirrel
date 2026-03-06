//! GGUF file format parser — reads model weights, metadata, and tensor layout.
//!
//! GGUF (GGML Universal Format) is the standard format for quantized LLM
//! weights used by llama.cpp and the broader community. This parser reads
//! the binary format directly in pure Rust — no C dependencies.
//!
//! Format overview (GGUF v3):
//!   - Magic: "GGUF" (4 bytes)
//!   - Version: u32 (we support v2 and v3)
//!   - Tensor count: u64
//!   - Metadata KV count: u64
//!   - Metadata key-value pairs (string keys, typed values)
//!   - Tensor info entries (name, dimensions, type, offset)
//!   - Alignment padding
//!   - Raw tensor data (contiguous block)
//!
//! Reference: https://github.com/ggerganov/ggml/blob/master/docs/gguf.md

use alloc::{collections::BTreeMap, string::String, vec::Vec};

/// Magic bytes at the start of every GGUF file.
const GGUF_MAGIC: u32 = 0x46475547; // "GGUF" in little-endian

/// Supported GGUF format versions.
const GGUF_VERSION_2: u32 = 2;
const GGUF_VERSION_3: u32 = 3;

/// Default alignment for tensor data (32 bytes).
const DEFAULT_ALIGNMENT: usize = 32;

/// GGML tensor data types.
///
/// We support F32 and F16 for the MVP. Quantized types (Q4_0, Q4_1, Q8_0, etc.)
/// are defined but dequantization is deferred — models must use F32 or F16
/// weights initially.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum GgmlType {
    F32 = 0,
    F16 = 1,
    Q4_0 = 2,
    Q4_1 = 3,
    Q5_0 = 6,
    Q5_1 = 7,
    Q8_0 = 8,
    Q8_1 = 9,
    Q2K = 10,
    Q3K = 11,
    Q4K = 12,
    Q5K = 13,
    Q6K = 14,
    Q8K = 15,
    IQ2XXS = 16,
    IQ2XS = 17,
    IQ3XXS = 18,
    IQ1S = 19,
    IQ4NL = 20,
    IQ3S = 21,
    IQ2S = 22,
    IQ4XS = 23,
}

impl GgmlType {
    /// Parse a u32 into a GgmlType.
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::F32),
            1 => Some(Self::F16),
            2 => Some(Self::Q4_0),
            3 => Some(Self::Q4_1),
            6 => Some(Self::Q5_0),
            7 => Some(Self::Q5_1),
            8 => Some(Self::Q8_0),
            9 => Some(Self::Q8_1),
            10 => Some(Self::Q2K),
            11 => Some(Self::Q3K),
            12 => Some(Self::Q4K),
            13 => Some(Self::Q5K),
            14 => Some(Self::Q6K),
            15 => Some(Self::Q8K),
            16 => Some(Self::IQ2XXS),
            17 => Some(Self::IQ2XS),
            18 => Some(Self::IQ3XXS),
            19 => Some(Self::IQ1S),
            20 => Some(Self::IQ4NL),
            21 => Some(Self::IQ3S),
            22 => Some(Self::IQ2S),
            23 => Some(Self::IQ4XS),
            _ => None,
        }
    }

    /// Size of one element in bytes for this type.
    /// For block-quantized types, returns the block size.
    pub fn element_size(&self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 => 2,
            Self::Q4_0 => 18,  // block of 32: 2 bytes scale + 16 bytes data
            Self::Q4_1 => 20,  // block of 32: 2+2 bytes + 16 bytes data
            Self::Q5_0 => 22,  // block of 32
            Self::Q5_1 => 24,  // block of 32
            Self::Q8_0 => 34,  // block of 32: 2 bytes scale + 32 bytes data
            Self::Q8_1 => 40,  // block of 32
            _ => 0, // Other quantization types — size varies
        }
    }

    /// Number of elements per block for quantized types.
    pub fn block_size(&self) -> usize {
        match self {
            Self::F32 | Self::F16 => 1,
            _ => 32, // All GGML quantized types use blocks of 32
        }
    }

    /// Calculate total bytes for `n_elements` of this type.
    pub fn tensor_bytes(&self, n_elements: usize) -> usize {
        match self {
            Self::F32 => n_elements * 4,
            Self::F16 => n_elements * 2,
            _ => {
                let bs = self.block_size();
                let n_blocks = (n_elements + bs - 1) / bs;
                n_blocks * self.element_size()
            }
        }
    }
}

/// Metadata value types in GGUF key-value pairs.
#[derive(Debug, Clone)]
pub enum MetadataValue {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(f32),
    F64(f64),
    Bool(bool),
    String(String),
    Array(Vec<MetadataValue>),
}

impl MetadataValue {
    /// Extract as u32 if applicable.
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::U32(v) => Some(*v),
            Self::U64(v) => Some(*v as u32),
            Self::I32(v) => Some(*v as u32),
            _ => None,
        }
    }

    /// Extract as u64 if applicable.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(v) => Some(*v),
            Self::U32(v) => Some(*v as u64),
            Self::I64(v) => Some(*v as u64),
            _ => None,
        }
    }

    /// Extract as f32 if applicable.
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Self::F32(v) => Some(*v),
            Self::F64(v) => Some(*v as f32),
            _ => None,
        }
    }

    /// Extract as string if applicable.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Describes a single tensor in the GGUF file.
#[derive(Debug, Clone)]
pub struct TensorInfo {
    /// Tensor name (e.g. "blk.0.attn_q.weight")
    pub name: String,
    /// Number of dimensions (1-4)
    pub n_dims: u32,
    /// Shape in each dimension
    pub dims: [u64; 4],
    /// Data type (F32, F16, Q4_0, etc.)
    pub dtype: GgmlType,
    /// Byte offset from the start of the tensor data section
    pub offset: u64,
}

impl TensorInfo {
    /// Total number of elements in this tensor.
    pub fn n_elements(&self) -> usize {
        let mut n = 1usize;
        for i in 0..self.n_dims as usize {
            n *= self.dims[i] as usize;
        }
        n
    }

    /// Total bytes this tensor occupies.
    pub fn byte_size(&self) -> usize {
        self.dtype.tensor_bytes(self.n_elements())
    }
}

/// A parsed GGUF file.
pub struct GgufFile<'a> {
    /// Raw file data (memory-mapped or loaded into a buffer)
    pub data: &'a [u8],
    /// GGUF format version
    pub version: u32,
    /// Metadata key-value pairs
    pub metadata: BTreeMap<String, MetadataValue>,
    /// Tensor descriptors
    pub tensors: BTreeMap<String, TensorInfo>,
    /// Byte offset where tensor data begins
    pub tensor_data_offset: usize,
}

/// Cursor for reading binary data from a byte slice.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn read_u8(&mut self) -> Option<u8> {
        if self.pos >= self.data.len() {
            return None;
        }
        let v = self.data[self.pos];
        self.pos += 1;
        Some(v)
    }

    fn read_u16(&mut self) -> Option<u16> {
        if self.pos + 2 > self.data.len() {
            return None;
        }
        let v = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Some(v)
    }

    fn read_u32(&mut self) -> Option<u32> {
        if self.pos + 4 > self.data.len() {
            return None;
        }
        let bytes: [u8; 4] = self.data[self.pos..self.pos + 4].try_into().ok()?;
        let v = u32::from_le_bytes(bytes);
        self.pos += 4;
        Some(v)
    }

    fn read_u64(&mut self) -> Option<u64> {
        if self.pos + 8 > self.data.len() {
            return None;
        }
        let bytes: [u8; 8] = self.data[self.pos..self.pos + 8].try_into().ok()?;
        let v = u64::from_le_bytes(bytes);
        self.pos += 8;
        Some(v)
    }

    fn read_i8(&mut self) -> Option<i8> {
        self.read_u8().map(|v| v as i8)
    }

    fn read_i16(&mut self) -> Option<i16> {
        self.read_u16().map(|v| v as i16)
    }

    fn read_i32(&mut self) -> Option<i32> {
        self.read_u32().map(|v| v as i32)
    }

    fn read_i64(&mut self) -> Option<i64> {
        self.read_u64().map(|v| v as i64)
    }

    fn read_f32(&mut self) -> Option<f32> {
        self.read_u32().map(f32::from_bits)
    }

    fn read_f64(&mut self) -> Option<f64> {
        self.read_u64().map(f64::from_bits)
    }

    fn read_bool(&mut self) -> Option<bool> {
        self.read_u8().map(|v| v != 0)
    }

    fn read_string(&mut self) -> Option<String> {
        let len = self.read_u64()? as usize;
        if self.pos + len > self.data.len() {
            return None;
        }
        let s = core::str::from_utf8(&self.data[self.pos..self.pos + len]).ok()?;
        self.pos += len;
        Some(String::from(s))
    }

    fn read_metadata_value(&mut self, value_type: u32) -> Option<MetadataValue> {
        match value_type {
            0 => self.read_u8().map(MetadataValue::U8),
            1 => self.read_i8().map(MetadataValue::I8),
            2 => self.read_u16().map(MetadataValue::U16),
            3 => self.read_i16().map(MetadataValue::I16),
            4 => self.read_u32().map(MetadataValue::U32),
            5 => self.read_i32().map(MetadataValue::I32),
            6 => self.read_f32().map(MetadataValue::F32),
            7 => self.read_bool().map(MetadataValue::Bool),
            8 => self.read_string().map(MetadataValue::String),
            9 => {
                // Array: element_type (u32) + count (u64) + elements
                let elem_type = self.read_u32()?;
                let count = self.read_u64()? as usize;
                let mut arr = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    arr.push(self.read_metadata_value(elem_type)?);
                }
                Some(MetadataValue::Array(arr))
            }
            10 => self.read_u64().map(MetadataValue::U64),
            11 => self.read_i64().map(MetadataValue::I64),
            12 => self.read_f64().map(MetadataValue::F64),
            _ => None,
        }
    }
}

impl<'a> GgufFile<'a> {
    /// Parse a GGUF file from raw bytes.
    ///
    /// The data slice must contain the complete GGUF file. This parser
    /// reads the header, metadata, and tensor info, but does NOT copy
    /// tensor data — it references it via offsets into the original slice.
    pub fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        let mut r = Reader::new(data);

        // Read and validate magic
        let magic = r.read_u32().ok_or("GGUF: truncated magic")?;
        if magic != GGUF_MAGIC {
            return Err("GGUF: invalid magic (not a GGUF file)");
        }

        // Read version
        let version = r.read_u32().ok_or("GGUF: truncated version")?;
        if version != GGUF_VERSION_2 && version != GGUF_VERSION_3 {
            return Err("GGUF: unsupported version (need v2 or v3)");
        }

        // Read counts
        let tensor_count = r.read_u64().ok_or("GGUF: truncated tensor count")? as usize;
        let metadata_kv_count = r.read_u64().ok_or("GGUF: truncated metadata count")? as usize;

        // Sanity check counts
        if tensor_count > 100_000 || metadata_kv_count > 100_000 {
            return Err("GGUF: unreasonable tensor/metadata count");
        }

        // Read metadata key-value pairs
        let mut metadata = BTreeMap::new();
        for _ in 0..metadata_kv_count {
            let key = r.read_string().ok_or("GGUF: truncated metadata key")?;
            let value_type = r.read_u32().ok_or("GGUF: truncated metadata value type")?;
            let value = r
                .read_metadata_value(value_type)
                .ok_or("GGUF: truncated metadata value")?;
            metadata.insert(key, value);
        }

        // Read alignment from metadata (or use default)
        let alignment = metadata
            .get("general.alignment")
            .and_then(|v| v.as_u32())
            .unwrap_or(DEFAULT_ALIGNMENT as u32) as usize;

        // Read tensor info entries
        let mut tensors = BTreeMap::new();
        for _ in 0..tensor_count {
            let name = r.read_string().ok_or("GGUF: truncated tensor name")?;
            let n_dims = r.read_u32().ok_or("GGUF: truncated tensor dims")?;
            if n_dims > 4 {
                return Err("GGUF: tensor has more than 4 dimensions");
            }
            let mut dims = [0u64; 4];
            for d in 0..n_dims as usize {
                dims[d] = r.read_u64().ok_or("GGUF: truncated tensor dim")?;
            }
            let dtype_raw = r.read_u32().ok_or("GGUF: truncated tensor type")?;
            let dtype =
                GgmlType::from_u32(dtype_raw).ok_or("GGUF: unknown tensor data type")?;
            let offset = r.read_u64().ok_or("GGUF: truncated tensor offset")?;

            tensors.insert(
                name.clone(),
                TensorInfo {
                    name,
                    n_dims,
                    dims,
                    dtype,
                    offset,
                },
            );
        }

        // Tensor data starts after the header, aligned
        let tensor_data_offset = align_up(r.pos, alignment);

        // Verify we have enough data
        if tensor_data_offset > data.len() {
            return Err("GGUF: tensor data offset past end of file");
        }

        Ok(GgufFile {
            data,
            version,
            metadata,
            tensors,
            tensor_data_offset,
        })
    }

    /// Get the raw bytes for a named tensor.
    pub fn tensor_data(&self, name: &str) -> Option<&'a [u8]> {
        let info = self.tensors.get(name)?;
        let start = self.tensor_data_offset + info.offset as usize;
        let end = start + info.byte_size();
        if end > self.data.len() {
            return None;
        }
        Some(&self.data[start..end])
    }

    /// Get a metadata string value.
    pub fn meta_str(&self, key: &str) -> Option<&str> {
        self.metadata.get(key)?.as_str()
    }

    /// Get a metadata u32 value.
    pub fn meta_u32(&self, key: &str) -> Option<u32> {
        self.metadata.get(key)?.as_u32()
    }

    /// Get a metadata u64 value.
    pub fn meta_u64(&self, key: &str) -> Option<u64> {
        self.metadata.get(key)?.as_u64()
    }

    /// Get a metadata f32 value.
    pub fn meta_f32(&self, key: &str) -> Option<f32> {
        self.metadata.get(key)?.as_f32()
    }
}

fn align_up(offset: usize, alignment: usize) -> usize {
    (offset + alignment - 1) & !(alignment - 1)
}
