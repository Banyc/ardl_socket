use ardl::utils::buf::BufSlice;
use ardl_socket::sockets::{self, ConnectConfig};
use std::io;

const LISTEN_ADDR: &str = "0.0.0.0:38947";

#[tokio::main]
async fn main() {
    let config = ConnectConfig::default();
    let (mut uploader, mut downloader) = sockets::connect(LISTEN_ADDR, config).await.unwrap();

    println!("[+] connected");

    'outer: loop {
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
        match uploader.write(slice).await {
            Ok(_) => (),
            Err(_) => {
                println!("[-] remote UDP endpoint closed. Detected by `write`");
                break;
            }
        }

        let slice = match downloader.read(1024).await {
            Ok(x) => x,
            Err(_) => {
                println!("[-] remote UDP endpoint closed. Detected by `read`");
                break;
            }
        };
        println!(
            "{}, {:X?}",
            String::from_utf8_lossy(&slice.data()),
            slice.data()
        );
        // check if there is more to read
        loop {
            match downloader.try_read(1024).await {
                Ok(slice) => match slice {
                    Some(slice) => {
                        println!(
                            "{}, {:X?}",
                            String::from_utf8_lossy(&slice.data()),
                            slice.data()
                        );
                    }
                    None => break,
                },
                Err(_) => {
                    println!("[-] remote UDP endpoint closed. Detected by `read`");
                    break 'outer;
                }
            }
        }
    }
}
