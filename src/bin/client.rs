use std::io;

use ardl::utils::buf::BufSlice;
use ardl_socket::stream::{self, ArdlStreamConfig};

#[tokio::main]
async fn main() {
    let config = ArdlStreamConfig::default();

    let (mut uploader, mut downloader) = stream::connect("localhost:38947", config).await;

    loop {
        let mut text = String::new();
        io::stdin().read_line(&mut text).unwrap();
        if text.ends_with("\n") {
            text.truncate(text.len() - 1);
        }
        let bytes = text.into_bytes();
        if bytes.len() == 0 {
            continue;
        }
        let slice = BufSlice::from_bytes(bytes);
        uploader.write(slice).await;

        let slice = downloader.read(1024).await;
        println!(
            "{}, {:X?}",
            String::from_utf8_lossy(&slice.data()),
            slice.data()
        );
        while let Some(slice) = downloader.try_read(1024).await {
            println!(
                "{}, {:X?}",
                String::from_utf8_lossy(&slice.data()),
                slice.data()
            );
        }
    }
}
