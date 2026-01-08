pub fn ephemeral_addr() -> String {
    let sock = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    format!("{}:{}", addr.ip(), addr.port())
}
