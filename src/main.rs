use std::error::Error;
use std::net::{TcpListener, TcpStream, UdpSocket, SocketAddr};
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

    let mut client_clone = client_stream.try_clone()
        .map_err(|e| ProxyError(format!("Failed to clone client stream: {}", e)))?;
    let mut socks5_clone = socks5_stream.try_clone()
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
    src_addr: SocketAddr,
    socks5_addr: &str,
) -> Result<(), Box<dyn Error>> {
    let mut socks5_tcp = TcpStream::connect(socks5_addr)?;

    socks5_tcp.write_all(&[0x05, 0x01, 0x00])?;
    let mut response = [0u8; 2];
    socks5_tcp.read_exact(&mut response)?;

    if response != [0x05, 0x00] {
        return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "SOCKS5 handshake failed")));
    }

    let udp_request = [
        0x05, 0x03, 0x00, 0x01,
        0, 0, 0, 0,
        0, 0,
    ];
    socks5_tcp.write_all(&udp_request)?;

    let mut response = [0u8; 10];
    socks5_tcp.read_exact(&mut response)?;

    if response[1] != 0x00 {
        return Err(Box::new(std::io::Error::new(std::io::ErrorKind::Other, "UDP ASSOCIATE failed")));
    }

    let relay_port = u16::from_be_bytes([response[8], response[9]]);
    let relay_addr = format!("{}.{}.{}.{}:{}", response[4], response[5], response[6], response[7], relay_port);

    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    
    let mut socks_udp_header = vec![0, 0, 0, 1];
    socks_udp_header.extend_from_slice(&src_addr.ip().to_string().parse::<std::net::IpAddr>()?.to_string().as_bytes());
    socks_udp_header.extend_from_slice(&src_addr.port().to_be_bytes());
    socks_udp_header.extend_from_slice(data);

    udp_socket.send_to(&socks_udp_header, &relay_addr)?;

    let mut response = [0u8; 65507];
    let (size, _) = udp_socket.recv_from(&mut response)?;

    let response_data = &response[10..size];
    local_socket.send_to(response_data, src_addr)?;

    Ok(())
}
