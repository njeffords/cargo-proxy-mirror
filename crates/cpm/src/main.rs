use std::{
    io,
    path::PathBuf,
    net::TcpStream,
};

use structopt::StructOpt;

use common::{
    cpm_api::{PackageId,Overlapped,Request,Response},
    SyncTcpEndPoint,
};

type EndPoint = SyncTcpEndPoint<Overlapped<Request>, Overlapped<Response>>;

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



#[derive(StructOpt)]
enum Command {
    Check{
        /// Lock file to check dependancy availability against
        #[structopt(parse(from_os_str), default_value = "Cargo.lock")]
        lock_file: PathBuf
    }
}

fn check(lock_file: PathBuf) -> io::Result<()> {

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

    let mut end_point = EndPoint::from(TcpStream::connect("127.0.0.1:4004")?);

    let response = end_point.transact(&Overlapped{sequence: 1, payload: Request::CheckMissing(packages)})?;

    end_point.close()?;

    let packages = match response.payload {
        Response::CheckMissing(packages) => packages,
    };

    for package in packages {
        println!("{}/{}", package.name, package.version);
    }

    Ok(())
}


fn main() {
    use Command::*;
    match Command::from_args() {
        Check{lock_file} => if let Err(err) = check(lock_file) {
            eprintln!("error occured: {}", err);
        }
    }
}
