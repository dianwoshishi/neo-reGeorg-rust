use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use tokio::time::Duration;

    // 测试会话的基本功能: 创建、写入、读取和关闭
    #[tokio::test]
    async fn test_session_basic_functionality() {
        // 启动一个临时 TCP 服务器用于测试
        let listener = TcpListener::bind("127.0.0.1:0").expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get local addr");

        // 启动服务器接受连接
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept() {
                let mut stream = tokio::net::TcpStream::from_std(stream)
                    .expect("Failed to convert to async TcpStream");
                
                // 读取客户端发送的数据
                let mut buf = [0; 1024];
                if let Ok(n) = stream.read(&mut buf).await {
                    // 发送响应
                    stream.write_all(&buf[..n]).await.expect("Failed to write");
                }
            }
        });

        // 连接到测试服务器
        let stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("Failed to connect");
        let std_stream = stream
            .into_std()
            .expect("Failed to convert to std TcpStream");

        // 创建会话
        let session = Session::new(std_stream);

        // 测试写入数据
        let test_data = b"Hello, Session!";
        session
            .write_async(test_data)
            .await
            .expect("Failed to write");

        // 测试读取数据
        let timeout_duration = Duration::from_millis(100);
        let read_result = tokio::time::timeout(timeout_duration, session.read_async())
            .await
            .expect("Read timeout")
            .expect("Failed to read");

        // 验证读取的数据与发送的数据一致
        assert_eq!(read_result, test_data);

        // 测试关闭会话
        session.close().await;
        assert!(session.is_closed().await);

        // 测试会话关闭后写入失败
        let result = session.write_async(test_data).await;
        assert!(result.is_err());
        assert!(matches!(result, Err(NeoError::SessionClosed)));
    }

    // 测试会话超时
    #[tokio::test]
    async fn test_session_timeout() {
        // 创建一个 pair of connected sockets
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stream1 = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (stream2, _) = listener.accept().await.unwrap();
            

        let std_stream1 = stream1
            .into_std()
            .expect("Failed to convert to std TcpStream");

        // 创建会话
        let session = Session::new(std_stream1);

        // 确保会话未关闭
        assert!(!session.is_closed().await);

        // 测试空读取（应该超时但不会关闭会话）
        let timeout_duration = Duration::from_millis(50);
        let read_result = tokio::time::timeout(timeout_duration, session.read_async())
            .await
            .expect("Read timeout");

        // 验证读取结果为空但会话未关闭
        assert!(read_result.is_ok());
        assert!(read_result.unwrap().is_empty());
        assert!(!session.is_closed().await);
    }

    // 测试会话关闭后的数据读取
    #[tokio::test]
    async fn test_read_after_close() {
        // 创建一个 pair of connected sockets
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stream1 = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (_stream2, _) = listener.accept().await.unwrap();
            

        let std_stream1 = stream1
            .into_std()
            .expect("Failed to convert to std TcpStream");

        // 创建会话
        let session = Session::new(std_stream1);

        // 关闭会话
        session.close().await;
        assert!(session.is_closed().await);

        // 尝试读取数据
        let result = session.read_async().await;

        // 验证读取失败
        assert!(result.is_err());
        assert!(matches!(result, Err(NeoError::SessionClosed)));
    }
}
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

use crate::errors::NeoError;

const CHANNEL_CAPACITY: usize = 1024;
const BUFFER_SIZE: usize = 1024;
const TIMEOUT_MS: u64 = 10;

// 会话结构体
#[derive(Clone)]
pub struct Session {
    tx: mpsc::Sender<Vec<u8>>,
    rx_buffer: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
    closed: Arc<Mutex<bool>>,
}

impl Session {
    /// 创建一个新的会话实例
    ///
    /// 会启动两个异步任务：一个用于从流中读取数据并存储到缓冲区，
    /// 另一个用于从通道接收数据并写入到流中。
    pub fn new(stream: TcpStream) -> Self {
        // 克隆TcpStream，为两个异步任务提供独立实例
        let read_stream = stream
            .try_clone()
            .map_err(|e| NeoError::Io(e))
            .expect("Failed to clone stream");
        let write_stream = stream
            .try_clone()
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

        Session {
            tx: tx_write,
            rx_buffer,
            closed,
        }
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

    pub async fn close(&self) {
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
                }
                Ok(None) => {
                    *self.closed.lock().await = true;
                    return Err(NeoError::SessionClosed);
                }
                Err(_) => {}
            }
        }

        if closed && all_data.is_empty() {
            return Err(NeoError::SessionClosed);
        }

        Ok(all_data)
    }

    pub async fn is_closed(&self) -> bool {
        *self.closed.lock().await
    }
}
