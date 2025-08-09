use base64::engine::Engine as _;
use rand::{Rng, RngCore};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::io::{self};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;
use tiny_http::Server;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;

// 常量定义
const DATA: i32 = 1;
const CMD: i32 = 2;
const MARK: i32 = 3;
const STATUS: i32 = 4;
const ERROR: i32 = 5;
const IP: i32 = 6;
const PORT: i32 = 7;
const CHANNEL_CAPACITY: usize = 1024;
const BUFFER_SIZE: usize = 1024;
const TIMEOUT_MS: u64 = 10;
const CONNECTION_TIMEOUT_MS: u64 = 3000;

// 自定义Base64编码表
const EN: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const DE: &[u8] = b"dhULNVGsuAk/MxH6ibjcEfRqDWYznXBe9Pl7+SKoZ8pJaICgrQO0mF21yv345wtT";
const BLV_OFFSET: i32 = 1966546385;
const NEO_HELLO: &[u8] = b"6UNI/jhLR7X7fqPmY+m0BofOMNXNbVV2XNbiEVEODRxUbshHWKXC/mQWx0SNYVDFx1bKY0VDjcS3RcS/nGIOzVA0XOdI/cy=";

// 自定义错误类型
#[derive(Debug)]
enum NeoError {
    Io(io::Error),
    SessionClosed,
    Base64Decode(base64::DecodeError),
    Other(String),
}

impl fmt::Display for NeoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NeoError::Io(e) => write!(f, "IO error: {}", e),
            NeoError::SessionClosed => write!(f, "Session is closed"),
            NeoError::Base64Decode(e) => write!(f, "Base64 decode error: {}", e),
            NeoError::Other(s) => write!(f, "Error: {}", s),
        }
    }
}

impl Error for NeoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            NeoError::Io(e) => Some(e),
            NeoError::Base64Decode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for NeoError {
    fn from(e: io::Error) -> Self {
        NeoError::Io(e)
    }
}

impl From<base64::DecodeError> for NeoError {
    fn from(e: base64::DecodeError) -> Self {
        NeoError::Base64Decode(e)
    }
}

// 类型别名
pub type BlvMap = HashMap<i32, Vec<u8>>;
// 全局会话存储
type Sessions = Arc<Mutex<HashMap<String, Session>>>;


// 编解码模块
#[derive(Clone)]
struct Codec {
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
        base64::engine::general_purpose::STANDARD.decode(&out).map_err(NeoError::from)
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

            if cursor + 4 > data.len() {
                break;
            }
            let l_bytes = [
                data[cursor],
                data[cursor + 1],
                data[cursor + 2],
                data[cursor + 3],
            ];
            let l = i32::from_be_bytes(l_bytes) - BLV_OFFSET;
            cursor += 4;

            let l = l as usize;
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

        info.insert(0, Self::rand_byte());
        info.insert(39, Self::rand_byte());

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

// 会话结构体
#[derive(Clone)]
struct Session {
    tx: mpsc::Sender<Vec<u8>>,
    rx_buffer: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
    closed: Arc<Mutex<bool>>,
}

impl Session {
    /// 创建一个新的会话实例
    /// 
    /// 会启动两个异步任务：一个用于从流中读取数据并存储到缓冲区，
    /// 另一个用于从通道接收数据并写入到流中。
    fn new(stream: TcpStream) -> Self {
        // 克隆TcpStream，为两个异步任务提供独立实例
        let read_stream = stream.try_clone()
            .map_err(|e| NeoError::Io(e))
            .expect("Failed to clone stream");
        let write_stream = stream.try_clone()
            .map_err(|e| NeoError::Io(e))
            .expect("Failed to clone stream");

        // 明确指定通道传输类型为Vec<u8>
        let (tx_write, rx_write) = mpsc::channel::<Vec<u8>>(CHANNEL_CAPACITY);
        let (tx_buffer, rx_buffer) = mpsc::channel::<Vec<u8>>(CHANNEL_CAPACITY);
        let closed = Arc::new(Mutex::new(false));
        let rx_buffer = Arc::new(Mutex::new(rx_buffer));

        // 启动读写任务
        Self::start_read_task(read_stream, tx_buffer, Arc::clone(&closed));
        Self::start_write_task(write_stream, rx_write, Arc::clone(&closed));

        Session { tx: tx_write, rx_buffer, closed}
    }

