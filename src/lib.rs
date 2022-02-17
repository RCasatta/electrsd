#![warn(missing_docs)]

//!
//! Electrsd
//!
//! Utility to run a regtest electrsd process, useful in integration testing environment
//!

mod versions;

use bitcoind::bitcoincore_rpc::jsonrpc::serde_json::Value;
use bitcoind::bitcoincore_rpc::RpcApi;
use bitcoind::tempfile::TempDir;
use bitcoind::{get_available_port, BitcoinD};
use electrum_client::raw_client::{ElectrumPlaintextStream, RawClient};
use log::{debug, error};
use std::ffi::OsStr;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

// re-export bitcoind
pub use bitcoind;

/// Electrs configuration parameters, implements a convenient [Default] for most common use.
///
/// Default values:
/// ```
/// let mut conf = electrsd::Conf::default();
/// conf.args = vec!["-vvv"];
/// conf.view_stderr = false;
/// conf.http_enabled = false;
/// conf.network = "regtest";
/// assert_eq!(conf, electrsd::Conf::default());
/// ```
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub struct Conf<'a> {
    /// Electrsd command line arguments
    /// note that `db-dir`, `cookie`, `cookie-file`, `daemon-rpc-addr`, `jsonrpc-import`, `electrum-rpc-addr`, `monitoring-addr`, `http-addr`  cannot be used cause they are automatically initialized.
    pub args: Vec<&'a str>,

    /// if `true` electrsd log output will not be suppressed
    pub view_stderr: bool,

    /// if `true` electrsd exposes an esplora endpoint
    pub http_enabled: bool,

    /// Must match bitcoind network
    pub network: &'a str,
}

impl Default for Conf<'_> {
    fn default() -> Self {
        Conf {
            args: vec!["-vvv"],
            view_stderr: false,
            http_enabled: false,
            network: "regtest",
        }
    }
}

/// Struct representing the bitcoind process with related information
pub struct ElectrsD {
    /// Process child handle, used to terminate the process when this struct is dropped
    process: Child,
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

    /// Wrapper of early exit status
    EarlyExit(ExitStatus),
}

impl ElectrsD {
    /// Create a new electrs process connected with the given bitcoind and default args.
    pub fn new<S: AsRef<OsStr>>(exe: S, bitcoind: &BitcoinD) -> Result<ElectrsD, Error> {
        ElectrsD::with_conf(exe, bitcoind, &Conf::default())
    }

    /// Create a new electrs process using given [Conf] connected with the given bitcoind
    pub fn with_conf<S: AsRef<OsStr>>(
        exe: S,
        bitcoind: &BitcoinD,
        conf: &Conf,
    ) -> Result<ElectrsD, Error> {
        let response = bitcoind.client.call::<Value>("getblockchaininfo", &[])?;
        if response
            .get("initialblockdownload")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            // electrum will remain idle until bitcoind is in IBD
            // bitcoind will remain in IBD if doesn't see a block from a long time, thus adding a block
            let node_address = bitcoind.client.call::<Value>("getnewaddress", &[])?;
            bitcoind
                .client
                .call::<Value>("generatetoaddress", &[1.into(), node_address])
                .unwrap();
        }

        let mut args = conf.args.clone();

        let _db_dir = TempDir::new()?;
        let db_dir = format!("{}", _db_dir.path().display());
        args.push("--db-dir");
        args.push(&db_dir);

        args.push("--network");
        args.push(conf.network);

        #[cfg(not(feature = "legacy"))]
        let cookie_file;
        #[cfg(not(feature = "legacy"))]
        {
            args.push("--cookie-file");
            cookie_file = format!("{}", bitcoind.params.cookie_file.display());
            args.push(&cookie_file);
        }

        #[cfg(feature = "legacy")]
        let mut cookie_value;
        #[cfg(feature = "legacy")]
        {
            use std::io::Read;
            args.push("--cookie");
            let mut cookie = std::fs::File::open(&bitcoind.params.cookie_file)?;
            cookie_value = String::new();
            cookie.read_to_string(&mut cookie_value)?;
            args.push(&cookie_value);
        }

        args.push("--daemon-rpc-addr");
        let rpc_socket = bitcoind.params.rpc_socket.to_string();
        args.push(&rpc_socket);

        let p2p_socket;
        if cfg!(feature = "electrs_0_9_1") {
            args.push("--daemon-p2p-addr");
            p2p_socket = bitcoind
                .params
                .p2p_socket
                .expect("electrs_0_9_1 requires bitcoind with p2p port open")
                .to_string();
            args.push(&p2p_socket);
        } else {
            args.push("--jsonrpc-import");
        }

