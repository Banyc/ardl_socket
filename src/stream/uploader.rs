use std::{io, sync::Arc};

use ardl::{
    layer::{IObserver, SetUploadState, Uploader},
    protocol::{
        frag::{ACK_HDR_LEN, PUSH_HDR_LEN},
        packet_hdr::PACKET_HDR_LEN,
    },
    utils::buf::{BufSlice, BufWtr, OwnedBufWtr},
};
use tokio::{select, sync::mpsc, task::JoinHandle, time::Interval};

use crate::{
    protocol::{FrameBuilder, FRAME_HDR_LEN},
    utils::UdpEndpoint,
};

pub struct ArdlStreamUploader {
    task: JoinHandle<()>,
    to_send_req: bmrng::RequestSender<BufSlice, UploadingToSendResponse>,
    on_send_available_rx: Arc<tokio::sync::Notify>,
}

impl ArdlStreamUploader {
    pub fn check_rep(&self) {}

    pub async fn write(&mut self, slice: BufSlice) -> Result<(), WriteError> {
        let mut some_slice = Some(slice);
        loop {
            // assert: only enter the loop when there is something in `some_slice`
            let slice = some_slice.take().unwrap();
            match self.to_send_req.send_receive(slice).await {
                Ok(response) => match response {
                    UploadingToSendResponse::Ok => break,
                    UploadingToSendResponse::Err(slice) => {
                        some_slice = Some(slice);
                    }
                },
                Err(_) => return Err(WriteError::RemoteClosedOrDownloaderDropped),
            }
            self.on_send_available_rx.notified().await;
        }
        Ok(())
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
    pub id: u32,
}

impl ArdlStreamUploaderBuilder {
    pub fn build(mut self) -> Result<ArdlStreamUploader, BuildError> {
        if !(FRAME_HDR_LEN + PACKET_HDR_LEN + PUSH_HDR_LEN + 1 <= self.mtu) {
            return Err(BuildError::MtuTooSmall);
        }
        if !(FRAME_HDR_LEN + PACKET_HDR_LEN + ACK_HDR_LEN <= self.mtu) {
            return Err(BuildError::MtuTooSmall);
        }

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

        let on_send_available_tx = Arc::clone(&on_send_available);
        let task = tokio::spawn(async move {
            uploading(
                self.set_state_rx,
                self.ardl_uploader,
                self.udp_endpoint,
                self.mtu,
                tokio::time::interval(self.flush_interval),
                to_send_res,
                on_send_available_tx,
                self.id,
            )
            .await;
        });
        let uploader = ArdlStreamUploader {
            task,
            to_send_req,
            on_send_available_rx: on_send_available,
        };
        uploader.check_rep();
        Ok(uploader)
    }
}

async fn uploading(
    mut set_state_rx: mpsc::Receiver<SetUploadState>,
    mut ardl_uploader: Uploader,
    udp_endpoint: UdpEndpoint,
    mtu: usize,
    mut flush_interval: Interval,
    mut to_send_res: bmrng::RequestReceiver<BufSlice, UploadingToSendResponse>,
    on_send_available_tx: Arc<tokio::sync::Notify>,
    id: u32,
) {
    let mut wtr = OwnedBufWtr::new(mtu, 0);

    // Encode frame header
    let frame_hdr = FrameBuilder { id }.build();
    // Write frame header
    frame_hdr.append_to(&mut wtr).unwrap();

    loop {
        select! {
            option = set_state_rx.recv() => {
                match option {
                    Some(state) => {
                        // assert: state produced by downloader has to be valid
                        ardl_uploader.set_state(state).unwrap();
                        match output_all(&mut ardl_uploader, &udp_endpoint, &mut wtr).await {
                            Ok(_) => (),
                            Err(e) => match e {
                                OutputError::BufferTooSmall => panic!(),
                                OutputError::SendBufferNotEnough => (),
                                OutputError::OtherIoError(_) => {
                                    // Remote UDP endpoint is closed
                                    break;
                                }
                            }
                        }
                    }
                    None => {
                        // Downloader is dropped
                        break;
                    }
                }
            }
            _now = flush_interval.tick() => {
                match output_all(&mut ardl_uploader, &udp_endpoint, &mut wtr).await {
                    Ok(_) => (),
                    Err(e) => match e {
                        OutputError::BufferTooSmall => panic!(),
                        OutputError::SendBufferNotEnough => (),
                        OutputError::OtherIoError(_) => {
                            // Remote UDP endpoint is closed
                            break;
                        }
                    }
                }
            }
            result = to_send_res.recv() => {
                match result {
                    Ok((slice, responser)) => {
                        match ardl_uploader.to_send(slice) {
                            Ok(_) => {
                                // assert: `to_send_req` won't die so soon
                                responser.respond(UploadingToSendResponse::Ok)
                                .map_err(|_|())
                                .unwrap();
                                match output_all(&mut ardl_uploader, &udp_endpoint, &mut wtr).await {
                                    Ok(_) => (),
                                    Err(e) => match e {
                                        OutputError::BufferTooSmall => panic!(),
                                        OutputError::SendBufferNotEnough => (),
                                        OutputError::OtherIoError(_) => {
                                            // Remote UDP endpoint is closed
                                            break;
                                        }
                                    }
                                }
                            }
                            // assert: `to_send_req` won't die so soon
                            Err(e) => responser
                                .respond(UploadingToSendResponse::Err(e.0))
                                .map_err(|_|())
                                .unwrap(),
                        }
                    }
                    Err(_) => {
                        // Uploader is dropped
                        break;
                    }
                }
            }
        }
    }
    // Trigger errors on the other sides of those channels
    on_send_available_tx.notify_one();
}

#[derive(Debug)]
enum OutputError {
    BufferTooSmall,
    SendBufferNotEnough,
    OtherIoError(io::Error),
}

#[derive(Debug)]
pub enum WriteError {
    RemoteClosedOrDownloaderDropped,
}

#[derive(Debug)]
pub enum BuildError {
    MtuTooSmall,
}

enum UploadingToSendResponse {
    Ok,
    Err(BufSlice),
}

async fn output_all(
    uploader: &mut Uploader,
    udp_endpoint: &UdpEndpoint,
    wtr: &mut impl BufWtr,
) -> Result<(), OutputError> {
    let data_len_then = wtr.data_len();
    loop {
        match uploader.output_packet(wtr) {
            Ok(_) => {
                match udp_endpoint {
                    UdpEndpoint::Listener {
                        listener,
                        remote_addr,
                    } => match listener.send_to(wtr.data(), remote_addr).await {
                        Ok(_) => (),
                        Err(e) => match e.kind() {
                            io::ErrorKind::OutOfMemory => {
                                // TODO: make sure the right error kind
                                return Err(OutputError::SendBufferNotEnough);
                            }
                            _ => return Err(OutputError::OtherIoError(e)),
                        },
                    },
                    UdpEndpoint::Connection { connection } => {
                        match connection.send(wtr.data()).await {
                            Ok(_) => (),
                            Err(e) => match e.kind() {
                                io::ErrorKind::OutOfMemory => {
                                    // TODO: make sure the right error kind
                                    return Err(OutputError::SendBufferNotEnough);
                                }
                                _ => return Err(OutputError::OtherIoError(e)),
                            },
                        }
                    }
                }

                // Reset wtr
                wtr.shrink_back(wtr.data_len() - data_len_then).unwrap();
            }
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
