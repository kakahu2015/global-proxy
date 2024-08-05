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

    let (mut client_read, mut client_write) = client_stream.split()
        .map_err(|e| ProxyError(format!("Failed to split client stream: {}", e)))?;
    let (mut socks_read, mut socks_write) = socks5_stream.split()
        .map_err(|e| ProxyError(format!("Failed to split SOCKS5 stream: {}", e)))?;

    let t1 = thread::spawn(move || {
        std::io::copy(&mut client_read, &mut socks_write)
            .map_err(|e| ProxyError(format!("Error forwarding client to SOCKS5 (TCP): {}", e)))
    });

    let t2 = thread::spawn(move || {
        std::io::copy(&mut socks_read, &mut client_write)
            .map_err(|e| ProxyError(format!("Error forwarding SOCKS5 to client (TCP): {}", e)))
    });

    t1.join().expect("Thread 1 panicked")??;
    t2.join().expect("Thread 2 panicked")??;

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
    let socks5_socket = UdpSocket::bind("0.0.0.0:0")?;
    socks5_socket.connect(socks5_addr)?;

    // 这里应该实现SOCKS5 UDP Associate命令
    // 简化起见，我们直接发送数据
    socks5_socket.send(data)?;

    let mut response = [0u8; 65507];
    let size = socks5_socket.recv(&mut response)?;

    local_socket.send_to(&response[..size], src_addr)?;

    Ok(())
}
