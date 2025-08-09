use std::collections::HashMap;

// 测试 base64_encode 和 base64_decode 函数
#[test]
fn test_base64_roundtrip() {
    // 构建编码映射表
    let (en_map, de_map) = super::build_maps();
    
    // 测试数据
    let test_data = b"Hello, world!"; 
    
    // 编码
    let encoded = super::base64_encode(test_data, &en_map);
    
    // 解码
    let decoded = super::base64_decode(&encoded, &de_map).expect("Decode failed");
    
    // 验证
    assert_eq!(test_data, decoded.as_slice());
}

// 测试 blv_encode 和 blv_decode 函数
#[test]
fn test_blv_roundtrip() {
    // 测试数据
    let mut test_info = HashMap::new();
    test_info.insert(1, b"data1".to_vec());
    test_info.insert(2, b"data2".to_vec());
    
    // 编码
    let encoded = super::blv_encode(&test_info);
    
    // 解码
    let decoded = super::blv_decode(&encoded);
    
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
    let data = super::rand_byte();
    
    // 验证长度在 5-20 之间
    assert!(data.len() >= 5 && data.len() <= 20);
}

// 测试 build_maps 函数
#[test]
fn test_build_maps() {
    let (en_map, de_map) = super::build_maps();
    
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