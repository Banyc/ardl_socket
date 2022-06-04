use std::sync::Arc;

use ardl::{
    layer::{IObserver, SetUploadState, Uploader},
    utils::buf::{BufSlice, BufWtr, OwnedBufWtr},
};
use tokio::{select, sync::mpsc, task::JoinHandle, time::Interval};

use crate::utils::UdpEndpoint;

pub struct ArdlStreamUploader {
    task: JoinHandle<()>,
    to_send_req: bmrng::RequestSender<BufSlice, UploadingToSendResponse>,
    on_send_available_rx: Arc<tokio::sync::Notify>,
}

impl ArdlStreamUploader {
    pub fn check_rep(&self) {}

    pub async fn write(&mut self, slice: BufSlice) {
        let mut some_slice = Some(slice);
        loop {
            let slice = some_slice.take().unwrap();
            match self.to_send_req.send_receive(slice).await {
                Ok(response) => match response {
                    UploadingToSendResponse::Ok => break,
                    UploadingToSendResponse::Err(slice) => {
                        some_slice = Some(slice);
                    }
                },
                Err(_) => panic!(),
            }
            self.on_send_available_rx.notified().await;
        }
    }
}

impl Drop for ArdlStreamUploader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct ArdlStreamUploaderBuilder {
    pub udp_endpoint: UdpEndpoint,
    pub ardl_uploader: Uploader,
    pub set_state_rx: mpsc::Receiver<SetUploadState>,
    pub flush_interval: std::time::Duration,
    pub mtu: usize,
}

impl ArdlStreamUploaderBuilder {
    pub fn build(mut self) -> ArdlStreamUploader {
        let (to_send_req, to_send_res) = bmrng::channel(1);

        let on_send_available = Arc::new(tokio::sync::Notify::new());
        let on_send_available_tx = Arc::clone(&on_send_available);
        let observer = OnSendAvailable {
            tx: on_send_available_tx,
        };
        let observer = Arc::new(observer);
        let weak_observer = Arc::downgrade(&observer);
        self.ardl_uploader
            .set_on_send_available(Some(weak_observer));

        let task = tokio::spawn(async move {
            uploading(
                self.set_state_rx,
                self.ardl_uploader,
                self.udp_endpoint,
                self.mtu,
                tokio::time::interval(self.flush_interval),
                to_send_res,
            )
            .await;
        });
        let uploader = ArdlStreamUploader {
            task,
            to_send_req,
            on_send_available_rx: on_send_available,
        };
        uploader.check_rep();
        uploader
    }
}

async fn uploading(
    mut set_state_rx: mpsc::Receiver<SetUploadState>,
    mut ardl_uploader: Uploader,
    udp_endpoint: UdpEndpoint,
    mtu: usize,
    mut flush_interval: Interval,
    mut to_send_res: bmrng::RequestReceiver<BufSlice, UploadingToSendResponse>,
) {
    loop {
        select! {
            Some(state) = set_state_rx.recv() => {
                ardl_uploader.set_state(state).unwrap();
                match output_all(&mut ardl_uploader, &udp_endpoint, mtu).await {
                    Ok(_) => (),
                    Err(e) => match e {
                        OutputError::BufferTooSmall => panic!(),
                        OutputError::SendBufferNotEnough => (),
                    }
                }
            }
            _now = flush_interval.tick() => {
                match output_all(&mut ardl_uploader, &udp_endpoint, mtu).await {
                    Ok(_) => (),
                    Err(e) => match e {
                        OutputError::BufferTooSmall => panic!(),
                        OutputError::SendBufferNotEnough => (),
                    }
                }
            }
            Ok((slice, responser)) = to_send_res.recv() => {
                match ardl_uploader.to_send(slice) {
                    Ok(_) => {
                        responser.respond(UploadingToSendResponse::Ok)
                            .map_err(|_|())
                            .unwrap();
                        match output_all(&mut ardl_uploader, &udp_endpoint, mtu).await {
                            Ok(_) => (),
                            Err(e) => match e {
                                OutputError::BufferTooSmall => panic!(),
                                OutputError::SendBufferNotEnough => (),
                            }
                        }
                    }
                    Err(e) => responser
                        .respond(UploadingToSendResponse::Err(e.0))
                        .map_err(|_|())
                        .unwrap(),
                }
            }
            else => panic!(),
        }
    }
}

#[derive(Debug)]
enum OutputError {
    BufferTooSmall,
    SendBufferNotEnough,
}

enum UploadingToSendResponse {
    Ok,
    Err(BufSlice),
}

async fn output_all(
    uploader: &mut Uploader,
    udp_endpoint: &UdpEndpoint,
    mtu: usize,
) -> Result<(), OutputError> {
    loop {
        let mut wtr = OwnedBufWtr::new(mtu, 0);
        match uploader.output_packet(&mut wtr) {
            Ok(_) => match udp_endpoint {
                UdpEndpoint::Listener {
                    listener,
                    remote_addr,
                } => match listener.send_to(wtr.data(), remote_addr).await {
                    Ok(_) => (),
                    Err(_e) => {
                        return Err(OutputError::SendBufferNotEnough);
                    }
                },
                UdpEndpoint::Connection { connection } => match connection.send(wtr.data()).await {
                    Ok(_) => (),
                    Err(_e) => {
                        return Err(OutputError::SendBufferNotEnough);
                    }
                },
            },
            Err(e) => match e {
                ardl::layer::OutputError::NothingToOutput => break,
                ardl::layer::OutputError::BufferTooSmall => {
                    return Err(OutputError::BufferTooSmall)
                }
            },
        }
    }
    Ok(())
}

struct OnSendAvailable {
    tx: Arc<tokio::sync::Notify>,
}

impl IObserver for OnSendAvailable {
    fn notify(&self) {
        let _result = self.tx.notify_one();
    }
}
