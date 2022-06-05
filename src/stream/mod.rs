use std::{
    io,
    net::ToSocketAddrs,
    sync::Arc,
    time::{Duration, Instant},
};

use ardl::{
    layer,
    utils::buf::{BufSlice, OwnedBufWtr},
};
use tokio::{net::UdpSocket, select, sync::mpsc, task::JoinHandle, time::interval};

use crate::{protocol::FrameHdr, utils::UdpEndpoint};

mod downloader;
pub use downloader::*;
mod uploader;
pub use uploader::*;

pub struct ArdlStreamBuilder {
    pub udp_endpoint: UdpEndpoint,
    pub flush_interval: std::time::Duration,
    pub mtu: usize,
    pub socket_recv_task: Option<JoinHandle<()>>,
    pub input_rx: mpsc::Receiver<BufSlice>,
    pub ardl_builder: layer::Builder,
    pub id: u32,
}

impl ArdlStreamBuilder {
    pub fn build(self) -> Result<(ArdlStreamUploader, ArdlStreamDownloader), BuildError> {
        let (ardl_uploader, ardl_downloader) = self
            .ardl_builder
            .build()
            .map_err(|e| BuildError::ArdlError(e))?;
        let (set_state_tx, set_state_rx) = mpsc::channel(1);

        let stream_uploader = ArdlStreamUploaderBuilder {
            udp_endpoint: self.udp_endpoint,
            ardl_uploader,
            set_state_rx,
            flush_interval: self.flush_interval,
            mtu: self.mtu,
            id: self.id,
        }
        .build()
        .map_err(|e| BuildError::UploaderError(e))?;
        let stream_downloader = ArdlStreamDownloaderBuilder {
            socket_recv_task: self.socket_recv_task,
            input_rx: self.input_rx,
            ardl_downloader,
            set_state_tx,
        }
        .build();
        Ok((stream_uploader, stream_downloader))
    }
}

pub async fn connect(
    addr: impl ToSocketAddrs,
    config: ConnectConfig,
) -> Result<(ArdlStreamUploader, ArdlStreamDownloader), ConnectError> {
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| ConnectError::IoError(e))?
        .collect();

    let udp_listener = match addrs[0].ip() {
        // assert: the addr is legit
        std::net::IpAddr::V4(_) => UdpSocket::bind("0.0.0.0:0").await.unwrap(),
        // assert: the addr is legit
        std::net::IpAddr::V6(_) => UdpSocket::bind("[::]:0").await.unwrap(),
    };
    udp_listener
        .connect(addrs[0])
        .await
        .map_err(|e| ConnectError::IoError(e))?;
    let udp_connection = Arc::new(udp_listener);

    let (input_tx, input_rx) = mpsc::channel(1);
    let udp_connection1 = Arc::clone(&udp_connection);
    let mtu = config.stream.mtu;
    let id: u32 = rand::random();
    let socket_recv_task = tokio::spawn(async move {
        let mut last_recv = Instant::now();
        let mut clean_inactive_timeout_interval = interval(config.inactive_timeout);
        let mut buf = vec![0; mtu];
        loop {
            select! {
                _ = clean_inactive_timeout_interval.tick() => {
                    let now = Instant::now();
                    if !(now.duration_since(last_recv) < config.inactive_timeout) {
                        // Peer didn't send anything here for so long
                        // Disconnect
                        break;
                    }
                }
                result = udp_connection1.recv(&mut buf) => {
                    let len = match result {
                        Ok(x) => x,
                        Err(_) => {
                            // Remote UDP endpoint not reachable
                            // Drop `input_tx` to inform `input_rx`
                            break;
                        }
                    };
                    let wtr = OwnedBufWtr::from_bytes(buf, 0, len);
                    buf = vec![0; mtu];
                    let mut slice = BufSlice::from_wtr(wtr);

                    // Decode frame header
                    let frame_hdr = match FrameHdr::from_slice(&mut slice) {
                        Ok(x) => x,
                        Err(_) => continue,
                    };
                    // Allow only frame with valid ID
                    if frame_hdr.id() != id {
                        continue;
                    }

                    last_recv = Instant::now();

                    match input_tx.send(slice).await {
                        Ok(_) => (),
                        Err(_) => {
                            // `input_rx` is closed
                            // No need to keep receiving from the UDP socket
                            break;
                        }
                    }
                }
            }
        }
    });

    ArdlStreamBuilder {
        udp_endpoint: UdpEndpoint::Connection {
            connection: udp_connection,
        },
        flush_interval: config.stream.flush_interval,
        mtu,
        socket_recv_task: Some(socket_recv_task),
        input_rx,
        ardl_builder: config.stream.ardl_builder,
        id,
    }
    .build()
    .map_err(|e| ConnectError::BuildError(e))
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

#[derive(Debug)]
pub enum BuildError {
    ArdlError(layer::BuildError),
    UploaderError(uploader::BuildError),
}

#[derive(Debug)]
pub enum ConnectError {
    IoError(io::Error),
    BuildError(BuildError),
}

pub struct ConnectConfig {
    pub stream: ArdlStreamConfig,
    pub inactive_timeout: Duration,
}

impl ConnectConfig {
    pub fn default() -> Self {
        ConnectConfig {
            stream: ArdlStreamConfig::default(),
            inactive_timeout: Duration::from_secs(300),
        }
    }
}
