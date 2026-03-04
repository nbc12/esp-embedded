use core::net::{Ipv4Addr, Ipv6Addr};

use edge_mdns::buf::VecBufAccess;
use edge_mdns::domain::base::Ttl;
use edge_mdns::io::{self, DEFAULT_SOCKET};
use edge_mdns::{HostAnswersMdnsHandler, host::Host};
use edge_nal::UdpSplit;
use edge_nal_embassy::{Udp, UdpBuffers};

use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

use esp_hal::rng::Rng;

/// Runs the mDNS responder for the given interface.
#[embassy_executor::task]
pub async fn run_mdns(stack: Stack<'static>, hostname: &'static str) {
    // Wait for interface to get an IP.
    stack.wait_config_up().await;
    let our_ip = stack.config_v4().unwrap().address.address();

    // Setup UDP factory and buffers
    static BUFFERS: static_cell::StaticCell<UdpBuffers<4, 512, 512, 10>> =
        static_cell::StaticCell::new();
    let buffers = BUFFERS.init(UdpBuffers::new());
    let udp_factory = Udp::new(stack, buffers);

    // Bind to the standard mDNS port
    let mut socket = io::bind(
        &udp_factory,
        DEFAULT_SOCKET,
        Some(Ipv4Addr::UNSPECIFIED),
        Some(0),
    )
    .await
    .unwrap();

    let (recv, send) = socket.split();

    let recv_buf = VecBufAccess::<CriticalSectionRawMutex, 512>::new();
    let send_buf = VecBufAccess::<CriticalSectionRawMutex, 512>::new();

    let host = Host {
        hostname,
        ipv4: our_ip,
        ipv6: Ipv6Addr::UNSPECIFIED,
        ttl: Ttl::from_secs(60),
    };

    // Notification signal (unused in this static example)
    let signal = Signal::<CriticalSectionRawMutex, _>::new();
    let mut rng = Rng::new();

    let mdns = io::Mdns::new(
        Some(Ipv4Addr::UNSPECIFIED),
        Some(0),
        recv,
        send,
        recv_buf,
        send_buf,
        &mut rng,
        &signal,
    );

    log::info!(
        "[mDNS] Responder started for {}.local at {}",
        hostname,
        our_ip
    );

    loop {
        // Run the responder
        if let Err(e) = mdns.run(HostAnswersMdnsHandler::new(&host)).await {
            log::error!("[mDNS] Error: {:?}", e);
        }
        embassy_time::Timer::after_millis(1000).await;
        log::info!("[mDNS] Looping...");
    }
}
