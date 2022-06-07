///! # Rust Cargo crate proxy service


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
    sync::watch,
};

use thiserror::Error;
use displaydoc::Display;

use common::{TcpSender,TcpReceiver,up_stream,down_stream};

use serde::{Serialize,Deserialize};
use structopt::StructOpt;

#[derive(StructOpt,Serialize,Deserialize,Debug)]
struct ServiceConfig {
    /// The address and port of the mirror service.
    #[structopt(short, long, env = "CPM_MIRROR_REMOTE_END_POINT")]
    mirror_end_point: SocketAddr,

    /// The base URL of the crate server.
    #[structopt(short, long, default_value="https://crates.io/api/v1/crates", env = "CPM_CRATES_IO_BASE_URL")]
    crates_io_base_url: String,
}

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
    base_url: &str,
) -> Result<(), io::Error> {
    while let Some(up_stream::Request{session_id,package,version}) = rx_end_point.next().await? {

        let tx_channel = tx_channel.clone();

        let uri_str = format!("{}/{}/{}/download", base_url, package, version);
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

async fn run_connection(end_point_id: SocketAddr, base_url: &str, mut running: watch::Receiver<bool>) -> Result<(), (bool,io::Error)> {

    let client = hyper::Client::builder().build::<_, hyper::Body>(hyper_tls::HttpsConnector::new());

    let (rx_end_point, tx_end_point) = TcpStream::connect(end_point_id).await.map_err(|e|(false,e))?.into_split();
    let (tx_channel, rx_channel) = mpsc::channel(TX_QUEUE_LENGTH);

    let rx_process_fut = rx_process(rx_end_point.into(), tx_channel, client, base_url);
    let tx_process_fut = TcpSender::mp_process(tx_end_point.into(), rx_channel);
    let terminated_fut = async { while *running.borrow() { running.changed().await.unwrap(); } Ok(()) };

    pin!{ rx_process_fut, tx_process_fut, terminated_fut };

    tracing::info!("connection established to: {}", end_point_id);

    (select! {
        r = rx_process_fut => r,
        r = tx_process_fut => r,
        r = terminated_fut => r,
    })
    .map_err(|e|(true,e))
}

pub async fn run_for_a_while(end_point: SocketAddr, base_url: String, running: watch::Receiver<bool>) {
    tracing::info!("base crate URL is: {}", base_url);
    tracing::info!("attempting connection to: {}", end_point);

    let mut show_error = true;

    while *running.borrow() {
        match run_connection(end_point, &base_url, running.clone()).await {
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

fn run_forever(config: ServiceConfig) {
    tracing_subscriber::fmt::init();
    let (_set_running,running) = tokio::sync::watch::channel(true);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_for_a_while(config.mirror_end_point, config.crates_io_base_url, running))
}

#[cfg(windows)]
mod winsvc_glue {

    use tokio::sync::watch::Receiver;
    use winsvc::{
        std_cli::{Command,ServiceDetail,LoggingConfig},
        async_service_main::InitializationToken,
    };
    use super::{ServiceConfig,run_forever,run_for_a_while};

    struct Service;

    async fn service_main(
        config: ServiceConfig,
        init: InitializationToken,
        running: Receiver<bool>
    ) {
        init.complete();
        run_for_a_while(config.mirror_end_point, config.crates_io_base_url, running).await
    }

    impl ServiceDetail for Service {

        const SERVICE_IDENTIFIER: &'static str = "cpm-proxy";
        const SERVICE_DISPLAY_NAME: &'static str = "Rust Cargo Crate Proxy";

        type Config = ServiceConfig;

        fn run_local(config: Self::Config) {
            run_forever(config);
        }

        fn run_as_service(log_config: LoggingConfig) {
            log_config.init();
            std::panic::set_hook(Box::new(|panic: &std::panic::PanicInfo<'_>| -> () {
                tracing::error!("panic: {}", panic);
            }));
            winsvc::async_service_dispatcher!{ "cpm-proxy" => service_main }
        }
    }

    pub fn main() { Command::<Service>::execute() }
}

#[cfg(windows)]
fn main() { winsvc_glue::main() }

#[cfg(not(windows))]
fn main() {
    tracing_subscriber::fmt::init();
    run_forever(ServiceConfig::from_args());
}
