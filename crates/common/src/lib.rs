
/// PDU for mirror -> proxy communications
pub mod up_stream
{
    use serde::{Serialize, Deserialize};

    /// Request package download
    #[derive(Serialize, Deserialize, Debug)]
    pub struct Request {
        pub session_id: u32,
        pub package: String,
        pub version: String,
    }
}

/// PDU for proxy -> mirror communications
pub mod down_stream
{
    use serde::{Serialize, Deserialize};
    use std::fmt;

    /// Important headers received when downloading a package.
    #[derive(Serialize, Deserialize, Debug)]
    pub struct Headers {
        pub content_type: String,
        pub content_length: usize,
    }

    /// Error that can occur while attempting to download a package.
    #[derive(Serialize, Deserialize, Debug)]
    pub enum Error {
        Unspecified,
        Generic(String)
    }

    /// A buffer containing a fragment of a downloading package.
    #[derive(Serialize, Deserialize)]
    pub struct Buffer(Vec<u8>);

    /// An fragment of the package download process.
    ///
    /// A state machine, `Init -> Chunk* -> Complete`
    #[derive(Serialize, Deserialize, Debug)]
    pub enum Opcode {
        Init(Headers),
        Chunk(Buffer),
        Complete(Result<(),Error>),
    }

    /// A message received from the proxy containing an opcode assocated with a
    /// particular session.
    #[derive(Serialize, Deserialize, Debug)]
    pub struct Message {
        pub session_id: u32,
        pub opcode: Opcode
    }


    impl From<Vec<u8>> for Buffer {
        fn from(vec: Vec<u8>) -> Self {
            Self(vec)
        }
    }

    impl From<Buffer> for Vec<u8> {
        fn from(buf: Buffer) -> Self {
            buf.0
        }
    }

    impl AsRef<[u8]> for Buffer {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }

    impl AsMut<[u8]> for Buffer {
        fn as_mut(&mut self) -> &mut [u8] {
            &mut self.0
        }
    }

    impl fmt::Debug for Buffer {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "Buffer({} bytes)", self.0.len())
        }
    }
}

pub mod cpm_api;

mod api_serde {

    use std::io;
    use serde::{Serialize,de::DeserializeOwned};

    pub fn serialize<T:Serialize>(value: &T) -> Result<Vec<u8>, io::Error> {
        bincode::serialize(value).map_err(|e|io::Error::new(io::ErrorKind::InvalidData,e))
    }

    pub fn deserialize<T:DeserializeOwned>(bytes: &[u8]) -> Result<T, io::Error> {
        bincode::deserialize::<T>(bytes).map_err(|e|io::Error::new(io::ErrorKind::InvalidData,e))
    }

}

mod tcp_sender
{
    use serde::Serialize;
    use tokio::{io::{self, AsyncWriteExt},net::tcp::OwnedWriteHalf};
    use futures::{
        channel::mpsc,
        stream::StreamExt,
    };

    use super::api_serde::serialize;

    /// Wraps an [OwnedWriteHalf] to allow sending a sequence of typed values.
    ///
    /// Encodes values using [bincode] and asynchronously sends the over a TPC
    /// stream with a simple framing protocol.
    pub struct TcpSender<T:Serialize> {
        socket: OwnedWriteHalf,
        _value: std::marker::PhantomData<T>
    }


    impl<T:Serialize> TcpSender<T> {

        pub async fn send(&mut self, value: &T) -> Result<(), io::Error> {
            let bytes = &serialize(value)?;
            let len = bytes.len ();
            assert!(len < (u32::MAX as usize));
            self.socket.write_u32(len as u32).await?;
            self.socket.write_all(&bytes).await?;
            Ok(())
        }

        pub async fn close(mut self) -> Result<(),io::Error> {
            self.socket.write_u32(0).await?;
            self.socket.shutdown().await?;
            Ok(())
        }

        pub async fn mp_process(
            mut self,
            mut source: mpsc::Receiver<T>
        ) -> Result<(), io::Error> {
            while let Some(event) = source.next().await {
                self.send(&event).await?;
            }
            self.close().await?;
            Ok(())
        }
    }

    impl<T:Serialize> From<OwnedWriteHalf> for TcpSender<T> {
        fn from(socket: OwnedWriteHalf) -> Self {
            Self { socket, _value: Default::default() }
        }
    }
}

mod tcp_receiver
{
    use serde::de::DeserializeOwned;
    use tokio::{io::{self, AsyncReadExt},net::tcp::OwnedReadHalf};

    use super::api_serde::deserialize;

    /// Wraps an [OwnedReadHalf] to allow receiving a sequence of typed values.
    ///
    /// Asynchronously receives value with a simple framing protocol from a TCP
    /// stream and decodes the with [bincode].
    pub struct TcpReceiver<T:DeserializeOwned> {
        socket: OwnedReadHalf,
        _value: std::marker::PhantomData<T>
    }

    impl<T:DeserializeOwned> TcpReceiver<T> {
        pub async fn next(&mut self) -> Result<Option<T>,io::Error> {
            let mut bytes = Vec::<u8>::new();
            let len = self.socket.read_u32().await?;
            if len > 0 {
                bytes.resize(len as usize, 0);
                self.socket.read_exact(&mut bytes).await?;
                Ok(Some(deserialize(&bytes)?))
            } else {
                Ok(None)
            }
        }
    }

    impl<T:DeserializeOwned> From<OwnedReadHalf> for TcpReceiver<T> {
        fn from(socket: OwnedReadHalf) -> Self {
            Self { socket, _value: Default::default() }
        }
    }
}

pub use tcp_sender::TcpSender;
pub use tcp_receiver::TcpReceiver;

mod sync_tcp_end_point;

pub use sync_tcp_end_point::SyncTcpEndPoint;