
use std::{
    //path::PathBuf,
    str::FromStr,
    net::SocketAddr,
    sync::{Arc,Mutex},
    collections::HashMap,
};

use thiserror::Error;
use displaydoc::Display;

use futures::{channel::mpsc, sink::SinkExt};

use tokio::net::TcpListener;

use common::{up_stream, down_stream, TcpSender, TcpReceiver};

#[derive(Error,Display,Debug)]
pub enum Error {
    /// No uplink was available to request download.
    NoUplink,
    /// The active uplink was disconnected.
    UpLinkReset,
    /// An IO error occurred.
    IoError(#[from]tokio::io::Error),
}

pub type Result<T> = std::result::Result<T,Error>;

#[derive(Default)]
pub struct State{
    last_mux: u32,
    uplink: Option<mpsc::Sender<up_stream::Request>>,
    sessions: HashMap<u32,mpsc::Sender<down_stream::Opcode>>,
}

pub struct ProxyConnection(Mutex<State>);

impl State {
    fn add_session(&mut self, tx: mpsc::Sender<down_stream::Opcode>) -> u32 {
        loop {
            use std::collections::hash_map::Entry::*;
            self.last_mux += 1;
            let session_id = self.last_mux;
            match self.sessions.entry(session_id) {
                Occupied(_) => continue,
                Vacant(entry) => {
                    entry.insert(tx);
                    break session_id;
                }
            }
        }
    }

    pub fn reset_uplink_to(&mut self, stream: TcpSender<up_stream::Request>) -> Result<()> {
        if self.uplink.is_some() {
            let sessions = std::mem::replace(&mut self.sessions, Default::default());

            // distance ourself from existing connections so that they may take their time cleaning up
            tokio::spawn(async move {
                use down_stream::{Opcode::Complete,Error::Unspecified};
                for (_, mut tx) in sessions {
                    if let Err(err) = tx.send(Complete(Err(Unspecified))).await {
                        tracing::error!("failed to cleanly terminated download on upload reset: {:?}", err)
                    }
                }
            });

            self.uplink = None;
            self.sessions.clear();
        }

        let (tx, rx) = mpsc::channel::<up_stream::Request>(8);

        tokio::spawn(async move {
            if let Err(err) = stream.mp_process(rx).await {
                tracing::error!("uplink failed with: {}", err);
            }
        });

        self.uplink = Some(tx);
        Ok(())
    }
}

impl ProxyConnection {

    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Default::default())))
    }

    pub async fn begin_download(self: &Arc<Self>, package: String, version: String) -> Result<mpsc::Receiver<down_stream::Opcode>> {
        let (mut uplink, session_id, rx) = {
            let mut state = self.0.lock().unwrap();
            if let Some(uplink) = state.uplink.clone() {
                let (tx,rx) = mpsc::channel::<down_stream::Opcode>(8);
                let session_id = state.add_session(tx);
                (uplink, session_id, rx)
            } else {
                return Err(Error::NoUplink);
            }
        };
        tracing::trace!("beginning proxy download of {}/{} on {}", package, version ,session_id);
        uplink.send(up_stream::Request{session_id, package, version}).await.map_err(|_|Error::UpLinkReset)?;
        Ok(rx)
    }


    async fn process_receives(self: &Arc<Self>, mut stream: TcpReceiver<down_stream::Message>) -> Result<()> {

        while let Some(down_stream::Message{session_id, opcode}) = stream.next().await? {
            tracing::trace!("down_stream message received for {}: {:?}", session_id, opcode);
            use std::collections::hash_map::Entry::*;
            let res = match self.0.lock().unwrap().sessions.entry(session_id) {
                Occupied(entry) => Some(entry.get().clone()),
                Vacant(_) => None,
            };
            match res {
                Some(mut sender) => if let Err(err) = sender.send(opcode).await {
                    tracing::error!("failed to deliver message for session {} with: {}", session_id, err);
                    if let Occupied(entry) = self.0.lock().unwrap().sessions.entry(session_id) {
                        entry.remove();
                    }
                },
                None => tracing::warn!("received fragment for unknown session {}", session_id),
            }
        }

        Ok(())
    }

    pub async fn serve(self: Arc<Self>)-> Result<()> {

        let local_end_point = std::env::var("CPM_MIRROR_PROXY_LOCAL_END_POINT").expect("value for `CPM_MIRROR_PROXY_LOCAL_END_POINT`");

        let local_end_point = SocketAddr::from_str(&local_end_point).expect("legal end point value for `CPM_MIRROR_PROXY_LOCAL_END_POINT`");

        let listener = TcpListener::bind(local_end_point).await?;
        loop {
            let (socket, from) = listener.accept().await?;
            tracing::info!("accepted connection from: {}", from);
            let (rx,tx) = socket.into_split();
            self.0.lock().unwrap().reset_uplink_to(tx.into())?;
            match self.process_receives(rx.into()).await {
                Err(err) => {
                    tracing::error!("receive process failed with: {}", err);
                },
                Ok(_) => {},
            }
        }
    }
}
