use crate::{
    protocol::FrameHdr,
    streams::{
        self, ArdlStreamBuilder, ArdlStreamConfig, ArdlStreamDownloader, ArdlStreamUploader,
    },
};
use ardl::utils::buf::{BufSlice, OwnedBufWtr};
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    select,
    sync::mpsc,
    task::JoinHandle,
    time::interval,
};

pub struct ArdlListener {
    task: JoinHandle<()>,
    udp_listener: Arc<UdpSocket>,
    on_available_accept: Arc<tokio::sync::Notify>,
    accept_req: bmrng::RequestSender<(), Option<StreamRx>>,
}

impl ArdlListener {
    pub fn check_rep(&self) {}

    pub async fn bind(addr: impl ToSocketAddrs, config: BindConfig) -> Result<Self, io::Error> {
        let udp_listener = Arc::new(UdpSocket::bind(addr).await?);
        let udp_listener_accept = Arc::clone(&udp_listener);

        let (accept_req, accept_res) = bmrng::channel(1);
        let on_available_accept = Arc::new(tokio::sync::Notify::new());
        let on_available_accept_tx = Arc::clone(&on_available_accept);
        let task = tokio::spawn(async move {
            listening(
                config.mtu,
                udp_listener_accept,
                config.accept_queue_len_cap,
                accept_res,
                on_available_accept_tx,
                config.inactive_timeout,
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
    ) -> Result<(ArdlStreamUploader, ArdlStreamDownloader, SocketAddr, u32), AcceptError> {
        let stream_rx = loop {
            match self.accept_req.send_receive(()).await {
                Ok(x) => match x {
                    Some(x) => break x,
                    None => {
                        self.on_available_accept.notified().await;
                    }
                },
                Err(_) => return Err(AcceptError::UdpError),
            }
        };
        let stream = ArdlStreamBuilder {
            udp_endpoint: crate::utils::UdpEndpoint::Listener {
                listener: Arc::clone(&self.udp_listener),
                remote_addr: stream_rx.remote_addr,
            },
            flush_interval: config.flush_interval,
            mtu: config.mtu,
            socket_recv_task: None,
            input_rx: stream_rx.input_rx,
            ardl_builder: config.ardl_builder,
            id: stream_rx.id,
        }
        .build()
        .map_err(|e| AcceptError::BuildError(e))?;
        Ok((stream.0, stream.1, stream_rx.remote_addr, stream_rx.id))
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
    mut accept_res: bmrng::RequestReceiver<(), Option<StreamRx>>,
    on_available_accept_tx: Arc<tokio::sync::Notify>,
    inactive_timeout: Duration,
) {
    let mut accepted_downloaders: HashMap<u32, StreamTx> = HashMap::new();
    let mut pending_downloaders: HashMap<u32, StreamTx> = HashMap::new();
    let mut accept_queue = VecDeque::with_capacity(accept_queue_len_cap);

    let mut bytes = vec![0; mtu];
    let mut clean_inactive_timeout_interval = interval(inactive_timeout);
    loop {
        select! {
            _ = clean_inactive_timeout_interval.tick() => {
                let now = Instant::now();
                let mut to_remove = Vec::new();
                for (&id, stream_tx) in &accepted_downloaders {
                    if !(now.duration_since(stream_tx.last_recv) < inactive_timeout) {
                        to_remove.push(id);
                    }
                }
                for id in to_remove {
                    accepted_downloaders.remove(&id);
                }
            }
            result = udp_listener.recv_from(&mut bytes) => {
                let (len, remote_addr) = match result {
                    Ok(x) => x,
                    Err(_) => {
                        // Listener is dropped or errors from `udp_listener`
                        break;
                    }
                };

                let wtr = OwnedBufWtr::from_bytes(bytes, 0, len);
                bytes = vec![0; mtu];
                let mut slice = BufSlice::from_wtr(wtr);

                // Decode frame header
                let frame_hdr = match FrameHdr::from_slice(&mut slice) {
                    Ok(x) => x,
                    Err(_) => continue,
                };

                if let Some(_) = pending_downloaders.get(&frame_hdr.id()) {
                    // The default local rwnd size is zero, no rwnd update until this ID is accepted and the first remote push is acknowledged
                    // Don't care until it is accepted
                    continue;
                }

                // Closure
                let mut try_enqueue_accept = |slice| {
                    if accept_queue.len() < accept_queue_len_cap {
                        let (input_tx, input_rx) = mpsc::channel(1);

                        // The first RTO will be so long that it is wiser to acknowledge this first push ASAP before the peer retransmits it much later
                        let _ = input_tx.try_send(slice);

                        let stream_rx = StreamRx {
                            remote_addr,
                            input_rx,
                            id: frame_hdr.id(),
                        };
                        let stream_tx = StreamTx {
                            remote_addr,
                            input_tx,
                            last_recv: Instant::now(),
                        };
                        pending_downloaders.insert(frame_hdr.id(), stream_tx);
                        accept_queue.push_back(stream_rx);
                        on_available_accept_tx.notify_one();
                    }
                };

                // Ensure created session for this ID and pass packet to the downloader
                match accepted_downloaders.get(&frame_hdr.id()) {
                    Some(stream_tx) => {
                        // This is an existing and accepted ID
                        match remote_addr == stream_tx.remote_addr {
                            true => {
                                match stream_tx.input_tx.try_send(slice) {
                                    Ok(_) => (),
                                    Err(e) => match e {
                                        // Drop overflowed packets
                                        mpsc::error::TrySendError::Full(_) => (),
                                        // Remove closed downloader
                                        mpsc::error::TrySendError::Closed(slice) => {
                                            accepted_downloaders.remove(&frame_hdr.id());
                                            try_enqueue_accept(slice);
                                        }
                                    }
                                }
                            }
                            false => {
                                // Might be an replay attack from the other source
                                // Ignore

                                // accepted_downloaders.remove(&frame_hdr.id());
                                // try_enqueue_accept(slice);
                            }
                        }
                    }
                    None => {
                        // Neither is this frame ID pending nor accepted
                        // Attempt to enqueue this ID
                        try_enqueue_accept(slice);
                    }
                }
            }
            result = accept_res.recv() => {
                let (_, responser) = match result {
                    Ok(x) => x,
                    Err(_) => {
                        // Listener is dropped
                        break;
                    }
                };

                match accept_queue.pop_front() {
                    Some(stream_rx) => {
                        // assert: because the relative items are each added to `accept_queue` and `pending_downloaders`, it must match
                        let stream_tx = pending_downloaders.remove(&stream_rx.id).unwrap();
                        accepted_downloaders.insert(stream_rx.id, stream_tx);
                        // assert:
                        // - `accept_req` is in `self`
                        // - `accept_res` is in `self.task` which aborts when `self` drops
                        responser.respond(Some(stream_rx)).unwrap();
                    }
                    None => {
                        // assert:
                        // - `accept_req` is in `self`
                        // - `accept_res` is in `self.task` which aborts when `self` drops
                        responser.respond(None).unwrap();
                    }
                };
            }
        }
    }
    // Trigger errors on the other sides of those channels
    on_available_accept_tx.notify_one();
}

#[derive(Debug)]
struct StreamRx {
    remote_addr: SocketAddr,
    input_rx: mpsc::Receiver<BufSlice>,
    id: u32,
}

#[derive(Debug)]
struct StreamTx {
    remote_addr: SocketAddr,
    input_tx: mpsc::Sender<BufSlice>,
    last_recv: Instant,
}

#[derive(Debug)]
pub enum AcceptError {
    BuildError(streams::BuildError),
    UdpError,
}

pub struct BindConfig {
    pub mtu: usize,
    pub inactive_timeout: Duration,
    pub accept_queue_len_cap: usize,
}

impl BindConfig {
    pub fn default() -> Self {
        BindConfig {
            mtu: 1300,
            inactive_timeout: Duration::from_secs(300),
            accept_queue_len_cap: 1280,
        }
    }
}
