use base64::engine::{Engine as _, general_purpose};
use rand::{Rng, TryRngCore};
use std::collections::HashMap;
use std::io::{self};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc,Mutex};
use std::time::Duration;
use tiny_http::Server;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time;

// 常量定义
const DATA: i32 = 1;
const CMD: i32 = 2;
const MARK: i32 = 3;
const STATUS: i32 = 4;
const ERROR: i32 = 5;
const IP: i32 = 6;
const PORT: i32 = 7;
// const REDIRECTURL: i32 = 8;
// const FORCEREDIRECT: i32 = 9;

// 自定义Base64编码表
const EN: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const DE: &[u8] = b"dhULNVGsuAk/MxH6ibjcEfRqDWYznXBe9Pl7+SKoZ8pJaICgrQO0mF21yv345wtT";

// 全局会话存储
type Sessions = Arc<Mutex<HashMap<String, Session>>>;

// 会话结构体
struct Session {
    tx: mpsc::Sender<Vec<u8>>,
    buffer: Arc<Mutex<Vec<u8>>>,
    closed: Arc<Mutex<bool>>,
}

impl Session {
    fn new(stream: TcpStream) -> Self {
        // 克隆TcpStream，为两个异步任务提供独立实例
        let read_stream = stream.try_clone().expect("Failed to clone stream");
        let write_stream = stream.try_clone().expect("Failed to clone stream");

        // 明确指定通道传输类型为Vec<u8>
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(100);
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(false));

        // 读取线程（使用克隆的流）
        let buffer_clone = Arc::clone(&buffer);
        let closed_clone = Arc::clone(&closed);
        tokio::spawn(async move {
            // 使用tokio的异步IO trait，而非std的同步IO
            use tokio::io::AsyncReadExt;
            let mut stream = tokio::net::TcpStream::from_std(read_stream)
                .expect("Failed to convert to async TcpStream");
            let mut buf = [0; 513]; // 调整为512字节，更符合常见缓冲区大小

            while !*closed_clone.lock().unwrap() {
                match stream.read(&mut buf).await {
                    // 使用异步read
                    Ok(n) => {
                        // 1. 检查缓冲区大小，若超过限制则等待（不持有锁）
                        loop {
                            let current_len = { buffer_clone.lock().unwrap().len() };
                            if current_len < 524288 {
                                // 512KB上限
                                break;
                            }
                            time::sleep(Duration::from_millis(10)).await;

                            // 再次检查关闭状态，避免无限等待
                            if *closed_clone.lock().unwrap() {
                                return;
                            }
                        }

                        // 2. 写入数据（无await，安全持有锁）
                        let mut buffer = buffer_clone.lock().unwrap();
                        // println!("{:?}", buf.clone());
                        buffer.extend_from_slice(&buf[..n]);
                    }
                    Err(e) => {
                        // 读取错误，标记为关闭
                        eprintln!("Read error: {}", e);
                        *closed_clone.lock().unwrap() = true;
                        break;
                    }
                }
            }
            // 尝试优雅关闭写入端
            let _ = stream.shutdown().await;
        });

        // 写入线程（使用原始流）
        let closed_clone = Arc::clone(&closed);
        tokio::spawn(async move {
            // 使用tokio的异步IO trait
            use tokio::io::AsyncWriteExt;
            let mut stream = tokio::net::TcpStream::from_std(write_stream)
                .expect("Failed to convert to async TcpStream");

            while let Some(data) = rx.recv().await {
                // 双重检查关闭状态，减少锁竞争
                if *closed_clone.lock().unwrap() {
                    break;
                }

                // 使用异步write_all
                if let Err(e) = stream.write_all(&data).await {
                    eprintln!("Write error: {}", e);
                    *closed_clone.lock().unwrap() = true;
                    break;
                }
            }
            // 尝试优雅关闭写入端
            let _ = stream.shutdown().await;
        });

