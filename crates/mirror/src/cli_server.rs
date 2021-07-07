
use std::{io,net::SocketAddr,path::PathBuf};
use tokio::net::{TcpStream,TcpListener};

use common::{
    TcpSender, TcpReceiver,
    cpm_api::{Request,Response,Overlapped},
};

pub async fn handle_connection(stream: TcpStream, cache_path: PathBuf) -> io::Result<()>
{
    let (rx_stream, tx_stream) = stream.into_split();

    let mut rx_stream = TcpReceiver::<Overlapped<Request>>::from(rx_stream);
    let mut tx_stream = TcpSender::<Overlapped<Response>>::from(tx_stream);

    while let Some(Overlapped::<Request>{sequence, payload: request}) = rx_stream.next().await? {
        match request {
            Request::CheckMissing(mut packages) => {

                packages.retain(|id| {

                    let mut cache_path = PathBuf::from(&cache_path);

                    cache_path.push(&id.name);
                    cache_path.push(&id.version);

                    !cache_path.exists()

                });

                tx_stream.send(&Overlapped{sequence, payload:Response::CheckMissing(packages)}).await?;

            }
        }
    }

    Ok(())
}

pub async fn service(local_end_point: SocketAddr, cache_path: PathBuf) -> io::Result<()> {
    let listener = TcpListener::bind(local_end_point).await?;
    loop {
        let (stream, from) = listener.accept().await?;
        let cache_path = cache_path.clone();
        tracing::debug!("accepted cpm api connection from: {}", from);
        tokio::spawn(async move {
            match handle_connection(stream, cache_path).await {
                Ok(_) => tracing::debug!("cpm api connection from {} shutdown gracefully", from),
                Err(err) => tracing::error!("cpm api connection from {} terminated with: {}", from, err),
            }
        });
    }
}
