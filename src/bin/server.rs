use ardl_socket::{listener::ArdlListener, stream::ArdlStreamConfig};

#[tokio::main]
async fn main() {
    let listener = ArdlListener::bind("localhost:38947").await.unwrap();

    loop {
        let config = ArdlStreamConfig::default();

        let (mut uploader, mut downloader, remote_addr) = listener.accept(config).await.unwrap();

        println!("[+] accepted {}", remote_addr);

        tokio::spawn(async move {
            loop {
                let slice = match downloader.read(1024).await {
                    Ok(x) => x,
                    Err(_) => {
                        println!(
                            "[-] remote UDP endpoint closed {}. Detected by `read`",
                            remote_addr
                        );
                        break;
                    }
                };
                println!(
                    "{}, {:X?}",
                    String::from_utf8_lossy(&slice.data()),
                    slice.data()
                );

                match uploader.write(slice).await {
                    Ok(x) => x,
                    Err(_) => {
                        println!(
                            "[-] remote UDP endpoint closed {}. Detected by `write`",
                            remote_addr
                        );
                        break;
                    }
                }
            }
        });
    }
}