        Session { tx, buffer, closed}
    }

    // 异步写入方法（推荐使用）
    async fn write_async(&self, data: &[u8]) -> Result<(), io::Error> {
        let closed = *self.closed.lock().unwrap();
        if closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "connection closed",
            ));
        }

        self.tx.send(data.to_vec()).await.map_err(|_| {
            *self.closed.lock().unwrap() = true;
            io::Error::new(io::ErrorKind::BrokenPipe, "send failed")
        })
    }

    // // 同步写入方法（仅在必要时使用）
    // fn write(&self, data: &[u8]) -> Result<(), io::Error> {
    //     // 避免每次创建新的runtime，使用阻塞方式等待异步操作
    //     tokio::task::block_in_place(|| {
    //         tokio::runtime::Runtime::new()?.block_on(self.write_async(data))
    //     })
    // }

    fn close(&self) {
        *self.closed.lock().unwrap() = true;
    }

    fn read_buffer(&self) -> Vec<u8> {
        let mut buffer = self.buffer.lock().unwrap();
        let data = buffer.clone();
        buffer.clear();
        data
    }

    fn is_closed(&self) -> bool {
        *self.closed.lock().unwrap()
    }
}

// 构建编码映射表
fn build_maps() -> (HashMap<u8, u8>, HashMap<u8, u8>) {
    let mut en_map = HashMap::new();
    let mut de_map = HashMap::new();

    for i in 0..EN.len() {
        en_map.insert(EN[i], DE[i]);
    }

    for i in 0..DE.len() {
        de_map.insert(DE[i], EN[i]);
    }

    (en_map, de_map)
}

// 自定义Base64解码
fn base64_decode(data: &[u8], de_map: &HashMap<u8, u8>) -> Result<Vec<u8>, base64::DecodeError> {
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        out.push(de_map.get(&b).copied().unwrap_or(b));
    }
    general_purpose::STANDARD.decode(&out)
}

// 自定义Base64编码
fn base64_encode(rawdata: &[u8], en_map: &HashMap<u8, u8>) -> Vec<u8> {
    let encoded = general_purpose::STANDARD.encode(rawdata);
    let mut out = Vec::with_capacity(encoded.len());
    for b in encoded.bytes() {
        out.push(en_map.get(&b).copied().unwrap_or(b));
    }
    out
}

