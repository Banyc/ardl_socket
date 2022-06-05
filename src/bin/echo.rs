use ardl_socket::{
    sockets::{ArdlListener, BindConfig},
    streams::ArdlStreamConfig,
};

const LISTEN_ADDR: &str = "0.0.0.0:38947";

#[tokio::main]
async fn main() {
    let bind_config = BindConfig::default();
    let listener = ArdlListener::bind(LISTEN_ADDR, bind_config).await.unwrap();

    loop {
        let config = ArdlStreamConfig::default();

        let (mut uploader, mut downloader, remote_addr, id) =
            listener.accept(config).await.unwrap();

        println!("[+] accepted {{ {}, {} }}", remote_addr, id);

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
                // println!(
                //     "{}, {:X?}",
                //     String::from_utf8_lossy(&slice.data()),
                //     slice.data()
                // );

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
