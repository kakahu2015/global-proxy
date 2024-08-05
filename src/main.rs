use std::error::Error;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::io::{Read, Write};
use std::thread;
use std::sync::Arc;
use std::fmt;

#[derive(Debug)]
struct ProxyError(String);

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ProxyError {}

fn main() -> Result<(), Box<dyn Error>> {
    // 启动TCP代理
    thread::spawn(|| {
        if let Err(e) = run_tcp_proxy() {
            eprintln!("TCP proxy error: {}", e);
        }
    });

    // 启动UDP代理
    run_udp_proxy()?;

    Ok(())
}

fn run_tcp_proxy() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:3000")?;
    println!("TCP proxy listening on port 3000");

    for stream in listener.incoming() {
        let stream = stream?;
        
        thread::spawn(move || {
            if let Err(e) = handle_tcp_connection(stream) {
                eprintln!("Error handling TCP connection: {}", e);
            }
        });
    }

    Ok(())
}

fn handle_tcp_connection(mut client_stream: TcpStream) -> Result<(), ProxyError> {
    let mut socks5_stream = TcpStream::connect("127.0.0.1:1080")
        .map_err(|e| ProxyError(format!("Failed to connect to SOCKS5: {}", e)))?;

    let mut client_to_socks = vec![0; 1024];
    let mut socks_to_client = vec![0; 1024];

    let client_clone = client_stream.try_clone()
        .map_err(|e| ProxyError(format!("Failed to clone client stream: {}", e)))?;
    let socks5_clone = socks5_stream.try_clone()
        .map_err(|e| ProxyError(format!("Failed to clone SOCKS5 stream: {}", e)))?;

    let t1 = thread::spawn(move || {
        loop {
            match client_stream.read(&mut client_to_socks) {
                Ok(0) => break,
                Ok(n) => {
                    if socks5_stream.write_all(&client_to_socks[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let t2 = thread::spawn(move || {
        loop {
            match socks5_clone.read(&mut socks_to_client) {
                Ok(0) => break,
                Ok(n) => {
                    if client_clone.write_all(&socks_to_client[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    t1.join().map_err(|_| ProxyError("Thread 1 panicked".to_string()))?;
    t2.join().map_err(|_| ProxyError("Thread 2 panicked".to_string()))?;

    Ok(())
}

fn run_udp_proxy() -> Result<(), Box<dyn Error>> {
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:3001")?);
    println!("UDP proxy listening on port 3001");

    let socks5_addr = "127.0.0.1:1080";

    loop {
        let socket_clone = Arc::clone(&socket);
        let mut buf = [0u8; 65507];  // Max UDP packet size
        let (size, src_addr) = socket_clone.recv_from(&mut buf)?;

        thread::spawn(move || {
            if let Err(e) = handle_udp_packet(socket_clone, &buf[..size], src_addr, socks5_addr) {
                eprintln!("Error handling UDP packet: {}", e);
            }
        });
    }
}

fn handle_udp_packet(
    local_socket: Arc<UdpSocket>,
    data: &[u8],
    src_addr: std::net::SocketAddr,
    socks5_addr: &str,
) -> Result<(), Box<dyn Error>> {
    // 建立到 SOCKS5 服务器的 TCP 连接
    let mut socks5_tcp = TcpStream::connect(socks5_addr)?;

    // 发送 SOCKS5 握手
    socks5_tcp.write_all(&[0x05, 0x01, 0x00])?;
    let mut response = [0u8; 2];
    socks5_tcp.read_exact(&mut response)?;

    if response != [0x05, 0x00] {
        return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "SOCKS5 handshake failed")));
    }

    // 发送 UDP ASSOCIATE 请求
    let udp_request = [
        0x05, // SOCKS version
        0x03, // UDP ASSOCIATE command
        0x00, // Reserved
        0x01, // IPv4 address type
        0, 0, 0, 0, // IP address (0.0.0.0)
        0, 0, // Port (0, let the server decide)
    ];
    socks5_tcp.write_all(&udp_request)?;

    // 读取 SOCKS5 服务器的响应
    let mut response = [0u8; 10];
    socks5_tcp.read_exact(&mut response)?;

    if response[1] != 0x00 {
        return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "UDP ASSOCIATE failed")));
    }

    // 解析 SOCKS5 服务器提供的 UDP 中继地址和端口
    let relay_port = u16::from_be_bytes([response[8], response[9]]);
    let relay_addr = format!("{}.{}.{}.{}:{}", response[4], response[5], response[6], response[7], relay_port);

    // 创建 UDP socket 并发送数据
    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    
    // 构造 SOCKS5 UDP 请求头
    let mut socks_udp_header = vec![
        0, 0, 0, // Reserved
        1, // IPv4 address type
    ];
    socks_udp_header.extend_from_slice(&src_addr.ip().octets());
    socks_udp_header.extend_from_slice(&src_addr.port().to_be_bytes());
    socks_udp_header.extend_from_slice(data);

    udp_socket.send_to(&socks_udp_header, &relay_addr)?;

    // 接收响应并转发回客户端
    let mut response = [0u8; 65507];
    let (size, _) = udp_socket.recv_from(&mut response)?;

    // 跳过 SOCKS5 UDP 响应头
    let response_data = &response[10..size];
    local_socket.send_to(response_data, src_addr)?;

    Ok(())
}
