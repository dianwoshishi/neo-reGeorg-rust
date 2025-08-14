use rand::{Rng, RngCore};
use std::collections::HashMap;

use base64::engine::Engine as _;

use crate::{BLV_OFFSET, DE, EN, errors::NeoError};

// 枚举定义
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageField {
    Data = 1,
    Cmd = 2,
    Mark = 3,
    Status = 4,
    Error = 5,
    Ip = 6,
    Port = 7,
    Random1 = 0,  // 用于blv_encode中的额外字段
    Random2 = 39, // 用于blv_encode中的额外字段
}

impl From<MessageField> for i32 {
    fn from(field: MessageField) -> Self {
        field as i32
    }
}

impl TryFrom<i32> for MessageField {
    type Error = NeoError;

    fn try_from(value: i32) -> Result<Self, NeoError> {
        match value {
            1 => Ok(MessageField::Data),
            2 => Ok(MessageField::Cmd),
            3 => Ok(MessageField::Mark),
            4 => Ok(MessageField::Status),
            5 => Ok(MessageField::Error),
            6 => Ok(MessageField::Ip),
            7 => Ok(MessageField::Port),
            0 => Ok(MessageField::Random1),
            39 => Ok(MessageField::Random2),
            _ => Err(NeoError::Other(format!(
                "Invalid message field value: {}",
                value
            ))),
        }
    }
}

/// 从数据中读取并解码长度字段
/// 长度字段为4字节大端序整数，需要减去BLV_OFFSET
pub fn read_and_decode_length(data: &[u8], cursor: &mut usize) -> Result<usize, NeoError> {
    if *cursor + 4 > data.len() {
        return Err(NeoError::Other(
            "Insufficient data for length decoding".to_string(),
        ));
    }

    let l_bytes = [
        data[*cursor],
        data[*cursor + 1],
        data[*cursor + 2],
        data[*cursor + 3],
    ];
    let l = i32::from_be_bytes(l_bytes) - BLV_OFFSET;
    *cursor += 4;

    if l < 0 {
        return Err(NeoError::Other("Decoded length is negative".to_string()));
    }

    Ok(l as usize)
}

// 类型别名
pub type BlvMap = HashMap<i32, Vec<u8>>; // 保持i32类型以便与现有代码兼容

// 编解码模块
#[derive(Clone)]
pub struct Codec {
    en_map: HashMap<u8, u8>,
    de_map: HashMap<u8, u8>,
}

impl Codec {
    /// 创建新的编解码器实例
    pub fn new() -> Self {
        let (en_map, de_map) = Self::build_maps();
        Codec { en_map, de_map }
    }

    /// 构建编码映射表
    fn build_maps() -> (HashMap<u8, u8>, HashMap<u8, u8>) {
        let mut en_map = HashMap::new();
        let mut de_map = HashMap::new();

        assert_eq!(EN.len(), DE.len());

        for i in 0..EN.len() {
            en_map.insert(EN[i], DE[i]);
            de_map.insert(DE[i], EN[i]);
        }

        (en_map, de_map)
    }

    /// 自定义Base64解码
    pub fn base64_decode(&self, data: &[u8]) -> Result<Vec<u8>, NeoError> {
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            out.push(self.de_map.get(&b).copied().unwrap_or(b));
        }
        base64::engine::general_purpose::STANDARD
            .decode(&out)
            .map_err(NeoError::from)
    }

    /// 自定义Base64编码
    pub fn base64_encode(&self, rawdata: &[u8]) -> Vec<u8> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(rawdata);
        let encoded_bytes = encoded.into_bytes();
        let mut out = Vec::with_capacity(encoded_bytes.len());
        for b in encoded_bytes {
            out.push(self.en_map.get(&b).copied().unwrap_or(b));
        }
        out
    }

    /// BLV解码
    pub fn blv_decode(&self, data: &[u8]) -> BlvMap {
        let mut info = BlvMap::new();
        let mut cursor = 0;

        while cursor < data.len() {
            if cursor + 1 > data.len() {
                break;
            }
            let b = data[cursor] as i32;
            cursor += 1;

            // 使用函数封装读取和解码逻辑
            let l = match read_and_decode_length(&data, &mut cursor) {
                Ok(len) => len,
                Err(_) => break,
            };
            if cursor + l > data.len() {
                break;
            }
            let v = data[cursor..cursor + l].to_vec();
            cursor += l;

            info.insert(b, v);
        }

        info
    }

    /// BLV编码
    pub fn blv_encode(&self, info: &BlvMap) -> Vec<u8> {
        let mut data = Vec::new();
        let mut info = info.clone();

        info.insert(MessageField::Random1.into(), Self::rand_byte());
        info.insert(MessageField::Random2.into(), Self::rand_byte());

        for (&b, v) in &info {
            let l = v.len() as i32 + BLV_OFFSET;
            data.push(b as u8);
            data.extend_from_slice(&l.to_be_bytes());
            data.extend_from_slice(v);
        }

        data
    }

    /// 生成随机字节
    fn rand_byte() -> Vec<u8> {
        let mut rng = rand::rng();
        let length = rng.random_range(5..20);
        let mut data = vec![0; length];
        rng.fill_bytes(&mut data);
        data
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // 测试 base64_encode 和 base64_decode 函数
    #[test]
    fn test_base64_roundtrip() {
        // 创建编解码器实例
        let codec = Codec::new();

        // 测试数据
        let test_data = b"Hello, world!";

        // 编码
        let encoded = codec.base64_encode(test_data);

        // 解码
        let decoded = codec.base64_decode(&encoded).expect("Decode failed");

        // 验证
        assert_eq!(test_data, decoded.as_slice());
    }

    // 测试 blv_encode 和 blv_decode 函数
    #[test]
    fn test_blv_roundtrip() {
        // 创建编解码器实例
        let codec = Codec::new();

        // 测试数据
        let mut test_info = HashMap::new();
        test_info.insert(1, b"data1".to_vec());
        test_info.insert(2, b"data2".to_vec());

        // 编码
        let encoded = codec.blv_encode(&test_info);

        // 解码
        let decoded = codec.blv_decode(&encoded);

        // 验证: 由于 blv_encode 会添加随机数据，我们需要排除这些键
        assert_eq!(decoded.get(&1), test_info.get(&1));
        assert_eq!(decoded.get(&2), test_info.get(&2));

        // 验证添加的随机键
        assert!(decoded.contains_key(&0));
        assert!(decoded.contains_key(&39));
    }

    // 测试 rand_byte 函数
    #[test]
    fn test_rand_byte() {
        let data = Codec::rand_byte();

        // 验证长度在 5-20 之间
        assert!(data.len() >= 5 && data.len() <= 20);
    }

    // 测试 build_maps 函数
    #[test]
    fn test_build_maps() {
        let (en_map, de_map) = Codec::build_maps();

        // 验证映射表长度
        assert_eq!(en_map.len(), super::EN.len());
        assert_eq!(de_map.len(), super::DE.len());

        // 验证映射关系
        for i in 0..super::EN.len() {
            let en_char = super::EN[i];
            let de_char = super::DE[i];

            assert_eq!(en_map.get(&en_char), Some(&de_char));
            assert_eq!(de_map.get(&de_char), Some(&en_char));
        }
    }
}