//! PDU's for cpm tool talking to mirror

use serde::{Serialize,Deserialize};

/// Identifies a version of a package
#[derive(Serialize,Deserialize,Debug)]
pub struct PackageId{
    pub name: String,
    pub version: String,
}

#[derive(Serialize,Deserialize,Debug)]
pub enum Request {
    /// request mirror check for packages missing from the cache
    CheckMissing(Vec<PackageId>)
}

#[derive(Serialize,Deserialize,Debug)]
pub enum Response {
    /// the set of packages from the check request missing from the cache
    CheckMissing(Vec<PackageId>)
}

#[derive(Serialize,Deserialize,Debug)]
pub struct Overlapped<T> {
    pub sequence: u32,
    pub payload: T
}