        let electrum_url = format!("0.0.0.0:{}", get_available_port()?);
        args.push("--electrum-rpc-addr");
        args.push(&electrum_url);

        // would be better to disable it, didn't found a flag
        let monitoring = format!("0.0.0.0:{}", get_available_port()?);
        args.push("--monitoring-addr");
        args.push(&monitoring);

        let esplora_url_string;
        let esplora_url = if conf.http_enabled {
            esplora_url_string = format!("0.0.0.0:{}", get_available_port()?);
            args.push("--http-addr");
            args.push(&esplora_url_string);
            #[allow(clippy::redundant_clone)]
            Some(esplora_url_string.clone())
        } else {
            None
        };

        let view_stderr = if conf.view_stderr {
            Stdio::inherit()
        } else {
            Stdio::null()
        };

        debug!("args: {:?}", args);
        let mut process = Command::new(exe).args(args).stderr(view_stderr).spawn()?;

        let client = loop {
            if let Some(status) = process.try_wait()? {
                error!("early exit with: {:?}", status);
                return Err(Error::EarlyExit(status));
            }
            match RawClient::new(&electrum_url, None) {
                Ok(client) => break client,
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        };

        Ok(ElectrsD {
            process,
            client,
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

    /// terminate the electrs process
    pub fn kill(&mut self) -> Result<(), Error> {
        Ok(self.process.kill()?)
    }
}

impl Drop for ElectrsD {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for Error {}

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

/// Provide the electrs executable path if a version feature has been specified
pub fn downloaded_exe_path() -> Option<String> {
    if versions::HAS_FEATURE {
        Some(format!(
            "{}/electrs/{}/electrs",
            env!("OUT_DIR"),
            versions::electrs_name(),
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod test {
    use crate::bitcoind::P2P;
    use crate::ElectrsD;
    use bitcoind::bitcoincore_rpc::RpcApi;
    use electrum_client::ElectrumApi;
    use log::{debug, log_enabled, Level};
    use std::env;

    #[test]
    fn test_electrsd() {
        let (bitcoind_exe, electrs_exe) = init();
        debug!("bitcoind: {}", &bitcoind_exe);
        debug!("electrs: {}", &electrs_exe);
        let mut conf = bitcoind::Conf::default();
        conf.view_stdout = log_enabled!(Level::Debug);
        if cfg!(feature = "electrs_0_9_1") {
            conf.p2p = P2P::Yes;
        }
        let bitcoind = bitcoind::BitcoinD::with_conf(&bitcoind_exe, &conf).unwrap();
        let electrs_conf = crate::Conf {
            view_stderr: log_enabled!(Level::Debug),
            ..Default::default()
        };
        let electrsd = ElectrsD::with_conf(&electrs_exe, &bitcoind, &electrs_conf).unwrap();
        let header = electrsd.client.block_headers_subscribe().unwrap();
        assert_eq!(header.height, 1);
        let address = bitcoind.client.get_new_address(None, None).unwrap();
        bitcoind.client.generate_to_address(100, &address).unwrap();

        #[cfg(feature = "trigger")]
        electrsd.trigger().unwrap();

        let header = loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let header = electrsd.client.block_headers_subscribe().unwrap();
            if header.height > 100 {
                break header;
            }
        };
        assert_eq!(header.height, 101);

        // launch another instance to check there are no fixed port used
        let electrsd = ElectrsD::new(&electrs_exe, &bitcoind).unwrap();
        let header = electrsd.client.block_headers_subscribe().unwrap();
        assert_eq!(header.height, 101);
    }

    fn init() -> (String, String) {
        let _ = env_logger::try_init();
        let bitcoind_exe_path = if let Ok(env_bitcoind_exe) = env::var("BITCOIND_EXE") {
            env_bitcoind_exe
        } else if let Ok(downloaded_exe_path) = bitcoind::downloaded_exe_path() {
            downloaded_exe_path
        } else {
            panic!("when no version feature is specified, you must specify BITCOIND_EXE env var")
        };
        let electrs_exe_path = if let Ok(env_electrs_exe) = env::var("ELECTRS_EXE") {
            env_electrs_exe
        } else if let Some(downloaded_exe_path) = crate::downloaded_exe_path() {
            downloaded_exe_path
        } else {
            panic!("when no version feature is specified, you must specify ELECTRS_EXE env var")
        };
        (bitcoind_exe_path, electrs_exe_path)
    }
}
