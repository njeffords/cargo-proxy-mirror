//! PDU's for cpm tool talking to mirror

use std::fmt;
use serde::{Serialize,Deserialize};
use thiserror::Error;
use displaydoc::Display;


#[derive(Error,Display,Serialize,Deserialize,Debug)]
pub enum Error {
    /// The requested function is not implemented
    NotImplemented,
}

/// Identifies a version of a package
#[derive(Serialize,Deserialize,Debug)]
pub struct PackageId{
    pub name: String,
    pub version: String,
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/{}", self.name, self.version)
    }
}

#[derive(Serialize,Deserialize,Debug)]
pub enum Request {
    /// request mirror check for packages missing from the cache
    CheckMissing(Vec<PackageId>),

    /// upload new crate version
    UploadCrate{package: PackageId, content: Vec<u8>},
}

#[derive(Serialize,Deserialize,Debug)]
pub enum Response {
    /// the set of packages from the check request missing from the cache
    CheckMissing(Vec<PackageId>),
    UploadCrate,
}

#[derive(Serialize,Deserialize,Debug)]
pub struct Overlapped<T> {
    pub sequence: u32,
    pub payload: T
}

pub type SendMessage = Overlapped<Request>;
pub type RecvMessage = Overlapped<Result<Response,Error>>;
