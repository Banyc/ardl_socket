use std::{net::SocketAddr, sync::Arc};
use tokio::net::UdpSocket;

pub enum UdpEndpoint {
    Listener {
        listener: Arc<UdpSocket>,
        remote_addr: SocketAddr,
    },
    Connection {
        connection: Arc<UdpSocket>,
    },
}