    /// 启动读取任务
    /// 
    /// 从TcpStream读取数据并通过通道发送，直到连接关闭或发生错误。
    fn start_read_task(
        stream: TcpStream,
        tx_buffer: mpsc::Sender<Vec<u8>>,
        closed: Arc<Mutex<bool>>,
    ) {
        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::from_std(stream)
                .map_err(|e| NeoError::Io(e))
                .expect("Failed to convert to async TcpStream");
            let mut buf = [0; BUFFER_SIZE];

            while !*closed.lock().await {
                match stream.read(&mut buf).await {
                    Ok(n) => {
                        if n == 0 {
                            // 连接关闭
                            *closed.lock().await = true;
                            break;
                        }
                        // 发送数据到通道
                        let data = buf[..n].to_vec();
                        if let Err(_e) = tx_buffer.send(data).await {
                            // eprintln!("Send to buffer channel error: {}", e);
                            *closed.lock().await = true;
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Read error: {}", e);
                        *closed.lock().await = true;
                        break;
                    }
                }
            }
            // 尝试优雅关闭
            if let Err(e) = stream.shutdown().await {
                eprintln!("Stream shutdown error: {}", e);
            }
        });
    }

    /// 启动写入任务
    /// 
    /// 从通道接收数据并写入到TcpStream中，直到通道关闭或发生错误。
    fn start_write_task(
        stream: TcpStream,
        mut rx: mpsc::Receiver<Vec<u8>>,
        closed: Arc<Mutex<bool>>,
    ) {
        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::from_std(stream)
                .map_err(|e| NeoError::Io(e))
                .expect("Failed to convert to async TcpStream");

            while let Some(data) = rx.recv().await {
                // 检查关闭状态
                if *closed.lock().await {
                    break;
                }

                // 写入数据
                if let Err(e) = stream.write_all(&data).await {
                    eprintln!("Write error: {}", e);
                    *closed.lock().await = true;
                    break;
                }
            }
            // 尝试优雅关闭
            if let Err(e) = stream.shutdown().await {
                eprintln!("Stream shutdown error: {}", e);
            }
        });
    }

    /// 异步写入方法
    pub async fn write_async(&self, data: &[u8]) -> Result<(), NeoError> {
        let closed = *self.closed.lock().await;
        if closed {
            return Err(NeoError::SessionClosed);
        }

        match self.tx.send(data.to_vec()).await {
            Ok(()) => Ok(()),
            Err(_) => {
                *self.closed.lock().await = true;
                Err(NeoError::Other("Send failed".to_string()))
            }
        }
    }

    async fn close(&self) {
        *self.closed.lock().await = true;
    }

    /// 异步读取缓冲区数据
    pub async fn read_async(&self) -> Result<Vec<u8>, NeoError> {
        let mut all_data = Vec::new();
        let closed = self.is_closed().await;

        // 尝试从通道接收所有可用数据
        let mut rx = self.rx_buffer.lock().await;
        while let Ok(data) = rx.try_recv() {
            all_data.extend(data);
        }

        // 如果没有数据且连接未关闭，尝试异步接收一个数据块
        if all_data.is_empty() && !closed {
            match timeout(Duration::from_millis(TIMEOUT_MS), rx.recv()).await {
                Ok(Some(data)) => {
                    all_data.extend(data);
                },
                Ok(None) => {
                    *self.closed.lock().await = true;
                    return Err(NeoError::SessionClosed);
                },
                Err(_) => {}
            }
        }

        if closed && all_data.is_empty() {
            return Err(NeoError::SessionClosed);
        }

        Ok(all_data)
    }

    async fn is_closed(&self) -> bool {
        *self.closed.lock().await
    }
}

// fn print_hashmap(map: &HashMap<i32, Vec<u8>>) {
//     println!("HashMap 内容：");
//     for (key, value) in map {
//         // 尝试作为字符串打印
//         let value_str = String::from_utf8_lossy(value);
//         println!("键: {}, 值: {}", key, value_str);
//     }
// }
// 处理HTTP请求

fn write_reponse(request: tiny_http::Request, content: Vec<u8>) {
    let response =
        tiny_http::Response::from_string(String::from_utf8_lossy(&content)).with_status_code(200);
    let _ = request.respond(response);
}

