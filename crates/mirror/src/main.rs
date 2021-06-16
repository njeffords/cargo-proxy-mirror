use std::{
    str::FromStr,
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    env
};

use futures::{select, FutureExt};

use tokio::pin;

use hyper::{Body, Request, Response, Server};
use hyper::service::{make_service_fn, service_fn};
use hyper::http::{Uri, Method,StatusCode};
use hyper_staticfile::FileResponseBuilder;

mod proxy_connection;

use proxy_connection::{State, StateRef, proxy_connection};

fn parse_download_request(uri: &Uri) -> Result<(&str, &str), u16> {
    if let Some(pnq) = uri.path_and_query() {
        if pnq.query().is_none() {
            let path = pnq.path();
            if let Some(path) = path.strip_prefix("/api/v1/crates/") {
                if let Some(path) = path.strip_suffix("/download") {
                    let mut parts = path.split('/');
                    match (parts.next(), parts.next(), parts.next()) {
                        (Some(package), Some(version), None) => Ok((package, version)),
                        _ => Err(404)
                    }
                } else {
                    Err(404)
                }
            } else {
                Err(404)
            }
        } else {
            Err(400)
        }
    } else {
        Err(400)
    }
}

fn error_response(code: u16) -> Response<Body> {
    tracing::warn!("sending error code: {}", code);
    Response::builder()
        .status(StatusCode::from_u16(code).unwrap())
        .body(Default::default())
        .unwrap()
}

async fn download_cached(req: &Request<Body>, cache_path: PathBuf) -> Result<Response<Body>,u16> {
    let file = tokio::fs::File::open(cache_path).await.map_err(|_|500u16)?;
    let metadata = file.metadata().await.map_err(|_|500u16)?;
    FileResponseBuilder::new()
        .request(req)
        .build(file, metadata)
        .map_err(|_|500u16)
}

use common::down_stream;
use futures::StreamExt;

use thiserror::Error;
use displaydoc::Display;

#[derive(Error,Display,Debug)]
enum StreamError{
    /// an unexpected error occured
    Unexpected
}

async fn proxy_download(state: StateRef, package: &str, version: &str, _cache_path: Option<PathBuf>) -> Result<Response<Body>,u16> {

    let mut stream = State::begin_download(state, package.into(), version.into()).await.map_err(|_|404u16)?;

    if let Some(down_stream::Opcode::Init(headers)) = stream.next().await {

        let mut builder = Response::builder();

        builder.headers_mut().unwrap().insert(&hyper::header::CONTENT_TYPE,   hyper::header::HeaderValue::from_str(&headers.content_type).unwrap());
        builder.headers_mut().unwrap().insert(&hyper::header::CONTENT_LENGTH, headers.content_length.into());

        let stream = stream.filter_map(|oc| async move {
            match oc {
                down_stream::Opcode::Chunk(buffer) => Some(Ok(Vec::<u8>::from(buffer))),
                down_stream::Opcode::Complete(Ok(())) => None,
                _ => Some(Err(StreamError::Unexpected))
            }
        });

        builder.body(Body::wrap_stream(stream)).map_err(|_|500)

        //Ok(Response::new(Body::wrap_stream(stream)))

    } else {
        tracing::error!("expected headers for file download");
        Err(500)
    }
}

async fn download(state: StateRef, req: &Request<Body>, package: &str, version: &str) -> Result<Response<Body>,u16> {

    if let Ok(cache_path) = env::var("CPM_CRATE_CACHE") {

        let mut cache_path = PathBuf::from(&cache_path);

        cache_path.push(package);
        cache_path.push(version);

        if cache_path.exists() {
            download_cached(req, cache_path).await
        } else {
            proxy_download(state, package, version, Some(cache_path)).await
        }

    } else {
        proxy_download(state, package, version, None).await
    }
}

async fn handler(state: StateRef, req: Request<Body>) -> Result<Response<Body>, Infallible> {
    tracing::trace!("entering handler...");
    if req.method() == Method::GET {
        match parse_download_request(req.uri()) {
            Ok((package, version)) => {
                tracing::info!("package: {:?}, version: {:?}", package, version);
                download(state, &req, package, version).await.or_else(|code|Ok(error_response(code)))
            },
            Err(code) => Ok(error_response(code)),
        }
    } else {
        Ok(error_response(400))
    }
}

#[tokio::main]
async fn main() {

    tracing_subscriber::fmt::init();

    let http_end_point = env::var("CPM_HTTP_LOCAL_END_POINT").expect("value for `CPM_HTTP_LOCAL_END_POINT`");

    let addr = SocketAddr::from_str(&http_end_point).expect("legal end point value for `CPM_HTTP_LOCAL_END_POINT`");

    let state = State::new();

    let make_svc = {
        let state = state.clone();
        make_service_fn(move |_conn| {
            let state = state.clone();
            async {
                Ok::<_, Infallible>(service_fn(move |req| { handler(state.clone(), req) }))
            }
        })
    };

    let cache_server = Server::bind(&addr).serve(make_svc).fuse();

    tracing::info!("accepting HTTP connections on: {}", addr);

    let proxy_server = proxy_connection(state).fuse();


    pin!{cache_server,proxy_server};

    select!{
        c_e = cache_server => {
            if let Err(e) = c_e {
                tracing::error!("cache server error: {}", e);
            }
        },
        p_e = proxy_server => {
            if let Err(e) = p_e {
                tracing::error!("proxy server error: {}", e);
            }
        },
    }
}
