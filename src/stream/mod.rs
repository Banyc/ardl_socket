use std::{net::ToSocketAddrs, sync::Arc, time::Duration};

use ardl::{layer, utils::buf::OwnedBufWtr};
use tokio::{net::UdpSocket, sync::mpsc, task::JoinHandle};

use crate::utils::UdpEndpoint;

mod downloader;
pub use downloader::*;
mod uploader;
pub use uploader::*;

pub struct ArdlStreamBuilder {
    pub udp_endpoint: UdpEndpoint,
    pub flush_interval: std::time::Duration,
    pub mtu: usize,
    pub socket_recv_task: Option<JoinHandle<()>>,
    pub input_rx: mpsc::Receiver<OwnedBufWtr>,
    pub ardl_builder: layer::Builder,
}

impl ArdlStreamBuilder {
    pub fn build(self) -> (ArdlStreamUploader, ArdlStreamDownloader) {
        let (ardl_uploader, ardl_downloader) = self.ardl_builder.build().unwrap();
        let (set_state_tx, set_state_rx) = mpsc::channel(1);

        let stream_uploader = ArdlStreamUploaderBuilder {
            udp_endpoint: self.udp_endpoint,
            ardl_uploader,
            set_state_rx,
            flush_interval: self.flush_interval,
            mtu: self.mtu,
        }
        .build();
        let stream_downloader = ArdlStreamDownloaderBuilder {
            socket_recv_task: self.socket_recv_task,
            input_rx: self.input_rx,
            ardl_downloader,
            set_state_tx,
        }
        .build();
        (stream_uploader, stream_downloader)
    }
}

pub async fn connect(
    addr: impl ToSocketAddrs,
    config: ArdlStreamConfig,
) -> (ArdlStreamUploader, ArdlStreamDownloader) {
    let addrs: Vec<_> = addr.to_socket_addrs().unwrap().collect();

    let udp_listener = match addrs[0].ip() {
        std::net::IpAddr::V4(_) => UdpSocket::bind("0.0.0.0:0").await.unwrap(),
        std::net::IpAddr::V6(_) => UdpSocket::bind("[::]:0").await.unwrap(),
    };
    udp_listener.connect(addrs[0]).await.unwrap();
    let udp_connection = Arc::new(udp_listener);

    let (input_tx, input_rx) = mpsc::channel(1);
    let udp_connection1 = Arc::clone(&udp_connection);
    let mtu = config.mtu;
    let socket_recv_task = tokio::spawn(async move {
        loop {
            let mut buf = vec![0; mtu];
            let len = udp_connection1.recv(&mut buf).await.unwrap();
            let wtr = OwnedBufWtr::from_bytes(buf, 0, len);
            input_tx.send(wtr).await.unwrap();
        }
    });

    ArdlStreamBuilder {
        udp_endpoint: UdpEndpoint::Connection {
            connection: udp_connection,
        },
        flush_interval: config.flush_interval,
        mtu,
        socket_recv_task: Some(socket_recv_task),
        input_rx,
        ardl_builder: config.ardl_builder,
    }
    .build()
}

pub struct ArdlStreamConfig {
    pub mtu: usize,
    pub flush_interval: Duration,
    pub ardl_builder: layer::Builder,
}

impl ArdlStreamConfig {
    pub fn default() -> Self {
        ArdlStreamConfig {
            mtu: 1300,
            flush_interval: Duration::from_millis(10),
            ardl_builder: layer::Builder::default(),
        }
    }
}
