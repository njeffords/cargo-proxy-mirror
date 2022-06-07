# Rust Cargo Proxy Mirror

## Overview

This tool allows access to the rust crate repository from a network without direct internet access, but with a access to a server that does.

```mermaid
graph LR
MS[Mirror Server] <--> PN
PN([Protected Network]) --> PS
PS[Proxy Server] --> IN([Internet])

```

It consists of a pair of services. The proxy service sits on a machine with access to both the internet and the protected network. The mirror service sits on a machine on the protected network and provides the HTTP download side of the cargo repository protocol. The mirror accepts a connection from the proxy, and while connected can request the download of crates from the proxy when called upon by an instance of cargo running on the protected network.

The proxy is intentionally limited in functionality to minimize security concerns. It initiates a connection to the mirror service so that it does not need to accept any arbitrary connections from unknown or untrusted clients. It only supports fielding requests for packages by accepting a package name and version via a custom RPC protocol which helps prevent misuse. It only attempts connecting to its configured mirror server which makes it difficult to intercept. By being implemented in only safe rust and not allowing any incoming connections, its attack surface is significantly reduced.

## Configuration

Cargo requires access to a git repository with an index of all available packages. Currently, the copy of the cargo package index must be kept up to date manually. Cargo uses a URL in the git index repository to form its download requests. The mirrored repository must have its `config.json` file updated to point at the mirror server.

In the following example, `{mirror-end-point}` must be replaced (including the port number, see `CPM_HTTP_LOCAL_END_POINT` below).

```json
{
  "dl": "http://{mirror-end-point}/api/v1/crates",
  "api": "http://{mirror-end-point}"
}
```

It can be served easily by the `git daemon` command on the mirror server. (see its documentation  for the specifics of hosting)

```
C:\Projects\n8ware\rust\cargo-proxy-mirror\test>dir
 Volume in drive C has no label.

 Directory of C:\Projects\n8ware\rust\cargo-proxy-mirror\test

06/14/2021  07:55 PM    <DIR>          .
06/14/2021  07:55 PM    <DIR>          ..
06/14/2021  07:56 PM    <DIR>          crates.io-index.git
               0 File(s)              0 bytes
               5 Dir(s)  66,001,768,448 bytes free

C:\Projects\n8ware\rust\cargo-proxy-mirror\test>echo . > crates.io-index.git\git-daemon-export-ok
C:\Projects\n8ware\rust\cargo-proxy-mirror\test>git daemon --base-path=%CD%
```

A machine on the protected network must have its cargo config setup to point at the mirror server.

Add this  `~\.cargo\config.toml` with `{git-server-name-or-address}` set appropriately:

```toml
[source.mirror]
registry = "git://{git-server-name-or-address}/crates.io-index.git"
[source.crates-io]
replace-with = "mirror"

```

The mirror service running on the protected network requires the following environmental variables to be configured:

```
set CPM_HTTP_LOCAL_END_POINT=<address and port to accept http connections on: `0.0.0.0:3000`>
set CPM_PROXY_LOCAL_END_POINT=<address and port to accept proxy connections on: `0.0.0.0:8080`>
```

Then:

```
C:\cpm> set RUST_LOG=info
C:\cpm> mirror
```



The proxy service running on the proxy machine requires the following environmental variables to be configured:

```
set CPM_MIRROR_REMOTE_END_POINT=<ip or host name and port of mirror server: e.i. `1.2.3.4:8080`>
set CPM_CRATES_IO_BASE_URL=<base URL of crates server `https://crates.io/api/v1/crates`>
```

Then:

```
C:\cpm> set RUST_LOG=info
C:\cpm> proxy
```

## "Manual Mode"

With the proxy configured, packages are downloaded automatically, and cached in the mirror. If the proxy is not available, the mirror's cache can be updated manually using a pair of command line tools `cpm` and `dl-crates`. The `cpm` tool is used on a development machine on the protected network to determine what packages are missing, and then to push those packages once acquired with the `dl-crates` tool into the mirror's cache.

If a package is missing from the cache, `cargo` will report an error about the inability to download the package from the mirror. When this occurs, run the following command to produce a list of missing packages:

```
C:\project> cpm check > crates.list
```

Transport the `crates.list` file to a network with internet access and run the following command to download the missing packages:

```
C:\> dl-crates crates.list crates.tar
```

Transport `crates.tar` back to the original machine on the protected network and use the following command to upload the missing packages into the mirror's cache.

```
C:\project> cpm upload crates.tar
```

At this point, the cargo command can be retries and should succeed.

**NOTE:** This method only works for project dependencies as it scans the projects lock file and compare that with what is available in the mirror. This means that `cargo install <package>` won't work in "manual mode".
