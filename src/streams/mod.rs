mod downloader;
mod uploader;

use crate::utils::UdpEndpoint;
use ardl::{layer, utils::buf::BufSlice};
pub use downloader::*;
use std::time::Duration;
use tokio::{sync::mpsc, task::JoinHandle};
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
