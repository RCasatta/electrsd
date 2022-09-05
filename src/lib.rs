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
use std::env;
use std::ffi::OsStr;
use std::path::PathBuf;
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
/// conf.tmpdir = None;
/// conf.staticdir = None;
/// assert_eq!(conf, electrsd::Conf::default());
/// ```
#[derive(Debug, PartialEq, Eq)]
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

    /// Optionally specify a temporary or persistent working directory for the electrs.
    /// electrs index files will be stored in this path.
    /// The following two parameters can be configured to simulate desired working directory configuration.
    ///
    /// tmpdir is Some() && staticdir is Some() : Error. Cannot be enabled at same time.
    /// tmpdir is Some(temp_path) && staticdir is None : Create temporary directory at `tmpdir` path.
    /// tmpdir is None && staticdir is Some(work_path) : Create persistent directory at `staticdir` path.
    /// tmpdir is None && staticdir is None: Creates a temporary directory in OS default temporary directory (eg /tmp) or `TEMPDIR_ROOT` env variable path.
    ///
    /// Temporary directory path
    pub tmpdir: Option<PathBuf>,

    /// Persistent directory path
    pub staticdir: Option<PathBuf>,
}

impl Default for Conf<'_> {
    fn default() -> Self {
        Conf {
            args: vec!["-vvv"],
            view_stderr: false,
            http_enabled: false,
            network: "regtest",
            tmpdir: None,
            staticdir: None,
        }
    }
}

/// Struct representing the bitcoind process with related information
pub struct ElectrsD {
    /// Process child handle, used to terminate the process when this struct is dropped
    process: Child,
    /// Electrum client connected to the electrs process
    pub client: RawClient<ElectrumPlaintextStream>,
    /// Work directory, where the electrs stores indexes and other stuffs.
    work_dir: DataDir,
    /// Url to connect to the electrum protocol (tcp)
    pub electrum_url: String,
    /// Url to connect to esplora protocol (http)
    pub esplora_url: Option<String>,
}

/// The DataDir struct defining the kind of data directory electrs will use.
/// /// Data directory can be either persistent, or temporary.
pub enum DataDir {
    /// Persistent Data Directory
    Persistent(PathBuf),
    /// Temporary Data Directory
    Temporary(TempDir),
}

impl DataDir {
    /// Return the data directory path
    fn path(&self) -> PathBuf {
        match self {
            Self::Persistent(path) => path.to_owned(),
            Self::Temporary(tmp_dir) => tmp_dir.path().to_path_buf(),
        }
    }
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

    /// Wrapper of nix Error
    Nix(nix::Error),

    /// Wrapper of early exit status
    EarlyExit(ExitStatus),

    /// Returned when both tmpdir and staticdir is specified in `Conf` options
    BothDirsSpecified,
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

        let work_dir = match (&conf.tmpdir, &conf.staticdir) {
            (Some(_), Some(_)) => return Err(Error::BothDirsSpecified),
            (Some(tmpdir), None) => DataDir::Temporary(TempDir::new_in(tmpdir)?),
            (None, Some(workdir)) => {
                std::fs::create_dir_all(workdir)?;
                DataDir::Persistent(workdir.to_owned())
            }
            (None, None) => match env::var("TEMPDIR_ROOT").map(PathBuf::from) {
                Ok(path) => DataDir::Temporary(TempDir::new_in(path)?),
                Err(_) => DataDir::Temporary(TempDir::new()?),
            },
        };

        let db_dir = format!("{}", work_dir.path().display());
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
            work_dir,
            electrum_url,
            esplora_url,
        })
    }

    /// triggers electrs sync by sending the `SIGUSR1` signal, useful to call after a block for example
    pub fn trigger(&self) -> Result<(), Error> {
        Ok(nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.process.id() as i32),
            nix::sys::signal::SIGUSR1,
        )?)
    }

    /// Return the current workdir path of the running electrs
    pub fn workdir(&self) -> PathBuf {
        self.work_dir.path()
    }

    /// terminate the electrs process
    pub fn kill(&mut self) -> Result<(), Error> {
        match self.work_dir {
            DataDir::Persistent(_) => {
                // Send SIGINT signal to electrsd
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(self.process.id() as i32),
                    nix::sys::signal::SIGINT,
                )?;
                // Wait for the process to exit
                match self.process.wait() {
                    Ok(_) => Ok(()),
                    Err(e) => Err(e.into()),
                }
            }
            DataDir::Temporary(_) => Ok(self.process.kill()?),
        }
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
        let (electrs_exe, bitcoind, electrsd) = setup_nodes();
        let header = electrsd.client.block_headers_subscribe().unwrap();
        assert_eq!(header.height, 1);
        let address = bitcoind.client.get_new_address(None, None).unwrap();
        bitcoind.client.generate_to_address(100, &address).unwrap();

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

    #[test]
    fn test_kill() {
        let (_, bitcoind, mut electrsd) = setup_nodes();
        let _ = bitcoind.client.ping().unwrap(); // without using bitcoind, it is dropped and all the rest fails.
        let _ = electrsd.client.ping().unwrap();
        assert!(electrsd.client.ping().is_ok());
        electrsd.kill().unwrap();
        assert!(electrsd.client.ping().is_err());
    }

    fn setup_nodes() -> (String, bitcoind::BitcoinD, ElectrsD) {
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
        (electrs_exe, bitcoind, electrsd)
    }

    fn init() -> (String, String) {
        let _ = env_logger::try_init();
        let bitcoind_exe_path = bitcoind::exe_path().unwrap();
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
