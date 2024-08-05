fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        println!("Shutting down...");
        r.store(false, Ordering::SeqCst);
    })?;

    let tcp_args = args.clone();
    let tcp_running = running.clone();
    let tcp_handle = thread::spawn(move || {
        if let Err(e) = run_tcp_proxy(&tcp_args, tcp_running) {
            eprintln!("TCP proxy error: {}", e);
        }
    });

    let udp_args = args;
    let udp_running = running.clone();
    let udp_handle = thread::spawn(move || {
        if let Err(e) = run_udp_proxy(&udp_args, udp_running) {
            eprintln!("UDP proxy error: {}", e);
        }
    });

    tcp_handle.join().unwrap();
    udp_handle.join().unwrap();

    Ok(())
}
