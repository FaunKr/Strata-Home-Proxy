//! Wake on LAN magic packet.

use std::net::SocketAddr;

use tokio::net::UdpSocket;

/// Six `0xFF` bytes followed by the MAC address sixteen times.
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xffu8; 102];
    for chunk in packet[6..].as_chunks_mut::<6>().0 {
        *chunk = mac;
    }
    packet
}

pub async fn send(mac: [u8; 6], target: SocketAddr) -> std::io::Result<()> {
    let bind: SocketAddr = if target.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = UdpSocket::bind(bind).await?;
    socket.set_broadcast(true)?;
    socket.send_to(&magic_packet(mac), target).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_layout() {
        let mac = [1, 2, 3, 4, 5, 6];
        let packet = magic_packet(mac);
        assert_eq!(&packet[..6], &[0xff; 6]);
        for chunk in packet[6..].as_chunks::<6>().0 {
            assert_eq!(*chunk, mac);
        }
    }
}
