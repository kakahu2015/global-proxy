use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use thiserror::Error;
use clap::Parser;
use serde::Deserialize;
use config::Config;

#[derive(Error, Debug)]
enum ProxyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("SOCKS5 handshake failed: {0}")]
    Socks5Handshake(String),
    
    #[error("UDP ASSOCIATE failed: {0}")]
    UdpAssociate(String),
    
    #[error("Config error: {0}")]
    ConfigError(#[from] config::ConfigError),

    #[error("Address parse error: {0}")]
    AddrParse(#[from] std::net::AddrParseError),
}

#[derive(Parser, Debug, Clone, Deserialize)]
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
    #[clap(long, default_value = "8192")]
    buffer_size: usize,
    #[clap(long, default_value = "60")]
    tcp_timeout: u64,
    #[clap(long, default_value = "30")]
    udp_timeout: u64,
}

impl From<config::Config> for Args {
    fn from(config: config::Config) -> Self {
        Args {
            listen_addr: config.get_string("listen_addr").unwrap_or_else(|_| "127.0.0.1".to_string()),
            tcp_port: config.get_int("tcp_port").unwrap_or(3000) as u16,
            udp_port: config.get_int("udp_port").unwrap_or(3001) as u16,
            socks5_addr: config.get_string("socks5_addr").unwrap_or_else(|_| "127.0.0.1".to_string()),
            socks5_port: config.get_int("socks5_port").unwrap_or(1080) as u16,
            buffer_size: config.get_int("buffer_size").unwrap_or(8192) as usize,
            tcp_timeout: config.get_int("tcp_timeout").unwrap_or(60) as u64,
            udp_timeout: config.get_int("udp_timeout").unwrap_or(30) as u64,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    
    let config = ConfigBuilder::new()
        .add_source(File::with_name("config").required(false))
        .add_source(Environment::with_prefix("PROXY"))
        .build()?;
    
    let config: Args = config.try_into()?;

    let (shutdown_sender, shutdown_receiver) = mpsc::channel::<()>(1);
    let shutdown_receiver = Arc::new(Mutex::new(shutdown_receiver));

    let tcp_handle = tokio::spawn(run_tcp_proxy(config.clone(), Arc::clone(&shutdown_receiver)));
    let udp_handle = tokio::spawn(run_udp_proxy(config, Arc::clone(&shutdown_receiver)));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            println!("Shutting down...");
            drop(shutdown_sender);
        }
    }

    tcp_handle.await??;
    udp_handle.await??;

    Ok(())
}

async fn run_tcp_proxy(config: Args, shutdown: Arc<Mutex<mpsc::Receiver<()>>>) -> Result<(), ProxyError> {
    let listener = TcpListener::bind(format!("{}:{}", config.listen_addr, config.tcp_port)).await?;
    println!("TCP proxy listening on {}:{}", config.listen_addr, config.tcp_port);

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, addr) = result?;
                println!("New TCP connection from {}", addr);
                let config = config.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_tcp_connection(stream, &config).await {
                        eprintln!("Error handling TCP connection: {}", e);
                    }
                });
            }
            _ = shutdown.lock().unwrap().recv() => {
                break;
            }
        }
    }

    println!("TCP proxy stopped");
    Ok(())
}

async fn run_udp_proxy(config: Args, shutdown: Arc<Mutex<mpsc::Receiver<()>>>) -> Result<(), ProxyError> {
    let socket = Arc::new(UdpSocket::bind(format!("{}:{}", config.listen_addr, config.udp_port)).await?);
    println!("UDP proxy listening on {}:{}", config.listen_addr, config.udp_port);

    let mut buf = vec![0u8; config.buffer_size];

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                let (size, src_addr) = result?;
                println!("Received UDP packet from {}", src_addr);
                let socket_clone = Arc::clone(&socket);
                let config = config.clone();
                let data = buf[..size].to_vec();
                tokio::spawn(async move {
                    if let Err(e) = handle_udp_packet(socket_clone, &data, src_addr, &config).await {
                        eprintln!("Error handling UDP packet: {}", e);
                    }
                });
            }
            _ = shutdown.lock().unwrap().recv() => {
                break;
            }
        }
    }

    println!("UDP proxy stopped");
    Ok(())
}

async fn handle_udp_packet(
    local_socket: Arc<UdpSocket>,
    data: &[u8],
    src_addr: SocketAddr,
    config: &Args,
) -> Result<(), ProxyError> {
    let mut socks5_tcp = TcpStream::connect(format!("{}:{}", config.socks5_addr, config.socks5_port)).await?;

    perform_socks5_handshake(&mut socks5_tcp).await?;

    let udp_request = [
        0x05, 0x03, 0x00, 0x01,
        0, 0, 0, 0,
        0, 0,
    ];
    socks5_tcp.write_all(&udp_request).await?;

    let mut response = [0u8; 10];
    socks5_tcp.read_exact(&mut response).await?;

    if response[1] != 0x00 {
        return Err(ProxyError::UdpAssociate("UDP ASSOCIATE failed".to_string()));
    }

    let relay_port = u16::from_be_bytes([response[8], response[9]]);
    let relay_addr = format!("{}.{}.{}.{}:{}", response[4], response[5], response[6], response[7], relay_port);

    let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
    
    let mut socks_udp_header = vec![0, 0, 0, 1];
    socks_udp_header.extend_from_slice(&src_addr.ip().to_string().parse::<std::net::IpAddr>()?.octets().into_iter().collect::<Vec<_>>());
    socks_udp_header.extend_from_slice(&src_addr.port().to_be_bytes());
    socks_udp_header.extend_from_slice(data);

    udp_socket.send_to(&socks_udp_header, &relay_addr).await?;

    let mut response = vec![0u8; config.buffer_size];
    let (size, _) = udp_socket.recv_from(&mut response).await?;

    let response_data = &response[10..size];
    local_socket.send_to(response_data, src_addr).await?;

    println!("UDP packet forwarded successfully");
    Ok(())
}

async fn perform_socks5_handshake(stream: &mut TcpStream) -> Result<(), ProxyError> {
    stream.write_all(&[0x05, 0x01, 0x00]).await?;

    let mut response = [0u8; 2];
    stream.read_exact(&mut response).await?;

    if response != [0x05, 0x00] {
        return Err(ProxyError::Socks5Handshake("Unexpected response".to_string()));
    }

    Ok(())
}




