use std::{net::{SocketAddr, TcpStream}, io};

use common::{SyncTcpEndPoint, cpm_api::{SendMessage, Request, PackageId, Response, Overlapped, RecvMessage}};

use super::{Error,Result};

pub type EndPoint = SyncTcpEndPoint<SendMessage, RecvMessage>;

pub struct CpmApiClient(EndPoint,u32);

impl CpmApiClient {
    pub fn new(addr: SocketAddr) -> io::Result<Self> {
        Ok(Self(EndPoint::from(TcpStream::connect(addr)?),0))
    }

    pub(crate) fn upload(&mut self, name: impl Into<String>, version: impl Into<String>, file_bytes: Vec<u8>) -> Result<()> {

        let response = self.transact(Request::UploadCrate{
            package: PackageId{name: name.into(), version: version.into()},
            content: file_bytes,
        })?;

        if let Response::UploadCrate = response {
            Ok(())
        } else {
            Err(Error::UnexpectedResponse)
        }
    }

    pub(crate) fn check(&mut self, packages: Vec<PackageId>) -> Result<Vec<PackageId>> {

        let response = self.transact(
            Request::CheckMissing(packages)
        )?;

        if let Response::CheckMissing(packages) = response {
            Ok(packages)
        } else {
            Err(Error::UnexpectedResponse)
        }
    }

    pub(crate) fn close(self) -> Result<()> {
        Ok(self.0.close()?)
    }

    fn transact(&mut self, request: Request) -> Result<Response> {
        self.1 += 1;
        let sequence = self.1;
        let payload = request;
        let response  = self.0.transact(&Overlapped{sequence, payload})?;
        if response.sequence != sequence {
            Err(Error::SequenceError)?;
        }
        Ok(response.payload?)
    }
}
