use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const GGUF_MAGIC: [u8; 4] = *b"GGUF";
const MAX_STRING_LEN: u64 = 65_536;
const MAX_KV_COUNT: u64 = 4_096;
const MAX_TENSOR_COUNT: u64 = 262_144;
const MAX_ARRAY_LEN: u64 = 1_048_576;
const MAX_DIMS: u32 = 8;
const MAX_BLOCK_COUNT: u64 = 512;

#[derive(Debug)]
pub enum GgufError {
    Io(std::io::Error),
    Malformed(String),
    Unsupported(String),
}

impl fmt::Display for GgufError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GgufError::Io(e) => write!(f, "gguf read failed: {e}"),
            GgufError::Malformed(msg) => write!(f, "gguf file is malformed: {msg}"),
            GgufError::Unsupported(msg) => write!(f, "gguf file is unsupported: {msg}"),
        }
    }
}

impl std::error::Error for GgufError {}

impl From<std::io::Error> for GgufError {
    fn from(e: std::io::Error) -> Self {
        GgufError::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct GgufModelInfo {
    pub n_layers: u32,
    pub layer_bytes: Vec<u64>,
    pub non_layer_bytes: u64,
    pub output_bytes: u64,
    pub kv_bytes_per_token: u64,
    pub n_vocab: u64,
}

impl GgufModelInfo {
    pub fn total_bytes(&self) -> u64 {
        self.layer_bytes
            .iter()
            .fold(self.non_layer_bytes, |acc, &b| acc.saturating_add(b))
    }
}

pub fn read_model_info(path: &Path) -> Result<GgufModelInfo, GgufError> {
    let file = std::fs::File::open(path)?;
    parse(std::io::BufReader::new(file))
}

fn parse<R: Read + Seek>(mut r: R) -> Result<GgufModelInfo, GgufError> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if magic != GGUF_MAGIC {
        return Err(GgufError::Malformed("bad magic".into()));
    }
    let version = read_u32(&mut r)?;
    if !(2..=3).contains(&version) {
        return Err(GgufError::Unsupported(format!("gguf version {version}")));
    }
    let tensor_count = read_u64(&mut r)?;
    if tensor_count > MAX_TENSOR_COUNT {
        return Err(GgufError::Malformed(format!(
            "tensor count {tensor_count} exceeds limit"
        )));
    }
    let kv_count = read_u64(&mut r)?;
    if kv_count > MAX_KV_COUNT {
        return Err(GgufError::Malformed(format!(
            "metadata count {kv_count} exceeds limit"
        )));
    }

    let mut meta: HashMap<String, u64> = HashMap::new();
    for _ in 0..kv_count {
        let key = read_string(&mut r)?;
        let value_type = read_u32(&mut r)?;
        if let Some(value) = read_scalar_as_u64(&mut r, value_type)? {
            meta.insert(key, value);
        }
    }

    let block_count = meta_suffix(&meta, ".block_count")
        .ok_or_else(|| GgufError::Malformed("missing block_count metadata".into()))?;
    if block_count == 0 || block_count > MAX_BLOCK_COUNT {
        return Err(GgufError::Malformed(format!(
            "block_count {block_count} out of range"
        )));
    }
    let n_layers = block_count as u32;

    let mut layer_bytes = vec![0u64; n_layers as usize];
    let mut non_layer_bytes = 0u64;
    let mut output_weight_bytes = 0u64;
    let mut output_norm_bytes = 0u64;
    let mut token_embd_bytes = 0u64;
    let mut output_vocab = 0u64;
    let mut token_embd_vocab = 0u64;
    for _ in 0..tensor_count {
        let name = read_string(&mut r)?;
        let n_dims = read_u32(&mut r)?;
        if n_dims > MAX_DIMS {
            return Err(GgufError::Malformed(format!(
                "tensor '{name}' has {n_dims} dims"
            )));
        }
        let mut n_elements = 1u64;
        let mut dims: Vec<u64> = Vec::with_capacity(n_dims as usize);
        for _ in 0..n_dims {
            let dim = read_u64(&mut r)?;
            dims.push(dim);
            n_elements = n_elements
                .checked_mul(dim)
                .ok_or_else(|| GgufError::Malformed(format!("tensor '{name}' overflows")))?;
        }
        let ggml_type = read_u32(&mut r)?;
        let _offset = read_u64(&mut r)?;
        let bytes = tensor_bytes(ggml_type, n_elements).ok_or_else(|| {
            GgufError::Unsupported(format!("tensor '{name}' has unknown type {ggml_type}"))
        })?;
        match block_index(&name) {
            Some(idx) if idx < n_layers as usize => {
                layer_bytes[idx] = layer_bytes[idx].saturating_add(bytes)
            }
            Some(idx) => {
                return Err(GgufError::Malformed(format!(
                    "tensor '{name}' block {idx} exceeds block_count {n_layers}"
                )))
            }
            None => {
                non_layer_bytes = non_layer_bytes.saturating_add(bytes);
                let vocab_dim = dims.get(1).copied().unwrap_or(0);
                match name.as_str() {
                    "output.weight" => {
                        output_weight_bytes = bytes;
                        output_vocab = vocab_dim;
                    }
                    "output_norm.weight" => output_norm_bytes = bytes,
                    "token_embd.weight" => {
                        token_embd_bytes = bytes;
                        token_embd_vocab = vocab_dim;
                    }
                    _ => {}
                }
            }
        }
    }
    let output_bytes = if output_weight_bytes > 0 {
        output_weight_bytes
    } else {
        token_embd_bytes
    }
    .saturating_add(output_norm_bytes);

    let embedding_length = meta_suffix(&meta, ".embedding_length").unwrap_or(0);
    let head_count = meta_suffix(&meta, ".attention.head_count").unwrap_or(0);
    let head_count_kv = meta_suffix(&meta, ".attention.head_count_kv").unwrap_or(head_count);
    let kv_bytes_per_token = if embedding_length > 0 && head_count > 0 {
        let head_dim = embedding_length / head_count;
        4u64.saturating_mul(u64::from(n_layers))
            .saturating_mul(head_count_kv)
            .saturating_mul(head_dim)
    } else {
        0
    };

    let n_vocab = if output_vocab > 0 {
        output_vocab
    } else {
        token_embd_vocab
    };

    Ok(GgufModelInfo {
        n_layers,
        layer_bytes,
        non_layer_bytes,
        output_bytes,
        kv_bytes_per_token,
        n_vocab,
    })
}

fn meta_suffix(meta: &HashMap<String, u64>, suffix: &str) -> Option<u64> {
    meta.iter()
        .find(|(k, _)| k.ends_with(suffix))
        .map(|(_, v)| *v)
}

fn block_index(name: &str) -> Option<usize> {
    let rest = name.strip_prefix("blk.")?;
    let end = rest.find('.')?;
    rest[..end].parse().ok()
}

fn tensor_bytes(ggml_type: u32, n_elements: u64) -> Option<u64> {
    let (block_size, type_size) = match ggml_type {
        0 => (1, 4),
        1 => (1, 2),
        2 => (32, 18),
        3 => (32, 20),
        6 => (32, 22),
        7 => (32, 24),
        8 => (32, 34),
        9 => (32, 36),
        10 => (256, 84),
        11 => (256, 110),
        12 => (256, 144),
        13 => (256, 176),
        14 => (256, 210),
        15 => (256, 292),
        16 => (256, 66),
        17 => (256, 74),
        18 => (256, 98),
        19 => (256, 50),
        20 => (32, 18),
        21 => (256, 110),
        22 => (256, 82),
        23 => (256, 136),
        24 => (1, 1),
        25 => (1, 2),
        26 => (1, 4),
        27 => (1, 8),
        28 => (1, 8),
        29 => (256, 56),
        30 => (1, 2),
        _ => return None,
    };
    Some(n_elements.div_ceil(block_size).saturating_mul(type_size))
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, GgufError> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64<R: Read>(r: &mut R) -> Result<u64, GgufError> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_string<R: Read>(r: &mut R) -> Result<String, GgufError> {
    let len = read_u64(r)?;
    if len > MAX_STRING_LEN {
        return Err(GgufError::Malformed(format!(
            "string length {len} exceeds limit"
        )));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|_| GgufError::Malformed("string is not utf-8".into()))
}

fn read_scalar_as_u64<R: Read + Seek>(
    r: &mut R,
    value_type: u32,
) -> Result<Option<u64>, GgufError> {
    match value_type {
        0 | 1 | 7 => {
            let mut buf = [0u8; 1];
            r.read_exact(&mut buf)?;
            Ok(Some(u64::from(buf[0])))
        }
        2 | 3 => {
            let mut buf = [0u8; 2];
            r.read_exact(&mut buf)?;
            Ok(Some(u64::from(u16::from_le_bytes(buf))))
        }
        4 | 5 => Ok(Some(u64::from(read_u32(r)?))),
        6 => {
            r.seek(SeekFrom::Current(4))?;
            Ok(None)
        }
        8 => {
            read_string(r)?;
            Ok(None)
        }
        9 => {
            let elem_type = read_u32(r)?;
            let len = read_u64(r)?;
            if len > MAX_ARRAY_LEN {
                return Err(GgufError::Malformed(format!(
                    "array length {len} exceeds limit"
                )));
            }
            match scalar_size(elem_type) {
                Some(size) => {
                    r.seek(SeekFrom::Current((len * size) as i64))?;
                }
                None if elem_type == 8 => {
                    for _ in 0..len {
                        read_string(r)?;
                    }
                }
                None => {
                    return Err(GgufError::Unsupported(format!(
                        "array element type {elem_type}"
                    )))
                }
            }
            Ok(None)
        }
        10 => Ok(Some(read_u64(r)?)),
        11 => {
            let value = read_u64(r)? as i64;
            Ok(u64::try_from(value).ok())
        }
        12 => {
            r.seek(SeekFrom::Current(8))?;
            Ok(None)
        }
        other => Err(GgufError::Unsupported(format!(
            "metadata value type {other}"
        ))),
    }
}

fn scalar_size(value_type: u32) -> Option<u64> {
    match value_type {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct GgufBuilder {
        kvs: Vec<u8>,
        kv_count: u64,
        tensors: Vec<u8>,
        tensor_count: u64,
        version: u32,
    }

    impl GgufBuilder {
        fn new() -> Self {
            Self {
                kvs: Vec::new(),
                kv_count: 0,
                tensors: Vec::new(),
                tensor_count: 0,
                version: 3,
            }
        }

        fn push_str_bytes(buf: &mut Vec<u8>, s: &str) {
            buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }

        fn kv_u32(mut self, key: &str, value: u32) -> Self {
            Self::push_str_bytes(&mut self.kvs, key);
            self.kvs.extend_from_slice(&4u32.to_le_bytes());
            self.kvs.extend_from_slice(&value.to_le_bytes());
            self.kv_count += 1;
            self
        }

        fn kv_u64(mut self, key: &str, value: u64) -> Self {
            Self::push_str_bytes(&mut self.kvs, key);
            self.kvs.extend_from_slice(&10u32.to_le_bytes());
            self.kvs.extend_from_slice(&value.to_le_bytes());
            self.kv_count += 1;
            self
        }

        fn kv_string(mut self, key: &str, value: &str) -> Self {
            Self::push_str_bytes(&mut self.kvs, key);
            self.kvs.extend_from_slice(&8u32.to_le_bytes());
            Self::push_str_bytes(&mut self.kvs, value);
            self.kv_count += 1;
            self
        }

        fn kv_f32_array(mut self, key: &str, values: &[f32]) -> Self {
            Self::push_str_bytes(&mut self.kvs, key);
            self.kvs.extend_from_slice(&9u32.to_le_bytes());
            self.kvs.extend_from_slice(&6u32.to_le_bytes());
            self.kvs
                .extend_from_slice(&(values.len() as u64).to_le_bytes());
            for v in values {
                self.kvs.extend_from_slice(&v.to_le_bytes());
            }
            self.kv_count += 1;
            self
        }

        fn tensor(mut self, name: &str, dims: &[u64], ggml_type: u32) -> Self {
            Self::push_str_bytes(&mut self.tensors, name);
            self.tensors
                .extend_from_slice(&(dims.len() as u32).to_le_bytes());
            for d in dims {
                self.tensors.extend_from_slice(&d.to_le_bytes());
            }
            self.tensors.extend_from_slice(&ggml_type.to_le_bytes());
            self.tensors.extend_from_slice(&0u64.to_le_bytes());
            self.tensor_count += 1;
            self
        }

        fn build(self) -> Vec<u8> {
            let mut buf = Vec::new();
            buf.extend_from_slice(&GGUF_MAGIC);
            buf.extend_from_slice(&self.version.to_le_bytes());
            buf.extend_from_slice(&self.tensor_count.to_le_bytes());
            buf.extend_from_slice(&self.kv_count.to_le_bytes());
            buf.extend_from_slice(&self.kvs);
            buf.extend_from_slice(&self.tensors);
            buf
        }
    }

    fn tiny_model() -> Vec<u8> {
        GgufBuilder::new()
            .kv_string("general.architecture", "llama")
            .kv_u32("llama.block_count", 2)
            .kv_u32("llama.embedding_length", 64)
            .kv_u32("llama.attention.head_count", 8)
            .kv_u32("llama.attention.head_count_kv", 4)
            .kv_f32_array("llama.rope.freqs", &[1.0, 2.0])
            .tensor("token_embd.weight", &[64, 100], 1)
            .tensor("blk.0.attn_q.weight", &[64, 64], 1)
            .tensor("blk.0.ffn_up.weight", &[64, 128], 0)
            .tensor("blk.1.attn_q.weight", &[64, 64], 1)
            .tensor("output.weight", &[64, 100], 0)
            .build()
    }

    #[test]
    fn parses_layer_bytes_and_kv_size() {
        let info = parse(Cursor::new(tiny_model())).unwrap();
        assert_eq!(info.n_layers, 2);
        assert_eq!(info.layer_bytes[0], 64 * 64 * 2 + 64 * 128 * 4);
        assert_eq!(info.layer_bytes[1], 64 * 64 * 2);
        assert_eq!(info.non_layer_bytes, 64 * 100 * 2 + 64 * 100 * 4);
        assert_eq!(info.output_bytes, 64 * 100 * 4);
        assert_eq!(info.kv_bytes_per_token, 2 * 2 * 4 * 8 * 2);
        assert_eq!(
            info.total_bytes(),
            info.layer_bytes.iter().sum::<u64>() + info.non_layer_bytes
        );
    }

    #[test]
    fn tied_embeddings_use_token_embd_as_output_slot() {
        let data = GgufBuilder::new()
            .kv_u32("llama.block_count", 1)
            .tensor("token_embd.weight", &[64, 100], 1)
            .tensor("output_norm.weight", &[64], 0)
            .tensor("blk.0.attn_q.weight", &[64, 64], 1)
            .build();
        let info = parse(Cursor::new(data)).unwrap();
        assert_eq!(info.output_bytes, 64 * 100 * 2 + 64 * 4);
        assert_eq!(
            info.n_vocab, 100,
            "a tied-embedding model must source n_vocab from token_embd.weight,              otherwise the logits term of the compute buffer silently vanishes"
        );
    }

    #[test]
    fn untied_output_weight_supplies_the_vocabulary() {
        let data = GgufBuilder::new()
            .kv_u32("llama.block_count", 1)
            .tensor("token_embd.weight", &[64, 100], 1)
            .tensor("output.weight", &[64, 128], 1)
            .tensor("blk.0.attn_q.weight", &[64, 64], 1)
            .build();
        let info = parse(Cursor::new(data)).unwrap();
        assert_eq!(
            info.n_vocab, 128,
            "output.weight wins over token_embd.weight"
        );
    }

    #[test]
    fn quantized_tensor_sizes_use_block_math() {
        let data = GgufBuilder::new()
            .kv_u32("llama.block_count", 1)
            .tensor("blk.0.ffn_up.weight", &[256, 2], 12)
            .build();
        let info = parse(Cursor::new(data)).unwrap();
        assert_eq!(info.layer_bytes[0], 2 * 144);
    }

    #[test]
    fn rejects_bad_magic() {
        let err = parse(Cursor::new(b"GGML".to_vec())).unwrap_err();
        assert!(matches!(err, GgufError::Malformed(_)));
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut data = tiny_model();
        data[4..8].copy_from_slice(&1u32.to_le_bytes());
        let err = parse(Cursor::new(data)).unwrap_err();
        assert!(matches!(err, GgufError::Unsupported(_)));
    }

    #[test]
    fn rejects_missing_block_count() {
        let data = GgufBuilder::new()
            .kv_string("general.architecture", "llama")
            .build();
        let err = parse(Cursor::new(data)).unwrap_err();
        assert!(err.to_string().contains("block_count"));
    }

    #[test]
    fn rejects_truncated_file() {
        let mut data = tiny_model();
        data.truncate(data.len() - 10);
        let err = parse(Cursor::new(data)).unwrap_err();
        assert!(matches!(err, GgufError::Io(_)));
    }

    #[test]
    fn rejects_oversized_string() {
        let mut data = Vec::new();
        data.extend_from_slice(&GGUF_MAGIC);
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes());
        data.extend_from_slice(&(MAX_STRING_LEN + 1).to_le_bytes());
        let err = parse(Cursor::new(data)).unwrap_err();
        assert!(err.to_string().contains("exceeds limit"));
    }

    #[test]
    fn rejects_tensor_block_beyond_count() {
        let data = GgufBuilder::new()
            .kv_u32("llama.block_count", 1)
            .tensor("blk.5.attn_q.weight", &[8, 8], 0)
            .build();
        let err = parse(Cursor::new(data)).unwrap_err();
        assert!(err.to_string().contains("exceeds block_count"));
    }

    #[test]
    fn u64_metadata_values_are_read() {
        let data = GgufBuilder::new()
            .kv_u64("llama.block_count", 3)
            .tensor("blk.0.attn_q.weight", &[4, 4], 0)
            .build();
        let info = parse(Cursor::new(data)).unwrap();
        assert_eq!(info.n_layers, 3);
    }

    #[test]
    fn kv_bytes_zero_when_metadata_missing() {
        let data = GgufBuilder::new().kv_u32("llama.block_count", 1).build();
        let info = parse(Cursor::new(data)).unwrap();
        assert_eq!(info.kv_bytes_per_token, 0);
    }

    #[test]
    fn head_count_kv_defaults_to_head_count() {
        let data = GgufBuilder::new()
            .kv_u32("llama.block_count", 1)
            .kv_u32("llama.embedding_length", 64)
            .kv_u32("llama.attention.head_count", 8)
            .build();
        let info = parse(Cursor::new(data)).unwrap();
        assert_eq!(info.kv_bytes_per_token, 2 * 8 * 8 * 2);
    }

    #[test]
    fn excessive_block_count_is_rejected() {
        let data = GgufBuilder::new()
            .kv_u64("llama.block_count", 1024)
            .tensor("blk.0.attn_q.weight", &[4, 4], 0)
            .build();
        let err = parse(Cursor::new(data)).unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn reads_real_gguf_when_available() {
        let Ok(path) = std::env::var("TXTCASTER_LLM_GGUF") else {
            return;
        };
        let info = read_model_info(Path::new(&path)).unwrap();
        assert!(info.n_layers > 0);
        assert!(info.total_bytes() > 0);
        assert!(info.layer_bytes.iter().all(|&b| b > 0));
    }
}
