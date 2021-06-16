
use std::{
    str::FromStr,
    net::SocketAddr,
    sync::{Arc,Mutex},
    convert::{TryFrom,TryInto},
};

use futures::{
    sink::SinkExt,
    channel::mpsc,
    stream::StreamExt,
};

use hyper::{http, body::{Buf, HttpBody}};

use tokio::{
    pin,select,
    io, time::{sleep, Duration},
    net::TcpStream,
};

use thiserror::Error;
use displaydoc::Display;

use common::{TcpSender,TcpReceiver,up_stream,down_stream};

const TX_BUFFER_SIZE: usize = 64*1024;
const TX_QUEUE_LENGTH: usize = 256;
const DOWN_LINK_RETRY_DELAY: Duration = Duration::from_millis(1000);

type HttpClient = hyper::client::Client<hyper_tls::HttpsConnector<hyper::client::connect::HttpConnector>>;

#[derive(Default)]
struct BufferPool(Vec<Vec<u8>>);

impl BufferPool {
    fn borrow_buffer(&mut self) -> Vec<u8> {
        match self.0.pop() {
            None => Vec::with_capacity(TX_BUFFER_SIZE),
            Some(buffer) => buffer
        }
    }

    fn return_buffer(&mut self, buffer: Vec<u8>) {
        self.0.push(buffer)
    }
}

struct State{
    client: HttpClient,
    buffers: Mutex<BufferPool>,
}

impl Default for State {
    fn default() -> Self {
        let https = hyper_tls::HttpsConnector::new();
        let client = hyper::Client::builder().build::<_, hyper::Body>(https);
        Self { client, buffers: Default::default() }
    }
}

type StateRef = Arc<State>;

impl State {
    fn new() -> StateRef { Arc::new(Default::default()) }
}

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

async fn do_download(state: StateRef, mut response: hyper::Response<hyper::Body>, tx: &mut DownloadStream) -> Result<(),DownloadError> {

    use down_stream::Opcode::*;

    tracing::trace!("headers: {:?}", response.headers());

    fn get_header<T:std::str::FromStr>(response: &hyper::Response<hyper::Body>, name: &'static hyper::header::HeaderName) -> Result<T,DownloadError> {
        use DownloadError::BadOrMissingHeader;
        if let Some(value) = response.headers().get(name) {
            let value = value.to_str().map_err(|_|BadOrMissingHeader(name))?;
            let value = T::from_str(value).map_err(|_|BadOrMissingHeader(name))?;
            Ok(value)
        } else {
            Err(BadOrMissingHeader(name))
        }
    }

    let headers = down_stream::Headers {
        content_type: get_header(&response, &hyper::header::CONTENT_TYPE)?,
        content_length: get_header(&response, &hyper::header::CONTENT_LENGTH)?,
    };

    tx.send_message(Init(headers)).await?;

    while let Some(block) = response.data().await {

        let mut block = block?;

        tracing::trace!("block: {}", block.len());

        while block.has_remaining() {

            let mut buffer = state.buffers.lock().unwrap().borrow_buffer();

            tracing::trace!("buffer size: {}", buffer.capacity());

            buffer.resize(buffer.capacity ().min(block.len()), 0);

            block.copy_to_slice(&mut buffer);

            tx.send_message(Chunk(buffer.into())).await?;
        }
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


async fn download_file(state: StateRef, mut uri: http::Uri, tx: &mut DownloadStream) -> Result<(),DownloadError> {

    let response = loop {

        let response = state.client.get(uri).await?;

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

    do_download(state, response, tx).await
}

async fn tx_process(
    state: StateRef,
    mut tx_end_point: TcpSender<down_stream::Message>,
    mut rx_channel: mpsc::Receiver<down_stream::Message>
) -> Result<(), io::Error> {
    while let Some(event) = rx_channel.next().await {
        tx_end_point.send(&event).await?;
        if let down_stream::Message{session_id:_,opcode:down_stream::Opcode::Chunk(buffer)} = event {
            state.buffers.lock().unwrap().return_buffer(buffer.into());
        }
    }
    Ok(())
}

async fn rx_process(
    state: StateRef,
    mut rx_end_point: TcpReceiver<up_stream::Request>,
    tx_channel: mpsc::Sender<down_stream::Message>
) -> Result<(), io::Error> {
    while let Some(up_stream::Request{session_id,package,version}) = rx_end_point.next().await? {

        let tx_channel = tx_channel.clone();

        let server = std::env::var("CPM_CRATES_IO_BASE_URL").expect("a value for `CPM_CRATES_IO_BASE_URL`");

        let uri_str = format!("{}/{}/{}/download", server, package, version);
        tracing::info!("request for: {}", uri_str);
        let uri = http::Uri::try_from(&uri_str).expect(&format!("{} to be a valid URI", uri_str));
        let mut stream = DownloadStream{ session_id, tx_channel };

        let state = state.clone();
        tokio::spawn(async move {
            match download_file(state, uri, &mut stream).await {
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

async fn run_connection(end_point_id: SocketAddr) -> Result<(), io::Error> {

    let state = State::new();

    let (rx_end_point, tx_end_point) = TcpStream::connect(end_point_id).await?.into_split();
    let (tx_channel, rx_channel) = mpsc::channel(TX_QUEUE_LENGTH);

    let rx_process_fut = rx_process(state.clone(), rx_end_point.into(), tx_channel);
    let tx_process_fut = tx_process(state.clone(), tx_end_point.into(), rx_channel);

    pin!{ rx_process_fut, tx_process_fut };

    tracing::info!("connection established to: {}", end_point_id);

    select! {
        rx_e = rx_process_fut => rx_e,
        tx_e = tx_process_fut => tx_e,
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    loop {
        let end_point = std::env::var("CPM_MIRROR_REMOTE_END_POINT").expect("value for `CPM_MIRROR_REMOTE_END_POINT`");

        let end_point = SocketAddr::from_str(&end_point).expect("a legal end point value for `CPM_MIRROR_REMOTE_END_POINT`");

        tracing::info!("attempting connection to: {}", end_point);

        match run_connection(end_point).await {
            Ok(_) => break,
            Err(err) => {
                tracing::error!("connection failed: {}", err);
                sleep(DOWN_LINK_RETRY_DELAY).await;
            }
        }
    }
}
