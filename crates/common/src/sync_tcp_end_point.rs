
use std::{io,io::{Read,Write},net::{TcpStream,Shutdown}};
use byteorder::{ReadBytesExt,WriteBytesExt};
use serde::{Serialize,de::DeserializeOwned};

#[allow(non_camel_case_types)]
type be = byteorder::BigEndian;

use super::api_serde::{serialize, deserialize};

pub struct SyncTcpEndPoint<Req,Rsp> where Req:Serialize,Rsp:DeserializeOwned {
    stream: TcpStream,
    _value: std::marker::PhantomData<(Req,Rsp)>
}

impl<Req:Serialize,Rsp:DeserializeOwned> From<TcpStream> for SyncTcpEndPoint<Req,Rsp> {
    fn from(stream: TcpStream) -> Self {
        Self{stream,_value:Default::default()}
    }
}

impl<Req,Rsp> SyncTcpEndPoint<Req,Rsp> where Req:Serialize,Rsp:DeserializeOwned {

    pub fn transact(&mut self, request: &Req) -> io::Result<Rsp> {

        self.send_request(request)?;

        self.recv_response()
    }

    pub fn close(mut self) -> io::Result<()> {
        self.stream.write_u16::<be>(0 as u16)?;
        self.stream.flush()?;
        self.stream.shutdown(Shutdown::Both)?;
        Ok(())
    }

    pub fn send_request(&mut self, request: &Req) -> io::Result<()> {

        let bytes = &serialize(request)?;
        let len = bytes.len ();

        assert!(len < (u16::MAX as usize));

        self.stream.write_u16::<be>(len as u16)?;
        self.stream.write_all(&bytes)?;

        Ok(())
    }

    pub fn recv_response(&mut self) -> io::Result<Rsp> {

        let mut bytes = Vec::<u8>::new();
        let len = self.stream.read_u16::<be>()?;

        bytes.resize(len as usize, 0);
        self.stream.read_exact(&mut bytes)?;

        Ok(deserialize(&bytes)?)

    }
}
