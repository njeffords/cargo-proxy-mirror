
use std::{
    //sync::Arc,
    str::FromStr,
    net::SocketAddr,
    convert::{TryFrom,TryInto},
};

use futures::{
    sink::SinkExt,
    channel::mpsc,
};

use hyper::{
    http,
    body::HttpBody,
    header::{HeaderName, CONTENT_TYPE,CONTENT_LENGTH},
};

use tokio::{
    pin,select,
    io, time::{sleep, Duration},
    net::TcpStream,
};

use thiserror::Error;
use displaydoc::Display;

use common::{TcpSender,TcpReceiver,up_stream,down_stream};

const TX_QUEUE_LENGTH: usize = 256;
const DOWN_LINK_RETRY_DELAY: Duration = Duration::from_millis(1000);

type HttpClient = hyper::client::Client<hyper_tls::HttpsConnector<hyper::client::connect::HttpConnector>>;

struct DownloadStream {
    session_id: u32,
    tx_channel: mpsc::Sender<down_stream::Message>,
}

impl DownloadStream {

    async fn send_message(&mut self, opcode: down_stream::Opcode) -> std::result::Result<(),mpsc::SendError> {
        self.tx_channel.send(down_stream::Message{ session_id: self.session_id, opcode }).await
    }

    async fn send_complete(&mut self) -> std::result::Result<(),mpsc::SendError> {
        use down_stream::{Opcode::Complete};
        self.send_message(Complete(Ok(()))).await
    }

    async fn send_failed(&mut self) -> std::result::Result<(),mpsc::SendError> {
        use down_stream::{Opcode::Complete,Error::Unspecified};
        self.send_message(Complete(Err(Unspecified))).await
    }
}

#[derive(Error,Display,Debug)]
enum DownloadError {
    /// HTTP error: {0}
    Hyper(#[from] hyper::Error),
    /// Downlink error: {0}
    Downlink(#[from] mpsc::SendError),
    /// The requested file is not available: {0}
    NotAvailable(hyper::StatusCode),
    /// Bad redirect
    BadRedirect,
    /// The required header '{0}' was invalid or missing
    BadOrMissingHeader(&'static hyper::header::HeaderName),
}

async fn do_download(mut response: hyper::Response<hyper::Body>, tx: &mut DownloadStream) -> Result<(),DownloadError> {

    use down_stream::Opcode::*;

    tracing::trace!("headers: {:?}", response.headers());

    fn get_header<T:FromStr>(response: &hyper::Response<hyper::Body>, name: &'static HeaderName) -> Result<T,DownloadError> {
        use DownloadError::BadOrMissingHeader;
        T::from_str(
            response.headers()
                .get(name).ok_or_else(||BadOrMissingHeader(name))?
                .to_str().map_err(|_|BadOrMissingHeader(name))?
        ).map_err(|_|BadOrMissingHeader(name))
    }

    let headers = down_stream::Headers {
        content_type: get_header(&response, &CONTENT_TYPE)?,
        content_length: get_header(&response, &CONTENT_LENGTH)?,
    };

    tx.send_message(Init(headers)).await?;

    while let Some(block) = response.data().await {

        let block = block?;

        tracing::trace!("block: {}", block.len());

        tx.send_message(Chunk(block.to_vec().into())).await?;
    }

    Ok(())

}

fn get_redirect_location(headers: &hyper::HeaderMap) -> std::result::Result<http::Uri,()> {
    headers
        .get("location")
        .ok_or_else(||())?
        .to_str()
        .map_err(|_|())?
        .try_into().map_err(|_|())
}


async fn download_file(client: HttpClient, mut uri: http::Uri, tx: &mut DownloadStream) -> Result<(),DownloadError> {

    let response = loop {

        let response = client.get(uri).await?;

        tracing::trace!("response: {:?}", response.status());

        if response.status().is_success() {
            break response;
        }

        if !response.status().is_redirection() {
            return Err(DownloadError::NotAvailable(response.status()));
        }

        uri = get_redirect_location(response.headers()).map_err(|_|DownloadError::BadRedirect)?;

        tracing::trace!("redirecting to: {:?}", uri);
    };

    do_download(response, tx).await
}

async fn rx_process(
    mut rx_end_point: TcpReceiver<up_stream::Request>,
    tx_channel: mpsc::Sender<down_stream::Message>,
    client: HttpClient,
) -> Result<(), io::Error> {
    while let Some(up_stream::Request{session_id,package,version}) = rx_end_point.next().await? {

        let tx_channel = tx_channel.clone();

        let server = std::env::var("CPM_CRATES_IO_BASE_URL").expect("a value for `CPM_CRATES_IO_BASE_URL`");

        let uri_str = format!("{}/{}/{}/download", server, package, version);
        tracing::info!("request for: {}", uri_str);
        let uri = http::Uri::try_from(&uri_str).expect(&format!("{} to be a valid URI", uri_str));
        let mut stream = DownloadStream{ session_id, tx_channel };

        let client = client.clone();
        tokio::spawn(async move {
            match download_file(client, uri, &mut stream).await {
                Ok(_) => {
                    tracing::info!("download of {}/{} completed", package, version);
                    if let Err(err) = stream.send_complete().await {
                        tracing::error!("unable to deliver completion: {}", err);
                    }
                },
                Err(err) => {
                    tracing::error!("download of {}/{} failed with: {}", package, version, err);
                    if let Err(err) = stream.send_failed().await {
                        tracing::error!("unable to deliver failure: {}", err);
                    }
                }
            }
        });
    }
    Ok(())
}

async fn run_connection(end_point_id: SocketAddr) -> Result<(), (bool,io::Error)> {

    let client = hyper::Client::builder().build::<_, hyper::Body>(hyper_tls::HttpsConnector::new());

    let (rx_end_point, tx_end_point) = TcpStream::connect(end_point_id).await.map_err(|e|(false,e))?.into_split();
    let (tx_channel, rx_channel) = mpsc::channel(TX_QUEUE_LENGTH);

    let rx_process_fut = rx_process(rx_end_point.into(), tx_channel, client);
    let tx_process_fut = TcpSender::mp_process(tx_end_point.into(), rx_channel);

    pin!{ rx_process_fut, tx_process_fut };

    tracing::info!("connection established to: {}", end_point_id);

    (select! {
        rx_e = rx_process_fut => rx_e,
        tx_e = tx_process_fut => tx_e,
    })
    .map_err(|e|(true,e))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let end_point = std::env::var("CPM_MIRROR_REMOTE_END_POINT").expect("value for `CPM_MIRROR_REMOTE_END_POINT`");
    let end_point = SocketAddr::from_str(&end_point).expect("a legal end point value for `CPM_MIRROR_REMOTE_END_POINT`");

    tracing::info!("attempting connection to: {}", end_point);

    let mut show_error = true;

    loop {
        match run_connection(end_point).await {
            Ok(_) => break,
            Err((did_connect, err)) => {
                if show_error || did_connect {
                    tracing::error!("failed connection to {} with: {}", end_point, err);
                    show_error = false;
                } else {
                    tracing::debug!("failed connection to {} with: {}", end_point, err);
                }
                sleep(DOWN_LINK_RETRY_DELAY).await;
            }
        }

        tracing::debug!("attempting connection to: {}", end_point);
    }
}
