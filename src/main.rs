use std::collections::HashMap;
use std::sync::Arc;
use tiny_http::Server;
use tokio::sync::Mutex;

mod codec;
mod commands;
mod errors;
mod session;
use crate::codec::Codec;
use crate::commands::handle_request;

// 自定义Base64编码表
const EN: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const DE: &[u8] = b"dhULNVGsuAk/MxH6ibjcEfRqDWYznXBe9Pl7+SKoZ8pJaICgrQO0mF21yv345wtT";
const BLV_OFFSET: i32 = 1966546385;
const NEO_HELLO: &[u8] = b"6UNI/jhLR7X7fqPmY+m0BofOMNXNbVV2XNbiEVEODRxUbshHWKXC/mQWx0SNYVDFx1bKY0VDjcS3RcS/nGIOzVA0XOdI/cy=";

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
