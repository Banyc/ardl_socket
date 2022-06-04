use std::{
    collections::{HashMap, VecDeque},
    io,
    net::SocketAddr,
    sync::Arc,
};

use ardl::utils::buf::OwnedBufWtr;
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    select,
    sync::mpsc,
    task::JoinHandle,
};

use crate::stream::{
    ArdlStreamBuilder, ArdlStreamConfig, ArdlStreamDownloader, ArdlStreamUploader,
};

pub struct ArdlListener {
    task: JoinHandle<()>,
    udp_listener: Arc<UdpSocket>,
    on_available_accept: Arc<tokio::sync::Notify>,
    accept_req: bmrng::RequestSender<(), Option<DownloadPath>>,
}

impl ArdlListener {
    pub fn check_rep(&self) {}

    pub async fn bind(addr: impl ToSocketAddrs) -> Result<Self, io::Error> {
        let udp_listener = Arc::new(UdpSocket::bind(addr).await?);
        let udp_listener_accept = Arc::clone(&udp_listener);

        let mtu = 1300;
        let accept_queue_len_cap = 1024;
        // let (accept_tx, accept_rx) = mpsc::channel(accept_queue_len_cap);
        let (accept_req, accept_res) = bmrng::channel(1);
        let on_available_accept = Arc::new(tokio::sync::Notify::new());
        let on_available_accept_tx = Arc::clone(&on_available_accept);
        let task = tokio::spawn(async move {
            listening(
                mtu,
                udp_listener_accept,
                accept_queue_len_cap,
                accept_res,
                on_available_accept_tx,
            )
            .await;
        });

        let ardl_listener = ArdlListener {
            task,
            udp_listener,
            on_available_accept,
            accept_req,
        };
        Ok(ardl_listener)
    }

    pub async fn accept(
        &self,
        config: ArdlStreamConfig,
    ) -> (ArdlStreamUploader, ArdlStreamDownloader, SocketAddr) {
        let accept = loop {
            match self.accept_req.send_receive(()).await.unwrap() {
                Some(x) => break x,
                None => {
                    self.on_available_accept.notified().await;
                }
            }
        };
        let stream = ArdlStreamBuilder {
            udp_endpoint: crate::utils::UdpEndpoint::Listener {
                listener: Arc::clone(&self.udp_listener),
                remote_addr: accept.remote_addr,
            },
            flush_interval: config.flush_interval,
            mtu: config.mtu,
            socket_recv_task: None,
            input_rx: accept.input_rx,
            ardl_builder: config.ardl_builder,
        }
        .build();
        (stream.0, stream.1, accept.remote_addr)
    }
}

impl Drop for ArdlListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn listening(
    mtu: usize,
    udp_listener: Arc<UdpSocket>,
    accept_queue_len_cap: usize,
    mut accept_res: bmrng::RequestReceiver<(), Option<DownloadPath>>,
    on_available_accept_tx: Arc<tokio::sync::Notify>,
) {
    let mut accepted_downloaders: HashMap<SocketAddr, mpsc::Sender<OwnedBufWtr>> = HashMap::new();
    let mut pending_downloaders: HashMap<SocketAddr, mpsc::Sender<OwnedBufWtr>> = HashMap::new();
    let mut accept_queue = VecDeque::with_capacity(accept_queue_len_cap);

    let mut bytes = vec![0; mtu];
    loop {
        select! {
            Ok((len, remote_addr)) = udp_listener.recv_from(&mut bytes) => {
                let wtr = OwnedBufWtr::from_bytes(bytes, 0, len);
                bytes = vec![0; mtu];

                let mut try_accept = |wtr| {
                    if accept_queue.len() < accept_queue_len_cap {
                        let (input_tx, input_rx) = mpsc::channel(1);
                        let _ = input_tx.try_send(wtr);
                        let accept = DownloadPath {
                            remote_addr,
                            input_rx,
                        };
                        pending_downloaders.insert(remote_addr, input_tx);
                        accept_queue.push_back(accept);
                        on_available_accept_tx.notify_one();
                    }
                };

                match accepted_downloaders.get(&remote_addr) {
                    Some(input_tx) => {
                        match input_tx.try_send(wtr) {
                            Ok(_) => (),
                            Err(e) => match e {
                                // drop overflowed packets
                                mpsc::error::TrySendError::Full(_) => (),
                                // remove closed downloader
                                mpsc::error::TrySendError::Closed(wtr) => {
                                    accepted_downloaders.remove(&remote_addr);
                                    try_accept(wtr);
                                }
                            }
                        }
                    }
                    None => {
                        try_accept(wtr);
                    }
                }
            }
            Ok((_, responser)) = accept_res.recv() => {
                match accept_queue.pop_front() {
                    Some(accept) => {
                        let downloader = pending_downloaders.remove(&accept.remote_addr).unwrap();
                        accepted_downloaders.insert(accept.remote_addr, downloader);
                        responser.respond(Some(accept)).unwrap();
                    }
                    None => {
                        responser.respond(None).unwrap();
                    }
                };
            }
            else => panic!(),
        }
    }
}

#[derive(Debug)]
struct DownloadPath {
    remote_addr: SocketAddr,
    input_rx: mpsc::Receiver<OwnedBufWtr>,
}