// BLV解码
fn blv_decode(data: &[u8]) -> HashMap<i32, Vec<u8>> {
    let mut info = HashMap::new();
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
        let l = i32::from_be_bytes(l_bytes) - 1966546385;
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

// 生成随机字节
fn rand_byte() -> Vec<u8> {
    let mut rng = rand::rng();
    let length = rng.random_range(5..20);
    let mut data = vec![0; length];
    _ = rng.try_fill_bytes(&mut data);
    data
}

// BLV编码
fn blv_encode(info: &HashMap<i32, Vec<u8>>) -> Vec<u8> {
    let mut data = Vec::new();
    let mut info = info.clone();

    info.insert(0, rand_byte());
    info.insert(39, rand_byte());

    for (&b, v) in &info {
        let l = v.len() as i32 + 1966546385;
        data.push(b as u8);
        data.extend_from_slice(&l.to_be_bytes());
        data.extend_from_slice(v);
    }

    data
}
fn print_hashmap(map: &HashMap<i32, Vec<u8>>) {
    println!("HashMap 内容：");
    for (key, value) in map {
        // 尝试作为字符串打印
        let value_str = String::from_utf8_lossy(value);
        println!("键: {}, 值: {}", key, value_str);
    }
}
// 处理HTTP请求
async fn handle_request(
    mut request: tiny_http::Request,
    en_map: &HashMap<u8, u8>,
    de_map: &HashMap<u8, u8>,
    sessions: Sessions,
) {
    let neoreg_hello = b"6UNI/jhLR7X7fqPmY+m0BofOMNXNbVV2XNbiEVEODRxUbshHWKXC/mQWx0SNYVDFx1bKY0VDjcS3RcS/nGIOzVA0XOdI/cy=";
    let decoded_hello = base64_decode(neoreg_hello, de_map).unwrap_or_default();

    // 读取请求体
    let mut data = Vec::new();
    if let Err(_) = request.as_reader().read_to_end(&mut data) {
        let response = tiny_http::Response::from_string(String::from_utf8_lossy(&decoded_hello))
            .with_status_code(200);
        let _ = request.respond(response);
        return;
    }

    // 解码数据
    let out = match base64_decode(&data, de_map) {
        Ok(out) if !out.is_empty() => out,
        _ => {
            let response =
                tiny_http::Response::from_string(String::from_utf8_lossy(&decoded_hello))
                    .with_status_code(200);
            let _ = request.respond(response);
            return;
        }
    };

    let info = blv_decode(&out);

    let mut rinfo = HashMap::new();

    let cmd = info
        .get(&CMD)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    let mark = info
        .get(&MARK)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    print_hashmap(&info);
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
                Ok(addr) => match TcpStream::connect_timeout(&addr, Duration::from_millis(3000)) {
                    Ok(conn) => {
                        sessions.lock().unwrap().insert(mark, Session::new(conn));
                        rinfo.insert(STATUS, b"OK".to_vec());
                    }
                    Err(e) => {
                        rinfo.insert(STATUS, b"FAIL".to_vec());
                        rinfo.insert(ERROR, e.to_string().into_bytes());
                    }
                },
                Err(e) => {
                    rinfo.insert(STATUS, b"FAIL".to_vec());
                    rinfo.insert(ERROR, format!("Invalid address: {}", e).into_bytes());
                }
            }
        }
        "FORWARD" => {
            let mut sessions = sessions.lock().unwrap();
            if let Some(session) = sessions.get_mut(&mark) {
                if let Some(data) = info.get(&DATA) {
                    match session.write_async(data).await {
                        Ok(_) => {
                            rinfo.insert(STATUS, b"OK".to_vec());
                        }
                        Err(e) => {
                            rinfo.insert(STATUS, b"FAIL".to_vec());
                            rinfo.insert(ERROR, e.to_string().into_bytes());
                        }
                    }
                } else {
                    rinfo.insert(STATUS, b"FAIL".to_vec());
                    rinfo.insert(ERROR, b"No data provided".to_vec());
                }
            } else {
                rinfo.insert(STATUS, b"FAIL".to_vec());
                rinfo.insert(ERROR, b"Session not found".to_vec());
            }
        }
        "READ" => {
            let sessions = sessions.lock().unwrap();
            if let Some(session) = sessions.get(&mark) {
                if session.is_closed() {
                    rinfo.insert(STATUS, b"FAIL".to_vec());
                    rinfo.insert(ERROR, b"Session is closed".to_vec());
                } else {
                    rinfo.insert(STATUS, b"OK".to_vec());
                    let data = session.read_buffer();
                    if !data.is_empty() {
                        rinfo.insert(DATA, data);
                    }
                }
            } else {
                rinfo.insert(STATUS, b"FAIL".to_vec());
                rinfo.insert(ERROR, b"Session not found".to_vec());
            }
        }
        "DISCONNECT" => {
            let mut sessions = sessions.lock().unwrap();
            if let Some(session) = sessions.remove(&mark) {
                session.close();
            }
            rinfo.insert(STATUS, b"OK".to_vec());
        }
        _ => {
            let response =
                tiny_http::Response::from_string(String::from_utf8_lossy(&decoded_hello))
                    .with_status_code(200);
            let _ = request.respond(response);
            return;
        }
    }

    let data = blv_encode(&rinfo);
    let encoded = base64_encode(&data, en_map);
    let response = tiny_http::Response::from_data(encoded).with_status_code(200);
    let _ = request.respond(response);
}

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
        format!(":{}", args[1])
    };

    let (en_map, de_map) = build_maps();
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));

    let server = match Server::http(&listen_addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to start server: {}", e);
            std::process::exit(1);
        }
    };

    println!("Server listening on http://{}", listen_addr);

    for request in server.incoming_requests() {
        let en_map = en_map.clone();
        let de_map = de_map.clone();
        let sessions = sessions.clone();
        handle_request(request, &en_map, &de_map, sessions).await;
    }
}


// todo: there ia a connect 