async fn handle_request(
    mut request: tiny_http::Request,
    codec: &Codec,
    sessions: Sessions,
) -> Result<(), NeoError> {
    let neoreg_hello = NEO_HELLO;
    let decoded_hello = codec.base64_decode(neoreg_hello).unwrap_or_default();

    let mut data = Vec::new();
    if let Err(_) = request.as_reader().read_to_end(&mut data) {
        write_reponse(request, decoded_hello.to_vec());
        return Ok(());
    }
    // 解码数据
    let out = match codec.base64_decode(&data) {
        Ok(out) if !out.is_empty() => out,
        _ => {
            write_reponse(request, decoded_hello.to_vec());
            return Ok(());
        }
    };

    let info = codec.blv_decode(&out);

    let mut rinfo = HashMap::new();

    // 辅助函数：设置失败响应
    fn set_failure_response(rinfo: &mut BlvMap, error_msg: impl Into<Vec<u8>>) {
        rinfo.insert(STATUS, b"FAIL".to_vec());
        rinfo.insert(ERROR, error_msg.into());
    }

    let cmd = info
        .get(&CMD)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    let mark = info
        .get(&MARK)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    // print_hashmap(&info);
    match cmd.as_str() {
        "CONNECT" => {
            let ip = info
                .get(&IP)
                .map(|v| String::from_utf8_lossy(v))
                .unwrap_or_default();
            let port_str = info
                .get(&PORT)
                .map(|v| String::from_utf8_lossy(v))
                .unwrap_or_default();
            let target_addr = format!("{}:{}", ip, port_str);

            match target_addr.parse::<SocketAddr>() {
                Ok(addr) => match TcpStream::connect_timeout(&addr, Duration::from_millis(CONNECTION_TIMEOUT_MS)) {
                    Ok(conn) => {
                        sessions.lock().await.insert(mark, Session::new(conn));
                        rinfo.insert(STATUS, b"OK".to_vec());
                    }
                    Err(e) => {
                        set_failure_response(&mut rinfo, e.to_string().into_bytes());
                    }
                },
                Err(e) => {
                    set_failure_response(&mut rinfo, format!("Invalid address: {}", e).into_bytes());
                }
            }
        }
        "FORWARD" => {
            let mut sessions = sessions.lock().await;
            if let Some(session) = sessions.get_mut(&mark) {
                if let Some(data) = info.get(&DATA) {
                    match session.write_async(data).await {
                        Ok(_) => {
                            rinfo.insert(STATUS, b"OK".to_vec());
                        }
                        Err(e) => {
                            set_failure_response(&mut rinfo, e.to_string().into_bytes());
                        }
                    }
                } else {
                        set_failure_response(&mut rinfo, b"No data provided".to_vec());
                    }
            } else {
                set_failure_response(&mut rinfo, b"Session not found".to_vec());
            }
        }
        "READ" => {
            // 首先检查会话是否存在
            let session_exists = { sessions.lock().await.contains_key(&mark) };
            
            if session_exists {
                // 获取会话的克隆引用
                let session = { sessions.lock().await.get(&mark).cloned() };
                if let Some(session) = session {
                    if session.is_closed().await {
                        set_failure_response(&mut rinfo, b"Session is closed".to_vec());
                    } else {
                        rinfo.insert(STATUS, b"OK".to_vec());
                        match session.read_async().await {
                            Ok(data) if !data.is_empty() => {
                                rinfo.insert(DATA, data);
                            }
                            Ok(_) => {
                                // Data is empty, do nothing
                            }
                            Err(e) => {
                                eprintln!("Failed to read data: {:?}", e);
                            }
                        }
                    }
                } else {
                    set_failure_response(&mut rinfo, b"Session not found".to_vec());
                }
            } else {
                set_failure_response(&mut rinfo, b"Session not found".to_vec());
            }
        }
        "DISCONNECT" => {
            let mut sessions = sessions.lock().await;
            if let Some(session) = sessions.remove(&mark) {
                session.close().await;
            }
            rinfo.insert(STATUS, b"OK".to_vec());
        }
        _ => {
            write_reponse(request, decoded_hello.to_vec());
            return Ok(());
        }
    }

    let data = codec.blv_encode(&rinfo);
    let encoded = codec.base64_encode(&data);
    write_reponse(request, encoded);
    Ok(())
}

// 主函数
#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <listen-address>", args[0]);
        std::process::exit(1);
    }

    let listen_addr = if args[1].contains(':') {
        args[1].clone()
    } else {
        format!("0.0.0.0:{}", args[1])
    };
    let server: Server = match Server::http(&listen_addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("服务器启动失败: {}", e);
            std::process::exit(1);
        }
    };
    
    // println!("服务器启动成功，监听地址: {}", &listen_addr);
    let codec = Codec::new();
    let sessions = Arc::new(Mutex::new(HashMap::new()));

    for request in server.incoming_requests() {
        let codec_clone = codec.clone();
        let sessions_clone = Arc::clone(&sessions);
        // println!("request: {:?}", request);
        tokio::spawn(async move {
            if let Err(e) = handle_request(request, &codec_clone, sessions_clone).await {
                eprintln!("请求处理错误: {}", e);
            }
        });
    }
}


// 测试模块
#[cfg(test)]
mod test;
