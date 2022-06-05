use ardl::utils::buf::{BufSlice, BufWtr, OwnedBufWtr};
use ardl_socket::{
    sockets::{self, ConnectConfig},
    streams::ArdlStreamDownloader,
};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::PathBuf,
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

const LISTEN_ADDR: &str = "0.0.0.0:38947";
const SOURCE_FILE_NAME: &str = "Free_Test_Data_10MB_MP4.upload.mp4";
const DESTINATION_FILE_NAME: &str = "Free_Test_Data_10MB_MP4.download.mp4";

#[tokio::main]
async fn main() {
    // file
    let source = PathBuf::from_str(SOURCE_FILE_NAME).unwrap();
    let destination = PathBuf::from_str(DESTINATION_FILE_NAME).unwrap();
    let mut source = File::open(source).unwrap();
    let source_size = source.metadata().unwrap().len() as usize;
    match fs::remove_file(&destination) {
        Ok(_) => (),
        Err(e) => match e.kind() {
            io::ErrorKind::NotFound => (),
            _ => panic!(),
        },
    }
    let destination = File::create(destination).unwrap();

    // connection
    let config = ConnectConfig::default();
    let (mut uploader, downloader) = sockets::connect(LISTEN_ADDR, config).await.unwrap();

    println!("[+] connected");

    // receive to destination
    let receiving = tokio::spawn(async move {
        receiving(downloader, destination, source_size).await;
    });

    // send source
    let before_send = Instant::now();
    loop {
        let mut wtr = OwnedBufWtr::new(1024 * 64, 0);
        let len = source.read(wtr.back_free_space()).unwrap();
        if len == 0 {
            break;
        }
        wtr.grow_back(len).unwrap();

        let slice = BufSlice::from_wtr(wtr);

        match uploader.write(slice).await {
            Ok(_) => (),
            Err(_) => {
                println!("[-] remote UDP endpoint closed. Detected by `write`");
                break;
            }
        }
    }
    let send_duration = Instant::now().duration_since(before_send);
    println!(
        "main: done reading file. Speed: {:.2} mB/s",
        source_size as f64 / send_duration.as_secs_f64() / 1000.0 / 1000.0
    );
    receiving.await.unwrap();
    println!("main: done receiving file");

    // flush all acks
    thread::sleep(Duration::from_secs(1));
}

async fn receiving(
    mut downloader: ArdlStreamDownloader,
    destination: fs::File,
    source_size: usize,
) {
    let mut destination = Some(destination);
    let mut bytes_written_so_far = 0;
    let mut last_print = Instant::now();
    let print_duration = Duration::from_secs(1);
    let mut speed = 0.0;
    let mut last_recv = Instant::now();
    let mut last_bytes_written_so_far = 0;
    // let alpha = 1.0 / 8.0;
    let alpha = 1.0 / 1.0;
    loop {
        let slice = match downloader.read(1024).await {
            Ok(x) => x,
            Err(_) => {
                println!("[-] remote UDP endpoint closed. Detected by `read`");
                break;
            }
        };

        let mut file = destination.take().unwrap();
        file.write(slice.data()).unwrap();
        bytes_written_so_far += slice.data().len();

        if Instant::now().duration_since(last_print) > print_duration {
            let this_speed = (bytes_written_so_far - last_bytes_written_so_far) as f64
                / Instant::now().duration_since(last_recv).as_secs_f64();
            speed = (1.0 - alpha) * speed + alpha * this_speed;
            last_recv = Instant::now();
            last_bytes_written_so_far = bytes_written_so_far;

            println!(
                "Progress: {:.2}%. Speed: {:.2} kB/s",
                bytes_written_so_far as f64 / source_size as f64 * 100.0,
                speed / 1000.0,
            );
            last_print = Instant::now();
        }

        if bytes_written_so_far == source_size {
            drop(file);
            break;
        }
        destination = Some(file);
        assert!(bytes_written_so_far < source_size);
    }
}
