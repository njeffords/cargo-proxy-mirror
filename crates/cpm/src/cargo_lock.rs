use std::{
    io,
    fs::File,
    path::Path,
    io::prelude::*,
};

use serde::Deserialize;

#[derive(Deserialize,Debug)]
pub struct LockFile {
    pub version: usize,
    pub package: Vec<Package>
}

#[derive(Deserialize,Debug)]
pub struct Package{
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    pub checksum: Option<String>,
    pub dependancies: Option<Vec<String>>,
}

pub fn load(path: impl AsRef<Path>) -> io::Result<LockFile> {

    let mut content = String::new();

    File::open(path.as_ref())?.read_to_string(&mut content)?;

    toml::from_str(&content).map_err(|err|io::Error::new(io::ErrorKind::InvalidData,err))
}
