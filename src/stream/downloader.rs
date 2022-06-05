use std::sync::Arc;

use ardl::{
    layer::{Downloader, SetUploadState},
    utils::buf::BufSlice,
};
use tokio::{select, sync::mpsc, task::JoinHandle};

pub struct ArdlStreamDownloader {
    task: JoinHandle<()>,
    socket_recv_task: Option<JoinHandle<()>>,
    recv_req: bmrng::RequestSender<usize, Option<BufSlice>>,
    on_receive_available_rx: Arc<tokio::sync::Notify>,
}

impl ArdlStreamDownloader {
    pub fn check_rep(&self) {}

    pub async fn read(&mut self, len_cap: usize) -> Result<BufSlice, ReadError> {
        loop {
            match self.recv_req.send_receive(len_cap).await {
                Ok(x) => match x {
                    Some(x) => return Ok(x),
                    None => {
                        self.on_receive_available_rx.notified().await;
                    }
                },
                Err(_) => return Err(ReadError::RemoteClosedOrUploaderDropped),
            }
        }
    }

    pub async fn try_read(&mut self, len_cap: usize) -> Result<Option<BufSlice>, ReadError> {
        self.recv_req
            .send_receive(len_cap)
            .await
            .map_err(|_| ReadError::RemoteClosedOrUploaderDropped)
    }
}

impl Drop for ArdlStreamDownloader {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(task) = &self.socket_recv_task {
            task.abort();
        }
    }
}

pub struct ArdlStreamDownloaderBuilder {
    pub socket_recv_task: Option<JoinHandle<()>>,
    pub input_rx: mpsc::Receiver<BufSlice>,
    pub ardl_downloader: Downloader,
    pub set_state_tx: mpsc::Sender<SetUploadState>,
}

impl ArdlStreamDownloaderBuilder {
    pub fn build(self) -> ArdlStreamDownloader {
        let (recv_req, recv_res) = bmrng::channel(1);
        let on_receive_available = Arc::new(tokio::sync::Notify::new());
        let on_receive_available_tx = Arc::clone(&on_receive_available);

        let task = tokio::spawn(async move {
            downloading(
                self.input_rx,
                self.ardl_downloader,
                self.set_state_tx,
                on_receive_available_tx,
                recv_res,
            )
            .await;
        });

        let this = ArdlStreamDownloader {
            task,
            socket_recv_task: self.socket_recv_task,
            recv_req,
            on_receive_available_rx: on_receive_available,
        };
        this.check_rep();
        this
    }
}

async fn downloading(
    mut input_rx: mpsc::Receiver<BufSlice>,
    mut ardl_downloader: Downloader,
    set_state_tx: mpsc::Sender<SetUploadState>,
    on_receive_available_tx: Arc<tokio::sync::Notify>,
    mut recv_res: bmrng::RequestReceiver<usize, Option<BufSlice>>,
) {
    loop {
        select! {
            option = input_rx.recv() => {
                match option {
                    Some(slice) => {
                        let set_upload_state = match ardl_downloader.input_packet(slice) {
                            Ok(x) => x,
                            Err(_) => {
                                // Ignore decoding errors
                                continue;
                            }
                        };
                        match set_state_tx.send(set_upload_state).await {
                            Ok(_) => (),
                            Err(_) => {
                                // Uploader is dropped
                                break;
                            }
                        }
                        on_receive_available_tx.notify_one();
                    }
                    None => {
                        // The remote UDP endpoint is closed
                        break;
                    }
                }
            }
            result = recv_res.recv() => {
                match result {
                    Ok((len_cap, responser)) => {
                        let slice = ardl_downloader.recv_max(len_cap);
                        // assert: requester won't die so soon
                        responser.respond(slice).map_err(|_|()).unwrap();
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
    on_receive_available_tx.notify_one();
}

#[derive(Debug)]
pub enum ReadError {
    RemoteClosedOrUploaderDropped,
}
