use core::net::Ipv4Addr;

use embassy_net::{Stack, udp::{PacketMetadata, UdpSocket}};

/// Captive-portal DNS hijacker.
///
/// Listens on UDP port 53 and answers every A-record query with `gw_ip`.
/// For AAAA (IPv6) queries, returns NOERROR with no answers so the OS
/// falls back to an A query rather than treating the name as unresolvable.
pub async fn run_dns(stack: Stack<'static>, gw_ip: Ipv4Addr) {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 512];
    let mut tx_buf = [0u8; 512];

    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta, &mut rx_buf,
        &mut tx_meta, &mut tx_buf,
    );
    socket.bind(53).unwrap();
    log::info!("[DNS] Captive portal DNS hijacker listening on port 53");

    let mut buf = [0u8; 512];
    loop {
        let (len, src) = match socket.recv_from(&mut buf).await {
            Ok(x) => x,
            Err(e) => { log::error!("[DNS] recv error: {:?}", e); continue; }
        };

        let query = &buf[..len];

        // DNS header is 12 bytes.
        if query.len() < 12 { continue; }

        let txid    = &query[0..2];
        let flags   = u16::from_be_bytes([query[2], query[3]]);
        let qdcount = u16::from_be_bytes([query[4], query[5]]);

        // QR bit (15) = 0, Opcode bits (14-11) = 0
        if (flags & 0xF800) != 0 || qdcount == 0 { continue; }

        let qstart = 12_usize;
        let mut qpos = qstart;
        let mut name_buf = [0u8; 128];
        let mut name_len = 0;
        while qpos < query.len() {
            let label_len = query[qpos] as usize;
            qpos += 1;
            if label_len == 0 { break; }

            if name_len > 0 && name_len < name_buf.len() {
                name_buf[name_len] = b'.';
                name_len += 1;
            }
            if qpos + label_len > query.len() { break; }
            let copy_end = (name_len + label_len).min(name_buf.len());
            let to_copy = copy_end - name_len;
            name_buf[name_len..copy_end].copy_from_slice(&query[qpos..qpos + to_copy]);
            name_len = copy_end;

            qpos += label_len;
        }
        let domain = core::str::from_utf8(&name_buf[..name_len]).unwrap_or("?");

        let qend = qpos + 4; // +4 for QTYPE + QCLASS
        if qend > query.len() { continue; }

        let qtype = u16::from_be_bytes([query[qpos], query[qpos + 1]]);
        let type_name = match qtype { 1 => "A", 28 => "AAAA", 255 => "ANY", _ => "other" };

        let respond_with_a = qtype == 1 || qtype == 255; // A or ANY

        let ip = src.endpoint.addr;
        if respond_with_a {
            log::info!("[DNS] from={} query={} ({}) -> {}", ip, domain, type_name, gw_ip);
        } else {
            log::info!("[DNS] from={} query={} ({}) -> NOERROR", ip, domain, type_name);
        }

        let mut resp = [0u8; 512];
        let mut pos = 12_usize;

        // Header
        resp[0..2].copy_from_slice(txid);
        resp[2..4].copy_from_slice(&0x8400u16.to_be_bytes()); // QR+AA, RCODE=0
        resp[4..6].copy_from_slice(&1u16.to_be_bytes());      // QDCOUNT=1
        resp[6..8].copy_from_slice(&(respond_with_a as u16).to_be_bytes()); // ANCOUNT 0 or 1
        resp[8..10].copy_from_slice(&0u16.to_be_bytes());     // NSCOUNT=0
        resp[10..12].copy_from_slice(&0u16.to_be_bytes());    // ARCOUNT=0

        // Echo the question section verbatim
        let qsection = &query[qstart..qend];
        if pos + qsection.len() > resp.len() { continue; }
        resp[pos..pos + qsection.len()].copy_from_slice(qsection);
        pos += qsection.len();

        // Answer section — only for A / ANY queries
        if respond_with_a {
            if pos + 16 > resp.len() { continue; }
            resp[pos..pos+2].copy_from_slice(&0xC00Cu16.to_be_bytes()); pos += 2; // name ptr → offset 12
            resp[pos..pos+2].copy_from_slice(&1u16.to_be_bytes()); pos += 2;      // Type A
            resp[pos..pos+2].copy_from_slice(&1u16.to_be_bytes()); pos += 2;      // Class IN
            resp[pos..pos+4].copy_from_slice(&60u32.to_be_bytes()); pos += 4;     // TTL 60 s
            resp[pos..pos+2].copy_from_slice(&4u16.to_be_bytes()); pos += 2;      // RDLENGTH=4
            resp[pos..pos+4].copy_from_slice(&gw_ip.octets()); pos += 4;          // RDATA
        }

        if let Err(e) = socket.send_to(&resp[..pos], src).await {
            log::error!("[DNS] send error: {:?}", e);
        }
    }
}
