//! The `spi` settings. One file, `spi.toml`, and environment variables that
//! override it (strongest first: the environment, the file, the default).
//! Everything psp-mint and esp-mint need is generated from it (see
//! `generate`); nothing is set twice.
//!
//! Network: regtest unless the file or `SPI_NETWORK` says otherwise. Main is
//! only ever used when it is written down.

use std::net::SocketAddr;
use std::path::PathBuf;

use toml::{Table, Value};

pub const DEFAULT_PSP_BIND: &str = "127.0.0.1:3000";
pub const DEFAULT_ESP_BIND: &str = "127.0.0.1:3100";
pub const DEFAULT_ESP_ADMIN_BIND: &str = "127.0.0.1:3101";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Main,
    Regtest,
    Signet,
    Testnet,
}

impl Network {
    pub fn parse(text: &str) -> Result<Network, String> {
        match text {
            "main" | "mainnet" => Ok(Network::Main),
            "regtest" => Ok(Network::Regtest),
            "signet" => Ok(Network::Signet),
            "testnet" => Ok(Network::Testnet),
            other => Err(format!(
                "`{other}` is not a network: use main, regtest, signet or testnet"
            )),
        }
    }

    /// The name the mints use.
    pub fn name(self) -> &'static str {
        match self {
            Network::Main => "main",
            Network::Regtest => "regtest",
            Network::Signet => "signet",
            Network::Testnet => "testnet",
        }
    }

    /// The `chain` bitcoind reports in `getblockchaininfo`.
    pub fn chain(self) -> &'static str {
        match self {
            Network::Main => "main",
            Network::Regtest => "regtest",
            Network::Signet => "signet",
            Network::Testnet => "test",
        }
    }

    /// Bitcoin Core's default RPC port (the same as psp-mint's default).
    pub fn rpc_port(self) -> u16 {
        match self {
            Network::Main => 8332,
            Network::Regtest => 18443,
            Network::Signet => 38332,
            Network::Testnet => 18332,
        }
    }

    /// The wallet psp-mint uses unless told otherwise.
    pub fn wallet(self) -> String {
        format!("psp-{}", self.name())
    }

    /// The folder under bitcoind's data folder that holds this chain's files,
    /// and so its cookie. Empty for main.
    pub fn subdir(self) -> &'static str {
        match self {
            Network::Main => "",
            Network::Regtest => "regtest",
            Network::Signet => "signet",
            Network::Testnet => "testnet3",
        }
    }

    /// Where Bitcoin Core writes its RPC cookie.
    pub fn cookie(self, home: &str, mac: bool) -> PathBuf {
        let sub = self.subdir();
        let mut p = PathBuf::from(home);
        if mac {
            p.push("Library/Application Support/Bitcoin");
        } else {
            p.push(".bitcoin");
        }
        if !sub.is_empty() {
            p.push(sub);
        }
        p.push(".cookie");
        p
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Psp,
    Esp,
}

