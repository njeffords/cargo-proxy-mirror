
use std::{io,io::Write,fs::File,net::SocketAddr,path::{Path,PathBuf}};
use tokio::net::{TcpStream,TcpListener};

use common::{
    TcpSender, TcpReceiver,
    cpm_api::{PackageId,Request,Response,Overlapped,SendMessage,RecvMessage},
};

/// checks the provided package list for missing entries in the cache
fn check_missing(cache_path: &Path, packages: &mut Vec<PackageId>) {
    packages.retain(|id| {
        let mut cache_path : PathBuf = cache_path.into();
        cache_path.push(&id.name);
        cache_path.push(&id.version);
        !cache_path.exists()
    });
}

/// place the provided package into the cache
async fn upload_crate(mut cache_path: PathBuf, package: PackageId, file_bytes: Vec<u8>) -> io::Result<()> {

    tracing::trace!("adding new crate version {:?}, {} bytes", package, file_bytes.len());

    cache_path.push(&package.name);

    if !cache_path.exists() {
        std::fs::create_dir(&cache_path)?;
    }

    cache_path.push(&package.version);

    if !cache_path.exists() {
        let mut file = File::create(cache_path)?;
        file.write_all(&file_bytes)?;

        tracing::info!("added new crate version {}, {} bytes", package, file_bytes.len());
    } else {
        tracing::warn!("ignoring attempted overwrite of {}", package);
    }

    Ok(())
}

/// process commands from an accepted TCP connection
pub async fn handle_connection(stream: TcpStream, cache_path: PathBuf) -> io::Result<()>
{
    let (rx_stream, tx_stream) = stream.into_split();

    let mut rx_stream = TcpReceiver::<SendMessage>::from(rx_stream);
    let mut tx_stream = TcpSender::<RecvMessage>::from(tx_stream);

    while let Some(Overlapped::<Request>{sequence, payload: request}) = rx_stream.next().await? {
        match request {

            Request::CheckMissing(mut packages) => {
                check_missing(&cache_path, &mut packages);
                tx_stream.send(&Overlapped{sequence, payload:Ok(Response::CheckMissing(packages))}).await?;
            },

            Request::UploadCrate{package,content} => {
                upload_crate(cache_path.clone(), package, content).await?;
                tx_stream.send(&Overlapped{sequence, payload:Ok(Response::UploadCrate)}).await?;
            }

            //_ => {
            //    tx_stream.send(&Overlapped{sequence, payload:Err(cpm_api::Error::NotImplemented)}).await?;
            //},
        }
    }

    Ok(())
}

/// listen on a TCP port, handling connection via [handle_connection]
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
