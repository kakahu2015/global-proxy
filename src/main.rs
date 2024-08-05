use std::error::Error;
use std::net::{TcpListener, TcpStream, UdpSocket, SocketAddr};
use std::io::{Read, Write};
use std::thread;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::fmt;
use std::time::Duration;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, default_value = "127.0.0.1")]
    listen_addr: String,
    #[clap(short, long, default_value = "3000")]
    tcp_port: u16,
    #[clap(short, long, default_value = "3001")]
    udp_port: u16,
    #[clap(short, long, default_value = "127.0.0.1")]
    socks5_addr: String,
    #[clap(short, long, default_value = "1080")]
    socks5_port: u16,
}

#[derive(Debug)]
struct ProxyError(String);

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ProxyError {}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        println!("Shutting down...");
        r.store(false, Ordering::SeqCst);
    })?;

    let tcp_args = args.clone();
    let tcp_handle = thread::spawn(move || {
        if let Err(e) = run_tcp_proxy(&tcp_args, running.clone()) {
            eprintln!("TCP proxy error: {}", e);
        }
    });

    let udp_args = args;
    let udp_handle = thread::spawn(move || {
        if let Err(e) = run_udp_proxy(&udp_args, running) {
            eprintln!("UDP proxy error: {}", e);
        }
    });

    tcp_handle.join().unwrap();
    udp_handle.join().unwrap();

    Ok(())
}

fn run_tcp_proxy(args: &Args, running: Arc<AtomicBool>) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(format!("{}:{}", args.listen_addr, args.tcp_port))?;
    println!("TCP proxy listening on {}:{}", args.listen_addr, args.tcp_port);

    listener.set_nonblocking(true)?;

    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let socks5_addr = format!("{}:{}", args.socks5_addr, args.socks5_port);
                thread::spawn(move || {
                    if let Err(e) = handle_tcp_connection(stream, &socks5_addr) {
                        eprintln!("Error handling TCP connection: {}", e);
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(Box::new(e)),
        }
    }

    Ok(())
}

fn handle_tcp_connection(mut client_stream: TcpStream, socks5_addr: &str) -> Result<(), ProxyError> {
    let mut socks5_stream = TcpStream::connect(socks5_addr)
        .map_err(|e| ProxyError(format!("Failed to connect to SOCKS5: {}", e)))?;

    perform_socks5_handshake(&mut socks5_stream)?;

    let mut client_clone = client_stream.try_clone()
        .map_err(|e| ProxyError(format!("Failed to clone client stream: {}", e)))?;
    let mut socks5_clone = socks5_stream.try_clone()
        .map_err(|e| ProxyError(format!("Failed to clone SOCKS5 stream: {}", e)))?;

    let t1 = thread::spawn(move || transfer_data(&mut client_stream, &mut socks5_stream));
    let t2 = thread::spawn(move || transfer_data(&mut socks5_clone, &mut client_clone));

    t1.join().map_err(|_| ProxyError("Thread 1 panicked".to_string()))?;
    t2.join().map_err(|_| ProxyError("Thread 2 panicked".to_string()))?;

    Ok(())
}

fn transfer_data(from: &mut impl Read, to: &mut impl Write) -> Result<(), ProxyError> {
    let mut buffer = [0; 8192];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                to.write_all(&buffer[..n])
                    .map_err(|e| ProxyError(format!("Failed to write data: {}", e)))?;
            }
            Err(e) => return Err(ProxyError(format!("Failed to read data: {}", e))),
        }
    }
    Ok(())
}

fn run_udp_proxy(args: &Args, running: Arc<AtomicBool>) -> Result<(), Box<dyn Error>> {
    let socket = Arc::new(UdpSocket::bind(format!("{}:{}", args.listen_addr, args.udp_port))?);
    println!("UDP proxy listening on {}:{}", args.listen_addr, args.udp_port);

    socket.set_nonblocking(true)?;

    while running.load(Ordering::SeqCst) {
        let mut buf = [0u8; 65507];
        match socket.recv_from(&mut buf) {
            Ok((size, src_addr)) => {
                let socket_clone = Arc::clone(&socket);
                let socks5_addr = format!("{}:{}", args.socks5_addr, args.socks5_port);
                thread::spawn(move || {
                    if let Err(e) = handle_udp_packet(socket_clone, &buf[..size], src_addr, &socks5_addr) {
                        eprintln!("Error handling UDP packet: {}", e);
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(Box::new(e)),
        }
    }

    Ok(())
}

fn handle_udp_packet(
    local_socket: Arc<UdpSocket>,
    data: &[u8],
    src_addr: SocketAddr,
    socks5_addr: &str,
) -> Result<(), Box<dyn Error>> {
    let mut socks5_tcp = TcpStream::connect(socks5_addr)?;

    perform_socks5_handshake(&mut socks5_tcp)?;

    let udp_request = [
        0x05, 0x03, 0x00, 0x01,
        0, 0, 0, 0,
        0, 0,
    ];
    socks5_tcp.write_all(&udp_request)?;

    let mut response = [0u8; 10];
    socks5_tcp.read_exact(&mut response)?;

    if response[1] != 0x00 {
        return Err(Box::new(ProxyError("UDP ASSOCIATE failed".to_string())));
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

fn perform_socks5_handshake(stream: &mut TcpStream) -> Result<(), ProxyError> {
    stream.write_all(&[0x05, 0x01, 0x00])
        .map_err(|e| ProxyError(format!("Failed to write SOCKS5 handshake: {}", e)))?;

    let mut response = [0u8; 2];
    stream.read_exact(&mut response)
        .map_err(|e| ProxyError(format!("Failed to read SOCKS5 handshake response: {}", e)))?;

    if response != [0x05, 0x00] {
        return Err(ProxyError("SOCKS5 handshake failed".to_string()));
    }

    Ok(())
}
