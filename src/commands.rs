use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tiny_http::Request;
use tokio::sync::Mutex;

use crate::NEO_HELLO;
use crate::codec::{BlvMap, Codec, MessageField};
use crate::errors::NeoError;
use crate::session::Session;

const CONNECTION_TIMEOUT_MS: u64 = 3000;

// 类型别名
pub type Sessions = Arc<Mutex<HashMap<String, Session>>>;

// 辅助函数：设置失败响应
pub fn set_failure_response(rinfo: &mut BlvMap, error_msg: impl Into<Vec<u8>>) {
    rinfo.insert(MessageField::Status.into(), b"FAIL".to_vec());
    rinfo.insert(MessageField::Error.into(), error_msg.into());
}

// 辅助函数：从info中获取字符串值
pub fn get_info_string_from_key(info: &BlvMap, field: MessageField) -> String {
    info.get(&field.into())
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default()
}

// 处理CONNECT命令
pub async fn handle_connect(info: &BlvMap, mark: &str, sessions: &Sessions, rinfo: &mut BlvMap) {
    let ip = get_info_string_from_key(info, MessageField::Ip);
    let port_str = get_info_string_from_key(info, MessageField::Port);
    let target_addr = format!("{}:{}", ip, port_str);

    match target_addr.parse::<SocketAddr>() {
        Ok(addr) => match std::net::TcpStream::connect_timeout(
            &addr,
            Duration::from_millis(CONNECTION_TIMEOUT_MS),
        ) {
            Ok(conn) => {
                sessions
                    .lock()
                    .await
                    .insert(mark.to_string(), Session::new(conn));
                rinfo.insert(MessageField::Status.into(), b"OK".to_vec());
            }
            Err(e) => {
                set_failure_response(rinfo, e.to_string().into_bytes());
            }
        },
        Err(e) => {
            set_failure_response(rinfo, format!("Invalid address: {}", e).into_bytes());
        }
    }
}

// 处理FORWARD命令
pub async fn handle_forward(info: &BlvMap, mark: &str, sessions: &Sessions, rinfo: &mut BlvMap) {
    let mut sessions = sessions.lock().await;
    if let Some(session) = sessions.get_mut(mark) {
        if let Some(data) = info.get(&MessageField::Data.into()) {
            match session.write_async(data).await {
                Ok(_) => {
                    rinfo.insert(MessageField::Status.into(), b"OK".to_vec());
                }
                Err(e) => {
                    set_failure_response(rinfo, e.to_string().into_bytes());
                }
            }
        } else {
            set_failure_response(rinfo, b"No data provided".to_vec());
        }
    } else {
        set_failure_response(rinfo, b"Session not found".to_vec());
    }
}

// 处理READ命令
pub async fn handle_read(mark: &str, sessions: &Sessions, rinfo: &mut BlvMap) {
    // 首先检查会话是否存在
    let session_exists = { sessions.lock().await.contains_key(mark) };

    if session_exists {
        // 获取会话的克隆引用
        let session = { sessions.lock().await.get(mark).cloned() };
        if let Some(session) = session {
            if session.is_closed().await {
                set_failure_response(rinfo, b"Session is closed".to_vec());
            } else {
                rinfo.insert(MessageField::Status.into(), b"OK".to_vec());
                match session.read_async().await {
                    Ok(data) => {
                        rinfo.insert(MessageField::Data.into(), data);
                    }
                    Err(e) => {
                        eprintln!("Failed to read data: {:?}", e);
                    }
                }
            }
        } else {
            set_failure_response(rinfo, b"Session not found".to_vec());
        }
    } else {
        set_failure_response(rinfo, b"Session not found".to_vec());
    }
}

// 处理DISCONNECT命令
pub async fn handle_disconnect(mark: &str, sessions: &Sessions, rinfo: &mut BlvMap) {
    let mut sessions = sessions.lock().await;
    if let Some(session) = sessions.remove(mark) {
        session.close().await;
    }
    rinfo.insert(MessageField::Status.into(), b"OK".to_vec());
}

// 主请求处理函数
pub async fn handle_request(
    mut request: Request,
    codec: &Codec,
    sessions: Sessions,
) -> Result<(), NeoError> {
    let decoded_hello = codec.base64_decode(NEO_HELLO).unwrap_or_default();

    // 读取并解码数据
    let out = {
        let mut data = Vec::new();
        if request.as_reader().read_to_end(&mut data).is_err() || data.is_empty() {
            write_reponse(request, decoded_hello.to_vec());
            return Ok(());
        }
        match codec.base64_decode(&data) {
            Ok(out) if !out.is_empty() => out,
            _ => {
                write_reponse(request, decoded_hello.to_vec());
                return Ok(());
            }
        }
    };

    let info = codec.blv_decode(&out);

    let mut rinfo = HashMap::new();

    // 提取命令和标记
    let cmd = get_info_string_from_key(&info, MessageField::Cmd);
    let mark = get_info_string_from_key(&info, MessageField::Mark);

    // 根据命令类型分发处理
    match cmd.as_str() {
        "CONNECT" => handle_connect(&info, &mark, &sessions, &mut rinfo).await,
        "FORWARD" => handle_forward(&info, &mark, &sessions, &mut rinfo).await,
        "READ" => handle_read(&mark, &sessions, &mut rinfo).await,
        "DISCONNECT" => handle_disconnect(&mark, &sessions, &mut rinfo).await,
        _ => {
            write_reponse(request, decoded_hello.to_vec());
            return Ok(());
        }
    }

    // 构建并发送响应
    let data = codec.blv_encode(&rinfo);
    let encoded = codec.base64_encode(&data);
    write_reponse(request, encoded);
    Ok(())
}

// 响应写入函数
pub fn write_reponse(request: Request, content: Vec<u8>) {
    let response =
        tiny_http::Response::from_string(String::from_utf8_lossy(&content)).with_status_code(200);
    let _ = request.respond(response);
}
