use core::net::{Ipv4Addr, SocketAddrV4};

use edge_dhcp::{
    io::{self, DEFAULT_SERVER_PORT},
    server::{Server, ServerOptions},
};
use edge_nal::UdpBind;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_net::Stack;
use embassy_time::{Duration, Timer};

/// DHCP server — assigns addresses to AP clients and advertises `gw_ip` as
/// both the default gateway and the DNS server (so queries reach our hijacker).
pub async fn run_dhcp(stack: Stack<'static>, gw_ip: Ipv4Addr) {
    let mut gw_buf = [gw_ip];
    let dns_buf = [gw_ip];
    let mut buf = [0u8; 1500];

    // ServerOptions is #[non_exhaustive]; set dns after construction so
    // clients actually receive a DNS server in their lease.
    let mut options = ServerOptions::new(gw_ip, Some(&mut gw_buf));
    options.dns = &dns_buf;

    log::info!("[DHCP] Starting server (gateway={}, dns={})", gw_ip, gw_ip);

    // Using a local static for buffers to avoid lifetime issues in the async loop
    static BUFFERS: static_cell::StaticCell<UdpBuffers<3, 1024, 1024, 10>> =
        static_cell::StaticCell::new();
    let buffers = BUFFERS.init(UdpBuffers::new());

    let unbound = Udp::new(stack, buffers);
    let mut bound = unbound
        .bind(core::net::SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            DEFAULT_SERVER_PORT,
        )))
        .await
        .unwrap();

    loop {
        _ = io::server::run(
            &mut Server::<_, 64>::new_with_et(gw_ip),
            &options,
            &mut bound,
            &mut buf,
        )
        .await
        .inspect_err(|e| log::error!("[DHCP] server error: {:?}", e));
        Timer::after(Duration::from_millis(500)).await;
    }
}
