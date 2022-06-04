use ardl_socket::{listener::ArdlListener, stream::ArdlStreamConfig};

#[tokio::main]
async fn main() {
    let listener = ArdlListener::bind("localhost:38947").await.unwrap();

    loop {
        let config = ArdlStreamConfig::default();

        let (mut uploader, mut downloader, remote_addr) = listener.accept(config).await;

        println!("[+] accepted {}", remote_addr);

        tokio::spawn(async move {
            loop {
                let slice = downloader.read(1024).await;
                println!(
                    "{}, {:X?}",
                    String::from_utf8_lossy(&slice.data()),
                    slice.data()
                );

                uploader.write(slice).await;
            }
        });
    }
}