impl Which {
    pub fn name(self) -> &'static str {
        match self {
            Which::Psp => "psp-mint",
            Which::Esp => "esp-mint",
        }
    }

    /// The prefix of the mint's environment variables.
    pub fn env_prefix(self) -> &'static str {
        match self {
            Which::Psp => "PSP",
            Which::Esp => "ESP",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MintSettings {
    pub bind: String,
    /// esp-mint only.
    pub admin_bind: Option<String>,
    /// The mint's binary: a name looked up on PATH, or a path.
    pub bin: String,
    /// Any mint setting `spi` does not name, passed to the generated file as
    /// it is.
    pub settings: Table,
}

/// Where and as whom the mints run when they are installed as services
/// (`spi install` writes the files; see `service`).
#[derive(Clone, Debug)]
pub struct Service {
    /// The account the services run as: no login, owns the data and the logs.
    pub user: String,
    pub group: String,
    /// Where the binaries are installed, and where the units run them from.
    pub bin_dir: String,
    /// The settings files, the env files and the health monitors' settings.
    pub etc_dir: String,
    /// A mint's data is `<data_dir>/<mint>`; bitcoind's, unless
    /// `bitcoind.datadir` is set, is `<data_dir>/bitcoind`.
    pub data_dir: String,
    pub log_dir: String,
    /// Whether a health monitor is installed for each mint.
    pub health: bool,
}

#[derive(Clone, Debug)]
pub struct Spi {
    pub dir: PathBuf,
    /// Whether this is macOS (launchd) and not Linux (systemd).
    pub mac: bool,
    pub network: Network,
    pub rpc_url: String,
    pub cookie: PathBuf,
    /// Whether the cookie was written down (in the file or the environment)
    /// and not left to its default. A service whose bitcoind is managed by
    /// spi uses a cookie of its own unless this is set.
    pub cookie_set: bool,
    pub wallet: String,
    pub create_wallet: bool,
    /// The bitcoind binary and how `spi start` runs it.
    pub bitcoind_bin: String,
    pub bitcoind_datadir: Option<String>,
    pub bitcoind_args: Vec<String>,
    /// Whether `spi start` starts bitcoind when it is not running.
    pub manage_bitcoind: bool,
    /// How long `spi start` waits for bitcoind to finish syncing before it
    /// stops and starts neither mint.
    pub sync_wait_secs: u64,
    pub psp: MintSettings,
    pub esp: MintSettings,
    pub service: Service,
    /// Every setting spi names, with its value and where it came from, in
    /// order, for `config show`.
    pub shown: Vec<(String, String, String)>,
}

pub struct Inputs<'a> {
    pub dir: PathBuf,
    /// The text of `spi.toml`, if there is a file.
    pub file: Option<&'a str>,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub home: String,
    pub mac: bool,
}

fn only_keys(t: &Table, allowed: &[&str], place: &str, problems: &mut Vec<String>) {
    for k in t.keys() {
        if !allowed.contains(&k.as_str()) {
            problems.push(format!(
                "{place}{k} is not a setting of spi (known here: {})",
                allowed.join(", ")
            ));
        }
    }
}

fn section<'a>(t: &'a Table, key: &str, problems: &mut Vec<String>) -> Option<&'a Table> {
    match t.get(key) {
        None => None,
        Some(Value::Table(s)) => Some(s),
        Some(_) => {
            problems.push(format!("[{key}] must be a table"));
            None
        }
    }
}

fn text(t: Option<&Table>, key: &str, place: &str, problems: &mut Vec<String>) -> Option<String> {
    match t?.get(key)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        _ => {
            problems.push(format!("{place}{key} must be a non-empty string"));
            None
        }
    }
}

fn check_bind(value: &str, name: &str, problems: &mut Vec<String>) {
    if value.parse::<SocketAddr>().is_err() {
        problems.push(format!("{name} is `{value}`: it must be IP:port"));
    }
}

