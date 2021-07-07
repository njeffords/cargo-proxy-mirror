use std::{fs::File, io::{self, Read}, net::{SocketAddr,TcpStream}, path::PathBuf};

use structopt::StructOpt;
use displaydoc::Display;
use thiserror::Error;

use common::{
    cpm_api::{self,PackageId,Request,Response,Overlapped,SendMessage,RecvMessage},
    SyncTcpEndPoint,
};

#[derive(StructOpt)]
struct Options {
    #[structopt(short, long, env = "CPM_API_SERVER_END_POINT")]
    server_end_point: SocketAddr,

    #[structopt(flatten)]
    command: Command
}

#[derive(StructOpt)]
enum Command {
    Check{
        /// Lock file to check dependancy availability against
        #[structopt(parse(from_os_str), default_value = "Cargo.lock")]
        lock_file: PathBuf
    },
    Upload{
        #[structopt(parse(from_os_str))]
        tarball: PathBuf
    }
}

#[derive(Error, Display, Debug)]
enum Error {
    /// An unexpected response was received from the mirror.
    UnexpectedResponse,

    /// IO error: {0}
    IoError(#[from] io::Error),

    /// Protocol error: {0}
    ProtocolError(#[from] cpm_api::Error),

    /// Protocol error: unexpected sequence recieved.
    SequenceError,

    /** A filename in the provided tar was not as expeceted. This
    probably means the tar file was not produced by the download tool.
    */
    BadTarFileName,
}


type EndPoint = SyncTcpEndPoint<SendMessage, RecvMessage>;
struct CpmApiClient(EndPoint,u32);

impl CpmApiClient {
    pub fn new(addr: SocketAddr) -> io::Result<Self> {
        Ok(Self(EndPoint::from(TcpStream::connect(addr)?),0))
    }

    pub fn upload(&mut self, name: impl Into<String>, version: impl Into<String>, file_bytes: Vec<u8>) -> Result<()> {

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

    fn close(self) -> Result<()> {
        Ok(self.0.close()?)
    }

    fn transact(&mut self, request: Request) -> Result<Response> {
        self.1 += 1;
        let sequence = self.1;
        let payload = request;
        let response = self.0.transact(&Overlapped{sequence, payload})?;
        if response.sequence != sequence {
            Err(Error::SequenceError)?;
        }
        Ok(response.payload?)
    }
}

/// Cargo.lock format
mod cargo_lock {

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
}

type Result<T> = std::result::Result<T,Error>;

fn check(server_end_point: SocketAddr, lock_file: PathBuf) -> Result<()> {

    eprintln!("checking lockfile: {:?}", lock_file);

    let lock_file = cargo_lock::load(lock_file)?;

    const SOURCE_CRATES_IO: &'static str = "registry+https://github.com/rust-lang/crates.io-index";

    let mut packages : Vec<PackageId> = Vec::new();

    for package in &lock_file.package {

        if package.checksum.is_none() {
            eprintln!("package wo/ checksum: {}", package.name);
        }

        if let Some(source) = &package.source {

            if source == SOURCE_CRATES_IO {
                packages.push(PackageId{
                    name:package.name.clone(),
                    version:package.version.clone()
                });
            } else {
                eprintln!("ignoring package with alternate source: {}", source)
            }

        } else {
            eprintln!("package wo/ source: {}", package.name);
        }
    }

    let mut end_point = EndPoint::from(TcpStream::connect(server_end_point)?);

    let response = end_point.transact(&Overlapped{sequence: 1, payload: Request::CheckMissing(packages)})?;

    end_point.close()?;

    let packages = match response.payload? {
        Response::CheckMissing(packages) => packages,
        _ => Err(Error::UnexpectedResponse)?,
    };

    for package in packages {
        println!("{}/{}", package.name, package.version);
    }

    Ok(())
}

fn upload_tarball(server_end_point: SocketAddr, tarball: PathBuf) -> Result<()> {

    let tarball = File::open(tarball)?;

    let mut tarball = tar::Archive::new(tarball);

    let mut client = CpmApiClient::new(server_end_point)?;

    for entry in tarball.entries()? {
        let mut entry = entry?;

        let path = dbg!(entry.path()?).into_owned();

        let path : &str = path.to_str().ok_or(Error::BadTarFileName)?;

        let (name,version) = path.split_once('/').ok_or(Error::BadTarFileName)?;

        let size = entry.size() as usize;

        let mut file_bytes = Vec::new();

        file_bytes.resize(size, 0);

        entry.read_exact(&mut file_bytes)?;

        client.upload(name, version, file_bytes)?;
    }

    client.close()?;

    Ok(())
}

fn main() {
    use Command::*;
    let options = Options::from_args();
    match options.command {
        Check{lock_file} => if let Err(err) = check(options.server_end_point, lock_file) {
            eprintln!("error occured: {}", err);
        },
        Upload{tarball} => if let Err(err) = upload_tarball(options.server_end_point, tarball) {
            eprintln!("error occured: {}", err);
        }
    }
}
