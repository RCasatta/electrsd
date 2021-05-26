#![warn(missing_docs)]

//!
//! Electrsd
//!
//! Utility to run a regtest electrsd process, useful in integration testing environment
//!

use bitcoind::tempfile::TempDir;
use bitcoind::{get_available_port, BitcoinD};
use electrum_client::raw_client::{ElectrumPlaintextStream, RawClient};
use std::ffi::OsStr;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Struct representing the bitcoind process with related information
pub struct ElectrsD {
    /// Process child handle, used to terminate the process when this struct is dropped
    process: Child,
    /// bitcoind process connected to this electrs
    pub bitcoind: BitcoinD,
    /// Electrum client connected to the electrs process
    pub client: RawClient<ElectrumPlaintextStream>,
    /// DB directory, where electrs store indexes. It is kept in the struct so that
    /// directory is deleted only when this struct is dropped
    _db_dir: TempDir,
    /// Url to connect to the electrum protocol (tcp)
    pub electrum_url: String,
    /// Url to connect to esplora protocol (http)
    pub esplora_url: Option<String>,
}

/// All the possible error in this crate
#[derive(Debug)]
pub enum Error {
    /// Wrapper of io Error
    Io(std::io::Error),

    /// Wrapper of bitcoind Error
    Bitcoind(bitcoind::Error),

    /// Wrapper of electrum_client Error
    ElectrumClient(electrum_client::Error),

    /// Wrapper of bitcoincore_rpc Error
    BitcoinCoreRpc(bitcoind::bitcoincore_rpc::Error),

    #[cfg(feature = "trigger")]
    /// Wrapper of nix Error
    Nix(nix::Error),
}

impl ElectrsD {
    /// Create a new electrs process connected with the given bitcoind
    pub fn new<S: AsRef<OsStr>>(
        exe: S,
        bitcoind: BitcoinD,
        view_stderr: bool,
        http_enabled: bool,
    ) -> Result<ElectrsD, Error> {
        let mut args = vec![];

        args.push("-vvv");

        let _db_dir = TempDir::new()?;
        let db_dir = format!("{}", _db_dir.path().display());
        args.push("--db-dir");
        args.push(&db_dir);

        args.push("--network");
        args.push("regtest");

        args.push("--cookie-file");
        let cookie_file = format!("{}", bitcoind.cookie_file.display());
        args.push(&cookie_file);

        args.push("--daemon-rpc-addr");
        let rpc_socket = bitcoind.rpc_socket.to_string();
        args.push(&rpc_socket);

        args.push("--jsonrpc-import");

        let electrum_url = format!("0.0.0.0:{}", get_available_port()?);
        args.push("--electrum-rpc-addr");
        args.push(&electrum_url);

        // would be better to disable it, didn't found a flag
        let monitoring = format!("0.0.0.0:{}", get_available_port()?);
        args.push("--monitoring-addr");
        args.push(&monitoring);

        let esplora_url_string;
        let esplora_url = if http_enabled {
            esplora_url_string = format!("0.0.0.0:{}", get_available_port()?);
            args.push("--http-addr");
            args.push(&esplora_url_string);
            Some(esplora_url_string.clone())
        } else {
            None
        };

        let view_stderr = if view_stderr {
            Stdio::inherit()
        } else {
            Stdio::null()
        };

        eprintln!("args: {:?}", args);
        let process = Command::new(exe).args(args).stderr(view_stderr).spawn()?;

        let client = loop {
            match RawClient::new(&electrum_url, None) {
                Ok(client) => break client,
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        };

        Ok(ElectrsD {
            client,
            bitcoind,
            process,
            _db_dir,
            electrum_url,
            esplora_url,
        })
    }

    #[cfg(feature = "trigger")]
    /// triggers electrs sync by sending the `SIGUSR1` signal, useful to call after a block for example
    pub fn trigger(&self) -> Result<(), Error> {
        Ok(nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.process.id() as i32),
            nix::sys::signal::SIGUSR1,
        )?)
    }
}

impl Drop for ElectrsD {
    fn drop(&mut self) {
        let _ = self.process.kill();
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<bitcoind::Error> for Error {
    fn from(e: bitcoind::Error) -> Self {
        Error::Bitcoind(e)
    }
}

impl From<electrum_client::Error> for Error {
    fn from(e: electrum_client::Error) -> Self {
        Error::ElectrumClient(e)
    }
}

impl From<bitcoind::bitcoincore_rpc::Error> for Error {
    fn from(e: bitcoind::bitcoincore_rpc::Error) -> Self {
        Error::BitcoinCoreRpc(e)
    }
}

#[cfg(feature = "trigger")]
impl From<nix::Error> for Error {
    fn from(e: nix::Error) -> Self {
        Error::Nix(e)
    }
}

#[cfg(test)]
mod test {
    use crate::ElectrsD;
    use bitcoind::bitcoincore_rpc::RpcApi;
    use bitcoind::BitcoinD;
    use electrum_client::ElectrumApi;
    use std::env;

    #[test]
    fn test_electrsd() {
        let bitcoind_exe = env::var("BITCOIND_EXE").expect("BITCOIND_EXE env var must be set");
        let electrs_exe = env::var("ELECTRS_EXE").expect("ELECTRS_EXE env var must be set");
        let bitcoind = BitcoinD::with_args(bitcoind_exe, vec![], true, bitcoind::P2P::No).unwrap();
        let electrsd = ElectrsD::new(electrs_exe, bitcoind, true, false).unwrap();
        let header = electrsd.client.block_headers_subscribe().unwrap();
        assert_eq!(header.height, 0);
        let address = electrsd
            .bitcoind
            .client
            .get_new_address(None, None)
            .unwrap();
        electrsd
            .bitcoind
            .client
            .generate_to_address(101, &address)
            .unwrap();

        #[cfg(feature = "trigger")]
        electrsd.trigger().unwrap();

        let header = loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let header = electrsd.client.block_headers_subscribe().unwrap();
            if header.height > 0 {
                break header;
            }
        };
        assert_eq!(header.height, 101);
    }
}