/// A user or group name that both `useradd` and `dscl` take: lowercase
/// letters, digits, `_` and `-`, at most 32 characters, not starting with a
/// digit or `-`.
fn is_account_name(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
    first_ok
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

impl Spi {
    pub fn load(i: &Inputs) -> Result<Spi, Vec<String>> {
        let mut problems = Vec::new();
        let root: Table = match i.file {
            None => Table::new(),
            Some(text) => match text.parse::<Table>() {
                Ok(t) => t,
                Err(e) => return Err(vec![format!("spi.toml is not valid TOML: {e}")]),
            },
        };
        only_keys(
            &root,
            &["network", "bitcoind", "psp", "esp", "service"],
            "",
            &mut problems,
        );
        let bitcoind = section(&root, "bitcoind", &mut problems);
        let psp = section(&root, "psp", &mut problems);
        let esp = section(&root, "esp", &mut problems);
        let service = section(&root, "service", &mut problems);
        if let Some(t) = service {
            only_keys(
                t,
                &[
                    "user", "group", "bin_dir", "etc_dir", "data_dir", "log_dir", "health",
                ],
                "[service] ",
                &mut problems,
            );
        }
        if let Some(t) = bitcoind {
            only_keys(
                t,
                &[
                    "rpc_url",
                    "cookie",
                    "wallet",
                    "create_wallet",
                    "bin",
                    "datadir",
                    "args",
                    "manage",
                    "sync_wait_secs",
                ],
                "[bitcoind] ",
                &mut problems,
            );
        }
        if let Some(t) = psp {
            only_keys(t, &["bind", "bin", "settings"], "[psp] ", &mut problems);
        }
        if let Some(t) = esp {
            only_keys(
                t,
                &["bind", "admin_bind", "bin", "settings"],
                "[esp] ",
                &mut problems,
            );
        }

        let mut shown: Vec<(String, String, String)> = Vec::new();
        // The setting from the environment, else the file, else the default.
        let mut pick =
            |key: &str, env_name: &str, file: Option<String>, default: String| -> String {
                let (value, from) = if let Some(v) = (i.env)(env_name).filter(|v| !v.is_empty()) {
                    (v, format!("env {env_name}"))
                } else if let Some(v) = file {
                    (v, "spi.toml".to_string())
                } else {
                    (default, "default".to_string())
                };
                shown.push((key.to_string(), value.clone(), from));
                value
            };

        let network_text = pick(
            "network",
            "SPI_NETWORK",
            match root.get("network") {
                None => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => {
                    problems.push("network must be a string".into());
                    None
                }
            },
            "regtest".into(),
        );
        let network = Network::parse(&network_text).unwrap_or_else(|e| {
            problems.push(format!("network: {e}"));
            Network::Regtest
        });

        let rpc_url = pick(
            "bitcoind.rpc_url",
            "SPI_BITCOIND_RPC_URL",
            text(bitcoind, "rpc_url", "[bitcoind] ", &mut problems),
            format!("http://127.0.0.1:{}", network.rpc_port()),
        );
        if !(rpc_url.starts_with("http://") || rpc_url.starts_with("https://")) {
            problems.push(format!(
                "bitcoind.rpc_url is `{rpc_url}`: it must start with http:// or https://"
            ));
        }
        let cookie_text = text(bitcoind, "cookie", "[bitcoind] ", &mut problems);
        let cookie_set =
            cookie_text.is_some() || (i.env)("SPI_BITCOIND_COOKIE").is_some_and(|v| !v.is_empty());
        let cookie = pick(
            "bitcoind.cookie",
            "SPI_BITCOIND_COOKIE",
            cookie_text,
            network.cookie(&i.home, i.mac).display().to_string(),
        );
        let wallet = pick(
            "bitcoind.wallet",
            "SPI_BITCOIND_WALLET",
            text(bitcoind, "wallet", "[bitcoind] ", &mut problems),
            network.wallet(),
        );
        if wallet.contains('/') {
            problems.push("bitcoind.wallet must not contain `/`".into());
        }
        let bitcoind_bin = pick(
            "bitcoind.bin",
            "SPI_BITCOIND_BIN",
            text(bitcoind, "bin", "[bitcoind] ", &mut problems),
            "bitcoind".into(),
        );
        let bitcoind_datadir = (i.env)("SPI_BITCOIND_DATADIR")
            .filter(|d| !d.is_empty())
            .or_else(|| text(bitcoind, "datadir", "[bitcoind] ", &mut problems));
        let mut bitcoind_args = Vec::new();
        match bitcoind.and_then(|t| t.get("args")) {
            None => {}
            Some(Value::Array(list)) => {
                for item in list {
                    match item {
                        Value::String(s) => bitcoind_args.push(s.clone()),
                        _ => problems.push("[bitcoind] args must be a list of strings".into()),
                    }
                }
            }
            Some(_) => problems.push("[bitcoind] args must be a list of strings".into()),
        }
        let manage_bitcoind = match bitcoind.and_then(|t| t.get("manage")) {
            None => true,
            Some(Value::Boolean(b)) => *b,
            Some(_) => {
                problems.push("[bitcoind] manage must be true or false".into());
                true
            }
        };
        let sync_wait_secs = match bitcoind.and_then(|t| t.get("sync_wait_secs")) {
            None => 600,
            Some(Value::Integer(n)) if (0..=604_800).contains(n) => *n as u64,
            Some(_) => {
                problems.push(
                    "[bitcoind] sync_wait_secs must be a whole number from 0 to 604800".into(),
                );
                600
            }
        };
        let create_wallet = match bitcoind.and_then(|t| t.get("create_wallet")) {
            None => true,
            Some(Value::Boolean(b)) => *b,
            Some(_) => {
                problems.push("[bitcoind] create_wallet must be true or false".into());
                true
            }
        };
        let mut mint = |which: Which, t: Option<&Table>, problems: &mut Vec<String>| {
            let p = which.env_prefix();
            let key = |k: &str| format!("{}.{k}", p.to_lowercase());
            let place = format!("[{}] ", p.to_lowercase());
            let default_bind = match which {
                Which::Psp => DEFAULT_PSP_BIND,
                Which::Esp => DEFAULT_ESP_BIND,
            };
            let bind = pick(
                &key("bind"),
                &format!("SPI_{p}_BIND"),
                text(t, "bind", &place, problems),
                default_bind.into(),
            );
            check_bind(&bind, &key("bind"), problems);
            let admin_bind = match which {
                Which::Psp => None,
                Which::Esp => {
                    let a = pick(
                        &key("admin_bind"),
                        "SPI_ESP_ADMIN_BIND",
                        text(t, "admin_bind", &place, problems),
                        DEFAULT_ESP_ADMIN_BIND.into(),
                    );
                    check_bind(&a, &key("admin_bind"), problems);
                    Some(a)
                }
            };
            let bin = pick(
                &key("bin"),
                &format!("SPI_{p}_BIN"),
                text(t, "bin", &place, problems),
                which.name().into(),
            );
            let settings = match t.and_then(|t| t.get("settings")) {
                None => Table::new(),
                Some(Value::Table(s)) => s.clone(),
                Some(_) => {
                    problems.push(format!("{place}settings must be a table"));
                    Table::new()
                }
            };
            MintSettings {
                bind,
                admin_bind,
                bin,
                settings,
            }
        };
        let psp_settings = mint(Which::Psp, psp, &mut problems);
        let esp_settings = mint(Which::Esp, esp, &mut problems);

        let account = if i.mac { "_spmint" } else { "spmint" };
        let user = pick(
            "service.user",
            "SPI_SERVICE_USER",
            text(service, "user", "[service] ", &mut problems),
            account.into(),
        );
        let group = pick(
            "service.group",
            "SPI_SERVICE_GROUP",
            text(service, "group", "[service] ", &mut problems),
            user.clone(),
        );
        for (key, value) in [("user", &user), ("group", &group)] {
            if !is_account_name(value) {
                problems.push(format!(
                    "service.{key} is `{value}`: use 1 to 32 lowercase letters, digits, `_` or `-`, not starting with a digit or `-`"
                ));
            }
        }
        let mut folder = |key: &str, env_name: &str, default: &str| -> String {
            let value = pick(
                &format!("service.{key}"),
                env_name,
                text(service, key, "[service] ", &mut problems),
                default.into(),
            );
            let value = if value.len() > 1 {
                value.trim_end_matches('/').to_string()
            } else {
                value
            };
            if !value.starts_with('/') || value == "/" {
                problems.push(format!(
                    "service.{key} is `{value}`: it must be an absolute path other than /"
                ));
            }
            value
        };
        let bin_dir = folder("bin_dir", "SPI_SERVICE_BIN_DIR", "/usr/local/bin");
        let etc_dir = folder("etc_dir", "SPI_SERVICE_ETC_DIR", "/etc/sp-mint");
        let data_dir = folder("data_dir", "SPI_SERVICE_DATA_DIR", "/var/lib/sp-mint");
        let log_dir = folder("log_dir", "SPI_SERVICE_LOG_DIR", "/var/log/sp-mint");
        let health = match service.and_then(|t| t.get("health")) {
            None => true,
            Some(Value::Boolean(b)) => *b,
            Some(_) => {
                problems.push("[service] health must be true or false".into());
                true
            }
        };

        shown.push((
            "bitcoind.create_wallet".into(),
            create_wallet.to_string(),
            if bitcoind.is_some_and(|t| t.contains_key("create_wallet")) {
                "spi.toml".into()
            } else {
                "default".into()
            },
        ));

        shown.push((
            "service.health".into(),
            health.to_string(),
            if service.is_some_and(|t| t.contains_key("health")) {
                "spi.toml".into()
            } else {
                "default".into()
            },
        ));

        if problems.is_empty() {
            Ok(Spi {
                dir: i.dir.clone(),
                mac: i.mac,
                network,
                rpc_url,
                cookie: PathBuf::from(cookie),
                cookie_set,
                wallet,
                create_wallet,
                bitcoind_bin,
                bitcoind_datadir,
                bitcoind_args,
                manage_bitcoind,
                sync_wait_secs,
                psp: psp_settings,
                esp: esp_settings,
                service: Service {
                    user,
                    group,
                    bin_dir,
                    etc_dir,
                    data_dir,
                    log_dir,
                    health,
                },
                shown,
            })
        } else {
            Err(problems)
        }
    }

    pub fn mint(&self, which: Which) -> &MintSettings {
        match which {
            Which::Psp => &self.psp,
            Which::Esp => &self.esp,
        }
    }

    pub fn data_dir(&self, which: Which) -> PathBuf {
        self.dir.join(which.name())
    }

    pub fn generated_config(&self, which: Which) -> PathBuf {
        self.dir
            .join("generated")
            .join(format!("{}.toml", which.name()))
    }

    pub fn pid_file(&self, which: Which) -> PathBuf {
        self.dir.join("run").join(format!("{}.pid", which.name()))
    }

    pub fn log_file(&self, which: Which) -> PathBuf {
        self.dir.join("logs").join(format!("{}.log", which.name()))
    }

    /// The URL other programs use for a bind address (a wildcard address is
    /// reached through loopback).
    pub fn url(&self, which: Which) -> String {
        url_of(&self.mint(which).bind)
    }
}

pub fn url_of(bind: &str) -> String {
    match bind.parse::<SocketAddr>() {
        Ok(SocketAddr::V4(a)) if a.ip().is_unspecified() => {
            format!("http://127.0.0.1:{}", a.port())
        }
        Ok(SocketAddr::V6(a)) if a.ip().is_unspecified() => format!("http://[::1]:{}", a.port()),
        _ => format!("http://{bind}"),
    }
}

/// The file `spi install` writes when there is none.
pub const STARTER: &str = "\
# spi.toml: the one place the settings of psp-mint, esp-mint and the bitcoind
# connection are set. `spi install` writes the mints' config files from it.
#
# The network is regtest unless you write another one here (or set SPI_NETWORK).
# Main is never used by default.
network = \"regtest\"

[bitcoind]
# rpc_url, cookie and wallet default per network: leave them out unless yours differ.
# rpc_url = \"http://127.0.0.1:18443\"
# cookie = \"/path/to/.cookie\"
# wallet = \"psp-regtest\"
# create_wallet = true
# `spi start` starts bitcoind if it is not running (manage = false to leave it to you),
# then waits for it to finish syncing before it starts psp-mint:
# bin = \"bitcoind\"
# datadir = \"/path/to/bitcoin-datadir\"
# args = [\"-txindex\"]
# manage = true
# sync_wait_secs = 600

[psp]
# bind = \"127.0.0.1:3000\"

[esp]
# bind = \"127.0.0.1:3100\"
# admin_bind = \"127.0.0.1:3101\"

# `spi install` also writes what is needed to run the mints (and bitcoind, unless
# manage = false) as services that start at boot: a script to run as root, the
# unit files or launchd plists, and the env and settings files. These are the
# places and the account it uses; leave them out unless yours differ:
# [service]
# user = \"spmint\"             # _spmint on macOS
# group = \"spmint\"
# bin_dir = \"/usr/local/bin\"
# etc_dir = \"/etc/sp-mint\"
# data_dir = \"/var/lib/sp-mint\"
# log_dir = \"/var/log/sp-mint\"
# health = true               # a health monitor for each mint

# Any psp-mint or esp-mint setting spi does not name goes in a settings table and
# is copied to the generated file as it is, for example:
#
# [psp.settings.settle]
# min_confirmations = 3
#
# [esp.settings.terms]
# maturity_days = 365
";
