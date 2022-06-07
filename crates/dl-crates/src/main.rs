
use std::{fs::File, io::{self, Read,Seek,SeekFrom::Start}, path::PathBuf};
use structopt::StructOpt;
use reqwest::blocking::get;
use tempfile::tempfile;
use thiserror::Error;
use displaydoc::Display;

/// Download a set crates into an archive.
#[derive(StructOpt)]
struct Opts {

    /// File containing list of crates to download. The output of the
    /// `cpm check` command.
    #[structopt(parse(from_os_str))]
    input: PathBuf,

    /// Archive containing downloaded crates. Passed back to the `cpm upload`
    /// command.
    #[structopt(parse(from_os_str))]
    output: PathBuf,

}

#[derive(Error,Display,Debug)]
enum Error {

    /// the format of the crate list file is illegal
    IllegalCrateListFormat,

    /// A download error occured: {0}
    DownloadError(reqwest::StatusCode),

    /// an io error occurred: {0}
    IoError(#[from] io::Error),

    /// An http error occurred: {0}
    HttpError(#[from] reqwest::Error),
}

use Error::{IllegalCrateListFormat,DownloadError};

fn execute(input: PathBuf, output: PathBuf) -> Result<(),Error> {

    let input = {
        let mut buf = String::new();
        File::open(input)?.read_to_string(&mut buf)?;
        buf
    };

    let output = File::create(output)?;
    let mut output = tar::Builder::new(output);

    for path in input.split('\n') {

        let path = path.trim();

        if path.is_empty() {
            continue;
        }

        let (name, version)=path.split_once('/').ok_or(IllegalCrateListFormat)?;
        let name = name.trim();
        let version = version.trim();

        let url = format!("https://crates.io/api/v1/crates/{}/{}/download", name, version);

        eprintln!("downloading: {}", url);

        let mut response = get(url)?;

        let status = response.status();

        if status.as_u16() != 200 {
            Err(DownloadError(status))?;
        }

        let mut temp = tempfile()?;

        response.copy_to(&mut temp)?;

        temp.seek(Start(0))?;

        output.append_file(path, &mut temp)?;
    }

    output.finish()?;

    Ok(())
}

fn main() {
    let Opts{input,output} = Opts::from_args();
    if let Err(err) = execute(input, output) {
        eprintln!("a failure occured: {}", err);
    }
}
