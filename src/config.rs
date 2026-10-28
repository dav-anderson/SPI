//! The `spi` settings. One file, `spi.toml`, and environment variables that
//! override it (strongest first: the environment, the file, the default).
//! Everything psp-mint and esp-mint need is generated from it (see
//! `generate`); nothing is set twice.
//!
//! Network: regtest unless the file or `SPI_NETWORK` says otherwise. Main is
//! only ever used when it is written down.
//!
//! What it holds: bitcoind, the AIR server (`[air]`, the stock `air_server`
//! the mints and wallets reach each other through), the two mints, the SPI's own
//! ecash-wallet (`[wallet]`) and the service layout. spi never manages fsp-mint:
//! that is the ASO's.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::signature;

/// Versions of each program kept under `<spi dir>/releases/`, besides the
/// installed one and the one before it (which a rollback needs and are never
/// removed). 0 keeps every release.
pub const DEFAULT_RELEASES_KEPT: u64 = 2;
/// Rotated copies of a log kept (`<log>.<epoch>.bak`). 0 keeps every copy.
pub const DEFAULT_LOG_KEEP: u64 = 4;
/// Days a reissued-PSP file is kept after `spi collect` took it in.
pub const DEFAULT_REVENUE_KEEP_DAYS: u64 = 90;
/// The size in MB past which a log is moved aside. 0 never moves one.
pub const DEFAULT_LOG_MAX_SIZE_MB: u64 = 50;

/// esp-mint's own `[stream]` settings (esp-mint 0.11.0): `(key, default)`. spi
/// copies them as written and `config show` names them; esp-mint checks them.
pub const ESP_STREAM: &[(&str, &str)] = &[
    ("days_ahead", "2"),
    ("record_max_bytes", "4194304"),
    ("max_services", "1000"),
];

/// The shortest rotation period psp-mint accepts besides 0, in hours: the claim
/// window (`sp_wire::psp::CLAIM_WINDOW_SECS`, 30 days) in hours, 720.
pub const PSP_MIN_ROTATION_HOURS: u64 = sp_wire::psp::CLAIM_WINDOW_SECS / 3600;

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
    /// The mint's DID (66 hex characters, what `<bin> pubkey` prints), for the
    /// SPI's ecash-wallet. When it is not written down, `spi` asks the binary
    /// (`commands::did_of`), which only works where spi can read the mint's
    /// data folder.
    pub did: Option<String>,
    /// The pin (`x.y.z`): `spi upgrade` installs this version, which may be
    /// older than the newest release. With none, it installs the newest.
    pub version: Option<String>,
}

/// The AIR server (`[air]`): the stock `air_server`, which spi installs, runs
/// and watches but does not change. Its settings reach it as environment
/// variables (`generate::air_env`).
#[derive(Clone, Debug)]
pub struct Air {
    /// Whether spi runs it (`spi start`, a service). `false`: it runs some other
    /// way and spi only reads its health.
    pub manage: bool,
    /// The binary: a name looked up on PATH, or a path.
    pub bin: String,
    /// The version spi installed (`x.y.z`). `air_server` prints none, so this is
    /// where `spi versions` gets it (see `commands::versions`), and the version
    /// `spi upgrade` brings it to: air_server publishes no releases, so this one
    /// is not an optional pin but the operator's word for what `bin` is.
    pub version: Option<String>,
    /// The server's name: its public key, 66 hex characters. The mints'
    /// `air.server` and the wallet's. `None` until the operator writes it.
    pub name: Option<String>,
    /// Where the mints and the wallet reach it: the mints' `air.host`. They
    /// always dial port 5702 of it.
    pub host: String,
    /// `AIR_SECRET_FILE`: the server's identity. `None`: spi's own place for it.
    pub identity: Option<String>,
    /// `AIR_STORAGE`. `None`: spi's own place for it.
    pub storage: Option<String>,
    /// `AIR_WS_ADDR`.
    pub ws_addr: String,
    /// `AIR_HEALTH_ADDR`.
    pub health_addr: String,
    /// `AIR_TLS_ADDR`, `AIR_TLS_CERT` and `AIR_TLS_KEY`: the server's own TLS
    /// listener, which it starts only when it has the certificate and the key.
    pub tls_addr: Option<String>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    /// The public name of the server, for the nginx file spi writes when it is
    /// set (nginx answers on it and forwards to `ws_addr`), with the certificate
    /// and key nginx uses for it.
    pub public_name: Option<String>,
    pub public_cert: Option<String>,
    pub public_key: Option<String>,
}

/// The SPI's own ecash-wallet (`[wallet]`): the wallet that holds the PSP the
/// PSP mint reissues to the SPI, and the PSP the SPI buys to start with.
#[derive(Clone, Debug)]
pub struct Wallet {
    /// The binary: a name looked up on PATH, or a path.
    pub bin: String,
    /// The wallet file. `None`: `<spi folder>/wallet/wallet.json`.
    pub file: Option<String>,
    /// The version `spi versions` expects (`x.y.z`).
    pub version: Option<String>,
    /// Any ecash-wallet setting `spi` does not name, passed to the generated
    /// `config.toml` as it is.
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
    /// How many rotated copies of a log to keep, and the size in MB past which a
    /// log is moved aside (Linux: `spi logs rotate`, run by the units and by
    /// `sp-logs.timer`; macOS does not rotate service logs). 0 keeps every
    /// copy (`log_keep`) and never moves a log aside (`log_max_size_mb`).
    pub log_keep: u64,
    pub log_max_size_mb: u64,
}

/// Where releases come from, and how many downloaded versions of a program to
/// keep (`[release]`).
#[derive(Clone, Debug)]
pub struct Release {
    /// The GitHub API that lists the releases of psp-mint and esp-mint.
    pub url: String,
    /// The site Bitcoin Core is downloaded from.
    pub bitcoin_url: String,
    /// Where each trusted builder's `<name>.gpg` key is (guix.sigs).
    pub builder_keys_url: String,
    /// Versions kept per program under `<spi dir>/releases/`, besides the
    /// installed one and the one before it. The oldest are removed first. 0
    /// keeps every release.
    pub keep: u64,
}

#[derive(Clone, Debug)]
pub struct Spi {
    pub dir: PathBuf,
    /// The home folder spi runs with (`HOME`).
    pub home: String,
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
    /// Whether `bin` was written down (file or environment): an operator-supplied
    /// bitcoind, which spi uses as it is and does not download.
    pub bitcoind_bin_set: bool,
    pub bitcoind_datadir: Option<String>,
    pub bitcoind_args: Vec<String>,
    /// `[bitcoind] min_signatures`: how many trusted builders must have signed
    /// Bitcoin Core's `SHA256SUMS`.
    pub bitcoind_min_signatures: u64,
    /// `[bitcoind] signers`: replaces the builders compiled into spi.
    pub bitcoind_signers: Option<Vec<(String, String)>>,
    /// Whether `spi start` starts bitcoind when it is not running.
    pub manage_bitcoind: bool,
    /// How long `spi start` waits for bitcoind to finish syncing before it
    /// stops and starts neither mint.
    pub sync_wait_secs: u64,
    /// `[bitcoind] version`: the pin for `spi upgrade` (`x.y` or `x.y.z`, as
    /// Bitcoin Core numbers its releases). With none, the newest release.
    pub bitcoind_version: Option<String>,
    pub release: Release,
    pub air: Air,
    pub wallet_settings: Wallet,
    pub psp: MintSettings,
    pub esp: MintSettings,
    /// `[psp] burn`: whether psp-mint may burn the PSP it is paid (psp-mint's
    /// `burn.enabled`). Off by default: the PSP it is paid is reissued to the
    /// SPI's own ecash-wallet.
    pub burn: bool,
    /// `[psp] burn_threshold_psp`: with `burn` on, the PSP the SPI's wallet is
    /// kept at (`burn.threshold_psp`): psp-mint reissues paid PSP only while the
    /// wallet holds less (its balance as `spi` publishes it, see
    /// `balance_figure`), and burns the rest.
    pub burn_threshold_psp: u64,
    /// `[psp] revenue_keep_days`: `spi collect` removes a reissued-PSP file this
    /// many days after it was taken in (0: never).
    pub revenue_keep_days: u64,
    /// `[psp] burn_balance_file`: where `spi` publishes the wallet's PSP balance
    /// for psp-mint's `burn.threshold_psp` (psp-mint's `burn.balance_file`), when
    /// written down. `None`: see [`Spi::balance_figure_file`].
    pub burn_balance_file: Option<String>,
    /// `[psp] data_dir`: where psp-mint's data is, for the reissued PSP in its
    /// `revenue/` folder. `None`: `<spi folder>/psp-mint`, where `spi start` runs
    /// it. A service keeps it under `[service] data_dir`.
    pub psp_data_dir: Option<String>,
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
    /// The operating system's name, as `std::env::consts::OS` gives it.
    /// `"macos"` gets the macOS layout (folders, service account, launchd);
    /// any other name gets the Linux one.
    pub os: &'a str,
}

/// Whether `os` (a `std::env::consts::OS` name) is macOS.
pub fn is_mac(os: &str) -> bool {
    os == "macos"
}

/// The settings spi.toml no longer has: (the place, the key, why and what to do).
/// A file that still sets one is refused with this text, not with "not a setting
/// of spi", so an old file is never silently ignored.
const REMOVED: &[(&str, &str, &str)] = &[(
    "[psp] ",
    "reconciliation_window_days",
    "psp-mint no longer has a reconciliation window: a payer claims what a payment is owed within 30 days of the owed record being written, and that window is not a setting",
)];

fn only_keys(t: &Table, allowed: &[&str], place: &str, problems: &mut Vec<String>) {
    for k in t.keys() {
        if let Some((_, _, why)) = REMOVED.iter().find(|(p, key, _)| *p == place && key == k) {
            problems.push(format!(
                "{place}{k} was removed in spi 0.3.0: {why}. Remove it"
            ));
        } else if !allowed.contains(&k.as_str()) {
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

fn boolean(t: Option<&Table>, key: &str, place: &str, problems: &mut Vec<String>) -> Option<bool> {
    match t?.get(key)? {
        Value::Boolean(b) => Some(*b),
        _ => {
            problems.push(format!("{place}{key} must be true or false"));
            None
        }
    }
}

fn whole(
    t: Option<&Table>,
    key: &str,
    range: (u64, u64),
    place: &str,
    problems: &mut Vec<String>,
) -> Option<u64> {
    match t?.get(key)? {
        Value::Integer(n) if *n >= 0 && (range.0..=range.1).contains(&(*n as u64)) => {
            Some(*n as u64)
        }
        _ => {
            problems.push(format!(
                "{place}{key} must be a whole number from {} to {}",
                range.0, range.1
            ));
            None
        }
    }
}

/// Whether `value` is a service's key: 66 hex characters, a compressed point
/// (`02` or `03` first).
pub fn is_key_hex(value: &str) -> bool {
    value.len() == 66
        && value.chars().all(|c| c.is_ascii_hexdigit())
        && (value.starts_with("02") || value.starts_with("03"))
}

/// Whether `value` is a program's version: `x.y.z`, digits only.
fn is_version(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// A Bitcoin Core version: two or three numbers.
fn is_core_version(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// A host the AIR client can dial: a name or an address, with no scheme, path or
/// port.
fn is_host(value: &str) -> bool {
    !value.is_empty()
        && !value.contains("://")
        && !value.contains('/')
        && (value.parse::<std::net::Ipv6Addr>().is_ok() || !value.contains(':'))
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
        let mac = is_mac(i.os);
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
            &[
                "network", "bitcoind", "air", "psp", "esp", "wallet", "service", "release",
            ],
            "",
            &mut problems,
        );
        let bitcoind = section(&root, "bitcoind", &mut problems);
        let psp = section(&root, "psp", &mut problems);
        let esp = section(&root, "esp", &mut problems);
        let service = section(&root, "service", &mut problems);
        let release_t = section(&root, "release", &mut problems);
        let air_t = section(&root, "air", &mut problems);
        let wallet_t = section(&root, "wallet", &mut problems);
        if let Some(t) = air_t {
            only_keys(
                t,
                &[
                    "manage",
                    "bin",
                    "version",
                    "name",
                    "host",
                    "identity",
                    "storage",
                    "ws_addr",
                    "health_addr",
                    "tls_addr",
                    "tls_cert",
                    "tls_key",
                    "public_name",
                    "public_cert",
                    "public_key",
                    "fee_read",
                    "fee_write",
                ],
                "[air] ",
                &mut problems,
            );
        }
        if let Some(t) = wallet_t {
            only_keys(
                t,
                &["bin", "file", "version", "settings"],
                "[wallet] ",
                &mut problems,
            );
        }
        if let Some(t) = service {
            only_keys(
                t,
                &[
                    "user",
                    "group",
                    "bin_dir",
                    "etc_dir",
                    "data_dir",
                    "log_dir",
                    "health",
                    "log_keep",
                    "log_max_size_mb",
                ],
                "[service] ",
                &mut problems,
            );
        }
        if let Some(t) = release_t {
            only_keys(
                t,
                &["url", "bitcoin_url", "builder_keys_url", "keep"],
                "[release] ",
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
                    "version",
                    "min_signatures",
                    "signers",
                ],
                "[bitcoind] ",
                &mut problems,
            );
        }
        if let Some(t) = psp {
            only_keys(
                t,
                &[
                    "bind",
                    "bin",
                    "settings",
                    "did",
                    "version",
                    "burn",
                    "burn_threshold_psp",
                    "revenue_keep_days",
                    "burn_balance_file",
                    "data_dir",
                ],
                "[psp] ",
                &mut problems,
            );
        }
        if let Some(t) = esp {
            only_keys(
                t,
                &["bind", "admin_bind", "bin", "settings", "did", "version"],
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
            network.cookie(&i.home, mac).display().to_string(),
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
        let bitcoind_bin_set = (i.env)("SPI_BITCOIND_BIN").is_some_and(|v| !v.is_empty())
            || bitcoind.is_some_and(|t| t.contains_key("bin"));
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
        let min_signatures = whole(
            bitcoind,
            "min_signatures",
            (1, 100),
            "[bitcoind] ",
            &mut problems,
        );
        let bitcoind_min_signatures = min_signatures.unwrap_or(signature::DEFAULT_MIN_SIGNATURES);
        let mut bitcoind_signers = None;
        match bitcoind.and_then(|t| t.get("signers")) {
            None => {}
            Some(Value::Array(list)) => {
                let parsed = list
                    .iter()
                    .map(|v| v.as_str().and_then(signature::parse_signer));
                let parsed: Option<Vec<_>> = parsed.collect();
                if parsed.is_none() {
                    problems.push("[bitcoind] signers must list strings like \"<40 hex digits of a fingerprint> <builder-keys file name>\"".into());
                }
                bitcoind_signers = parsed;
            }
            Some(_) => problems.push("[bitcoind] signers must be a list of strings".into()),
        }
        let trusted = bitcoind_signers
            .as_ref()
            .map_or(signature::BUILDERS.len(), Vec::len);
        if bitcoind_min_signatures as usize > trusted {
            problems.push(format!(
                "bitcoind.min_signatures is {bitcoind_min_signatures}, but only {trusted} signers are trusted"
            ));
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
        let bitcoind_version = text(bitcoind, "version", "[bitcoind] ", &mut problems);
        if let Some(v) = &bitcoind_version
            && !is_core_version(v)
        {
            problems.push(format!(
                "bitcoind.version is `{v}`: a version is two or three numbers, like 29.1 or 29.1.0 (a release candidate such as 29.0rc1 is not installed)"
            ));
        }
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
            let did = (i.env)(&format!("SPI_{p}_DID"))
                .filter(|v| !v.is_empty())
                .or_else(|| text(t, "did", &place, problems));
            if let Some(d) = &did
                && !is_key_hex(d)
            {
                problems.push(format!(
                    "{} is `{d}`: a mint's DID is 66 hex characters (what `{} pubkey` prints)",
                    key("did"),
                    which.name()
                ));
            }
            let version = text(t, "version", &place, problems);
            if let Some(v) = &version
                && !is_version(v)
            {
                problems.push(format!(
                    "{} is `{v}`: a version is three numbers, like 0.5.0 (a release candidate such as 0.7.0-rc1 is not installed)",
                    key("version")
                ));
            }
            MintSettings {
                bind,
                admin_bind,
                bin,
                settings,
                did,
                version,
            }
        };
        let psp_settings = mint(Which::Psp, psp, &mut problems);
        // psp-mint needs a rotation period that outlives the 30-day claim window
        // (a payer claims with the keyset that is current when the claim is made),
        // so 1 to 719 hours is refused here, before psp-mint refuses to start.
        // esp-mint's period is its own: spi does not look at it.
        if let Some(n) = psp_settings
            .settings
            .get("rotation")
            .and_then(Value::as_table)
            .and_then(|t| t.get("period_hours"))
            .and_then(Value::as_integer)
            && (1..PSP_MIN_ROTATION_HOURS as i64).contains(&n)
        {
            problems.push(format!(
                "psp.settings.rotation.period_hours is {n}: psp-mint needs 0 (never rotate) or at least {PSP_MIN_ROTATION_HOURS} hours, so that the keyset a payer claims with outlives the 30-day claim window"
            ));
        }
        let esp_settings = mint(Which::Esp, esp, &mut problems);

        // psp-mint settings spi names (so that they are shown by name, and a
        // pass-through of the same key is refused).
        let burn = boolean(psp, "burn", "[psp] ", &mut problems).unwrap_or(false);
        let burn_threshold_psp = whole(
            psp,
            "burn_threshold_psp",
            (0, 1 << 62),
            "[psp] ",
            &mut problems,
        );
        let revenue_keep_days = whole(
            psp,
            "revenue_keep_days",
            (0, 36_500),
            "[psp] ",
            &mut problems,
        );
        let burn_balance_file = text(psp, "burn_balance_file", "[psp] ", &mut problems);
        if let Some(f) = &burn_balance_file
            && !f.starts_with('/')
        {
            problems.push(format!(
                "psp.burn_balance_file is `{f}`: it must be an absolute path"
            ));
        }
        let psp_data_dir = text(psp, "data_dir", "[psp] ", &mut problems);
        if let Some(d) = &psp_data_dir
            && !d.starts_with('/')
        {
            problems.push(format!(
                "psp.data_dir is `{d}`: it must be an absolute path"
            ));
        }

        // The AIR server.
        let air_manage = boolean(air_t, "manage", "[air] ", &mut problems).unwrap_or(true);
        let air_bin = pick(
            "air.bin",
            "SPI_AIR_BIN",
            text(air_t, "bin", "[air] ", &mut problems),
            "air_server".into(),
        );
        let air_version = text(air_t, "version", "[air] ", &mut problems);
        if let Some(v) = &air_version
            && !is_version(v)
        {
            problems.push(format!(
                "air.version is `{v}`: a version is three numbers, like 1.0.1"
            ));
        }
        let air_name = (i.env)("SPI_AIR_NAME")
            .filter(|v| !v.is_empty())
            .or_else(|| text(air_t, "name", "[air] ", &mut problems));
        if let Some(n) = &air_name
            && !is_key_hex(n)
        {
            problems.push(format!(
                "air.name is `{n}`: the AIR server's name is its public key, 66 hex characters"
            ));
        }
        let air_host = pick(
            "air.host",
            "SPI_AIR_HOST",
            text(air_t, "host", "[air] ", &mut problems),
            "127.0.0.1".into(),
        );
        if !is_host(&air_host) {
            problems.push(format!(
                "air.host is `{air_host}`: a host name or IP with no scheme, path or port (the mints always dial port 5702)"
            ));
        }
        let air_identity = text(air_t, "identity", "[air] ", &mut problems);
        let air_storage = text(air_t, "storage", "[air] ", &mut problems);
        for (key, value) in [("identity", &air_identity), ("storage", &air_storage)] {
            if let Some(v) = value
                && !v.starts_with('/')
            {
                problems.push(format!("air.{key} is `{v}`: it must be an absolute path"));
            }
        }
        let air_ws_addr = pick(
            "air.ws_addr",
            "SPI_AIR_WS_ADDR",
            text(air_t, "ws_addr", "[air] ", &mut problems),
            "127.0.0.1:5702".into(),
        );
        check_bind(&air_ws_addr, "air.ws_addr", &mut problems);
        let air_health_addr = pick(
            "air.health_addr",
            "SPI_AIR_HEALTH_ADDR",
            text(air_t, "health_addr", "[air] ", &mut problems),
            "127.0.0.1:5701".into(),
        );
        check_bind(&air_health_addr, "air.health_addr", &mut problems);
        let air_tls_addr = text(air_t, "tls_addr", "[air] ", &mut problems);
        let air_tls_cert = text(air_t, "tls_cert", "[air] ", &mut problems);
        let air_tls_key = text(air_t, "tls_key", "[air] ", &mut problems);
        if let Some(a) = &air_tls_addr {
            check_bind(a, "air.tls_addr", &mut problems);
        }
        for (key, value) in [("tls_cert", &air_tls_cert), ("tls_key", &air_tls_key)] {
            if let Some(v) = value
                && !v.starts_with('/')
            {
                problems.push(format!("air.{key} is `{v}`: it must be an absolute path"));
            }
        }
        if air_tls_cert.is_some() != air_tls_key.is_some() {
            problems.push(
                "air.tls_cert and air.tls_key go together: air_server starts its TLS listener only with both".into(),
            );
        }
        if air_tls_addr.is_some() && air_tls_cert.is_none() {
            problems.push(
                "air.tls_addr is set without air.tls_cert and air.tls_key: air_server would not start its TLS listener".into(),
            );
        }
        if air_tls_addr.is_none() && air_tls_cert.is_some() {
            problems.push(
                "air.tls_cert and air.tls_key are set without air.tls_addr: air_server would listen on its own default, 0.0.0.0:443. Set air.tls_addr".into(),
            );
        }
        let air_public_name = text(air_t, "public_name", "[air] ", &mut problems);
        if let Some(n) = &air_public_name
            && !is_host(n)
        {
            problems.push(format!(
                "air.public_name is `{n}`: a host name, with no scheme, path or port"
            ));
        }
        let air_public_cert = text(air_t, "public_cert", "[air] ", &mut problems);
        let air_public_key = text(air_t, "public_key", "[air] ", &mut problems);
        for (key, value) in [
            ("public_cert", &air_public_cert),
            ("public_key", &air_public_key),
        ] {
            if let Some(v) = value
                && !v.starts_with('/')
            {
                problems.push(format!("air.{key} is `{v}`: it must be an absolute path"));
            }
        }
        if air_public_name.is_some() && (air_public_cert.is_none() || air_public_key.is_none()) {
            problems.push(
                "air.public_name needs air.public_cert and air.public_key, the certificate and key nginx answers with".into(),
            );
        }
        if air_public_name.is_none() && (air_public_cert.is_some() || air_public_key.is_some()) {
            problems
                .push("air.public_cert and air.public_key are for air.public_name: set it".into());
        }
        // The AIR server's fees. air_server 1.0.1 has no hook for one (ROADMAP:
        // the fees are per read and per write in the PSP, which air_server does
        // not know), so only 0 is accepted, and a value says what is missing.
        for key in ["fee_read", "fee_write"] {
            match air_t.and_then(|t| t.get(key)) {
                None | Some(Value::Integer(0)) => {}
                Some(Value::Integer(n)) => problems.push(format!(
                    "air.{key} = {n}: stock air_server 1.0.1 has no fee hook, so it takes no fee (upstream item 1, the air_server fee hook). Leave it 0 or remove it"
                )),
                Some(_) => problems.push(format!(
                    "air.{key} must be a whole number, and 0 until upstream item 1 (the air_server fee hook) exists"
                )),
            }
        }

        // The SPI's own ecash-wallet.
        let wallet_bin = pick(
            "wallet.bin",
            "SPI_WALLET_BIN",
            text(wallet_t, "bin", "[wallet] ", &mut problems),
            "ecash-wallet".into(),
        );
        let wallet_file = (i.env)("SPI_WALLET_FILE")
            .filter(|v| !v.is_empty())
            .or_else(|| text(wallet_t, "file", "[wallet] ", &mut problems));
        if let Some(f) = &wallet_file
            && !f.starts_with('/')
        {
            problems.push(format!("wallet.file is `{f}`: it must be an absolute path"));
        }
        let wallet_version = text(wallet_t, "version", "[wallet] ", &mut problems);
        if let Some(v) = &wallet_version
            && !is_version(v)
        {
            problems.push(format!(
                "wallet.version is `{v}`: a version is three numbers, like 0.6.0"
            ));
        }
        let wallet_settings = match wallet_t.and_then(|t| t.get("settings")) {
            None => Table::new(),
            Some(Value::Table(s)) => s.clone(),
            Some(_) => {
                problems.push("[wallet] settings must be a table".into());
                Table::new()
            }
        };

        let account = if mac { "_spmint" } else { "spmint" };
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

        let from = |present: bool| -> String {
            if present {
                "spi.toml".into()
            } else {
                "default".into()
            }
        };
        let has = |t: Option<&Table>, key: &str| t.is_some_and(|t| t.contains_key(key));
        // psp-mint prunes its own payment records; spi only names the two
        // settings (they are passed through under [psp.settings.settle], and
        // psp-mint validates them), so that `config show` shows them.
        for (key, default) in [("retention_days", "90"), ("prune_horizon_days", "210")] {
            let set = psp_settings
                .settings
                .get("settle")
                .and_then(Value::as_table)
                .and_then(|t| t.get(key));
            shown.push((
                format!("psp.settings.settle.{key}"),
                match set {
                    Some(v) => v.to_string(),
                    None => format!("{default} (psp-mint's own)"),
                },
                from(set.is_some()),
            ));
        }
        // esp-mint publishes the keys of every service it knows on AIR (esp-mint
        // 0.11.0), so that a wallet can check the keys it is signed with. spi only
        // names the three settings of that stream (they are passed through under
        // [esp.settings.stream], and esp-mint validates them and reloads them
        // live), so that `config show` shows them.
        for (key, default) in ESP_STREAM {
            let set = esp_settings
                .settings
                .get("stream")
                .and_then(Value::as_table)
                .and_then(|t| t.get(*key));
            shown.push((
                format!("esp.settings.stream.{key}"),
                match set {
                    Some(v) => v.to_string(),
                    None => format!("{default} (esp-mint's own)"),
                },
                from(set.is_some()),
            ));
        }
        shown.push(("psp.burn".into(), burn.to_string(), from(has(psp, "burn"))));
        shown.push((
            "psp.revenue_keep_days".into(),
            revenue_keep_days
                .unwrap_or(DEFAULT_REVENUE_KEEP_DAYS)
                .to_string(),
            from(revenue_keep_days.is_some()),
        ));
        shown.push((
            "psp.burn_threshold_psp".into(),
            burn_threshold_psp.unwrap_or(0).to_string(),
            from(burn_threshold_psp.is_some()),
        ));
        shown.push((
            "psp.burn_balance_file".into(),
            figure_file(
                burn_balance_file.as_deref(),
                &match &psp_data_dir {
                    Some(d) => PathBuf::from(d),
                    None => i.dir.join(Which::Psp.name()),
                },
                network,
                &data_dir,
            )
            .display()
            .to_string(),
            from(burn_balance_file.is_some()),
        ));
        for (key, value, present) in [
            ("psp.did", &psp_settings.did, psp_settings.did.is_some()),
            ("esp.did", &esp_settings.did, esp_settings.did.is_some()),
        ] {
            shown.push((
                key.into(),
                value
                    .clone()
                    .unwrap_or_else(|| "(asked of the binary)".into()),
                from(present),
            ));
        }
        shown.push((
            "air.manage".into(),
            air_manage.to_string(),
            from(has(air_t, "manage")),
        ));
        shown.push((
            "air.name".into(),
            air_name.clone().unwrap_or_else(|| "(not set)".into()),
            if (i.env)("SPI_AIR_NAME").is_some_and(|v| !v.is_empty()) {
                "env SPI_AIR_NAME".into()
            } else {
                from(air_name.is_some())
            },
        ));
        shown.push((
            "air.fee_read".into(),
            "0".into(),
            from(has(air_t, "fee_read")),
        ));
        shown.push((
            "air.fee_write".into(),
            "0".into(),
            from(has(air_t, "fee_write")),
        ));
        shown.push((
            "wallet.file".into(),
            wallet_file
                .clone()
                .unwrap_or_else(|| format!("{}/wallet/wallet.json", i.dir.display())),
            if (i.env)("SPI_WALLET_FILE").is_some_and(|v| !v.is_empty()) {
                "env SPI_WALLET_FILE".into()
            } else {
                from(wallet_file.is_some())
            },
        ));
        for (key, version, present) in [
            (
                "bitcoind.version",
                bitcoind_version.clone(),
                has(bitcoind, "version"),
            ),
            ("air.version", air_version.clone(), has(air_t, "version")),
            (
                "psp.version",
                psp_settings.version.clone(),
                psp_settings.version.is_some(),
            ),
            (
                "esp.version",
                esp_settings.version.clone(),
                esp_settings.version.is_some(),
            ),
            (
                "wallet.version",
                wallet_version.clone(),
                wallet_version.is_some(),
            ),
        ] {
            shown.push((
                key.into(),
                version.unwrap_or_else(|| "(not set)".into()),
                from(present),
            ));
        }
        let log_keep = whole(service, "log_keep", (0, 52), "[service] ", &mut problems);
        let log_max_size_mb = whole(
            service,
            "log_max_size_mb",
            (0, 10_240),
            "[service] ",
            &mut problems,
        );
        let mut url = |key: &str, default: &str| -> String {
            let value = text(release_t, key, "[release] ", &mut problems)
                .unwrap_or_else(|| default.to_string());
            let value = value.trim_end_matches('/').to_string();
            if !(value.starts_with("http://") || value.starts_with("https://"))
                || value.contains(char::is_whitespace)
            {
                problems.push(format!(
                    "release.{key} is `{value}`: it must be an http:// or https:// address"
                ));
            }
            value
        };
        let release = Release {
            url: url("url", "https://api.github.com"),
            bitcoin_url: url("bitcoin_url", "https://bitcoincore.org"),
            builder_keys_url: url(
                "builder_keys_url",
                "https://raw.githubusercontent.com/bitcoin-core/guix.sigs/main/builder-keys",
            ),
            keep: whole(release_t, "keep", (0, 100), "[release] ", &mut problems)
                .unwrap_or(DEFAULT_RELEASES_KEPT),
        };
        let (log_keep, log_max_size_mb) = (
            log_keep.unwrap_or(DEFAULT_LOG_KEEP),
            log_max_size_mb.unwrap_or(DEFAULT_LOG_MAX_SIZE_MB),
        );
        for (key, value, present) in [
            ("service.log_keep", log_keep, has(service, "log_keep")),
            (
                "service.log_max_size_mb",
                log_max_size_mb,
                has(service, "log_max_size_mb"),
            ),
            ("release.keep", release.keep, has(release_t, "keep")),
        ] {
            shown.push((key.into(), value.to_string(), from(present)));
        }
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
                home: i.home.clone(),
                mac,
                network,
                rpc_url,
                cookie: PathBuf::from(cookie),
                cookie_set,
                wallet,
                create_wallet,
                bitcoind_bin,
                bitcoind_bin_set,
                bitcoind_datadir,
                bitcoind_args,
                bitcoind_min_signatures,
                bitcoind_signers,
                manage_bitcoind,
                sync_wait_secs,
                bitcoind_version,
                air: Air {
                    manage: air_manage,
                    bin: air_bin,
                    version: air_version,
                    name: air_name,
                    host: air_host,
                    identity: air_identity,
                    storage: air_storage,
                    ws_addr: air_ws_addr,
                    health_addr: air_health_addr,
                    tls_addr: air_tls_addr,
                    tls_cert: air_tls_cert,
                    tls_key: air_tls_key,
                    public_name: air_public_name,
                    public_cert: air_public_cert,
                    public_key: air_public_key,
                },
                wallet_settings: Wallet {
                    bin: wallet_bin,
                    file: wallet_file,
                    version: wallet_version,
                    settings: wallet_settings,
                },
                psp: psp_settings,
                esp: esp_settings,
                burn,
                burn_threshold_psp: burn_threshold_psp.unwrap_or(0),
                revenue_keep_days: revenue_keep_days.unwrap_or(DEFAULT_REVENUE_KEEP_DAYS),
                burn_balance_file,
                psp_data_dir,
                service: Service {
                    user,
                    group,
                    bin_dir,
                    etc_dir,
                    data_dir,
                    log_dir,
                    health,
                    log_keep,
                    log_max_size_mb,
                },
                release,
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

    /// The folder `spi start` runs the AIR server in.
    pub fn air_dir(&self) -> PathBuf {
        self.dir.join("air")
    }

    pub fn air_pid_file(&self) -> PathBuf {
        self.dir.join("run").join("air_server.pid")
    }

    pub fn bitcoind_log_file(&self) -> PathBuf {
        self.dir.join("logs").join("bitcoind.log")
    }

    pub fn air_log_file(&self) -> PathBuf {
        self.dir.join("logs").join("air_server.log")
    }

    /// The AIR server's identity file (`AIR_SECRET_FILE`): `[air] identity`, else
    /// spi's own place, which for a service is in `[service] etc_dir`.
    pub fn air_identity(&self, service: bool) -> String {
        match &self.air.identity {
            Some(p) => p.clone(),
            None if service => format!("{}/air-identity.json", self.service.etc_dir),
            None => self.air_dir().join("identity.json").display().to_string(),
        }
    }

    /// The AIR server's database (`AIR_STORAGE`), likewise.
    pub fn air_storage(&self, service: bool) -> String {
        match &self.air.storage {
            Some(p) => p.clone(),
            None if service => format!("{}/air-server/STORAGE.db", self.service.data_dir),
            None => self.air_dir().join("STORAGE.db").display().to_string(),
        }
    }

    /// The SPI's ecash-wallet file.
    pub fn wallet_file(&self) -> PathBuf {
        match &self.wallet_settings.file {
            Some(f) => PathBuf::from(f),
            None => self.dir.join("wallet").join("wallet.json"),
        }
    }

    /// The ecash-wallet settings file spi writes, beside nothing else: the wallet
    /// is told where it is (`--config`).
    pub fn wallet_config(&self) -> PathBuf {
        self.dir.join("generated").join("ecash-wallet.toml")
    }

    /// Where psp-mint's data is (`[psp] data_dir`, else where `spi start` runs
    /// it).
    pub fn psp_data(&self) -> PathBuf {
        match &self.psp_data_dir {
            Some(d) => PathBuf::from(d),
            None => self.data_dir(Which::Psp),
        }
    }

    /// The folder psp-mint writes the PSP it reissues to the SPI's wallet in: its
    /// `revenue/` folder under the network's data folder.
    pub fn revenue_dir(&self) -> PathBuf {
        self.psp_data().join(self.network.name()).join("revenue")
    }

    /// Where `spi` publishes the SPI wallet's PSP balance for psp-mint's burn
    /// threshold (`[psp] burn_balance_file`): the setting, else
    /// `<service data_dir>/spi-share/wallet_balance.json` when psp-mint's data
    /// is under `[service] data_dir` (the service layout: the folder is made
    /// group-writable for `spi`), else `wallet_balance.json` in psp-mint's
    /// network folder, which is psp-mint's own default.
    pub fn balance_figure_file(&self) -> PathBuf {
        figure_file(
            self.burn_balance_file.as_deref(),
            &self.psp_data(),
            self.network,
            &self.service.data_dir,
        )
    }

    /// The URL other programs use for a bind address (a wildcard address is
    /// reached through loopback).
    pub fn url(&self, which: Which) -> String {
        url_of(&self.mint(which).bind)
    }
}

/// See [`Spi::balance_figure_file`].
fn figure_file(
    explicit: Option<&str>,
    psp_data: &Path,
    network: Network,
    service_data_dir: &str,
) -> PathBuf {
    if let Some(f) = explicit {
        return PathBuf::from(f);
    }
    if psp_data.starts_with(service_data_dir) {
        return Path::new(service_data_dir)
            .join("spi-share")
            .join(crate::balance_figure::FILE);
    }
    psp_data
        .join(network.name())
        .join(crate::balance_figure::FILE)
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
# then waits for it to finish syncing before it starts psp-mint. With manage = true,
# `spi install` and `spi upgrade` download Bitcoin Core from bitcoincore.org and check
# the signatures on its SHA256SUMS with gpg, unless you write `bin` (your own bitcoind, used as it is). With manage = false spi never
# installs, starts or upgrades bitcoind: it only connects to it over RPC.
# bin = \"bitcoind\"
# datadir = \"/path/to/bitcoin-datadir\"
# args = [\"-txindex\"]
# manage = true
# sync_wait_secs = 600
# min_signatures = 7          # trusted builders that must have signed SHA256SUMS (needs gpg)
# signers = [\"<40-digit fingerprint> <name of the key in guix.sigs builder-keys>\"]   # replaces spi's list
# version = \"29.1\"          # a pin for `spi upgrade` (older allowed); without it, the newest release

# The AIR server (the stock air_server), which the mints and the wallets reach
# each other through. `name` is its public key (66 hex characters): the mints'
# air.server. The server's identity file is yours to make and to back up offline.
[air]
# name = \"02...\"
# host = \"127.0.0.1\"        # where the mints dial it (always port 5702)
# bin = \"air_server\"
# manage = true              # spi runs it; false: it runs some other way
# version = \"1.0.1\"        # air_server prints no version: say what you installed
# ws_addr = \"127.0.0.1:5702\"
# health_addr = \"127.0.0.1:5701\"
# identity = \"/path/to/identity.json\"
# storage = \"/path/to/STORAGE.db\"
# tls_addr = \"0.0.0.0:443\"  # with tls_cert and tls_key
# public_name = \"air.example.com\"   # spi writes an nginx file for it

[psp]
# bind = \"127.0.0.1:3000\"
# did = \"02...\"             # `psp-mint pubkey`, for the SPI's wallet
# burn = false               # true: burn paid PSP once the SPI wallet holds burn_threshold_psp
# burn_threshold_psp = 0     # the SPI wallet's balance psp-mint keeps it at
# burn_balance_file = \"/var/lib/sp-mint/spi-share/wallet_balance.json\"   # where spi publishes that balance
# revenue_keep_days = 90     # `spi collect` removes a reissued-PSP file this many days after taking it in (0 keeps all)
# version = \"0.5.0\"        # a pin for `spi upgrade` (older allowed); without it, the newest release

[esp]
# bind = \"127.0.0.1:3100\"
# admin_bind = \"127.0.0.1:3101\"
# did = \"02...\"             # `esp-mint pubkey`, for the SPI's wallet

# The SPI's own ecash-wallet: it holds the PSP the PSP mint reissues to the SPI.
# [wallet]
# bin = \"ecash-wallet\"
# file = \"/path/to/wallet.json\"
# version = \"0.6.0\"
# Any other ecash-wallet setting goes in [wallet.settings], as for the mints.

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
# log_keep = 4                # rotated copies of a log kept (Linux); 0 keeps all
# log_max_size_mb = 50        # a log bigger than this is moved aside (Linux); 0 never moves one

# Where `spi upgrade` gets releases, and how many downloaded versions of each
# program it keeps (the installed one and the one before it are never removed;
# the oldest go first; 0 keeps every release):
# [release]
# url = \"https://api.github.com\"      # the releases of psp-mint and esp-mint
# bitcoin_url = \"https://bitcoincore.org\"   # Bitcoin Core (bitcoind) is downloaded from here
# builder_keys_url = \"https://raw.githubusercontent.com/bitcoin-core/guix.sigs/main/builder-keys\"   # the builders' public keys
# keep = 2                                  # 0 keeps every release

# Any psp-mint or esp-mint setting spi does not name goes in a settings table and
# is copied to the generated file as it is, for example:
#
# [psp.settings.settle]
# base_confirmations = 3
# retention_days = 90         # psp-mint removes finished payments from its file after this many days; 0 keeps all
# prune_horizon_days = 210    # and forgets a removed payment after this many; 0 keeps all (psp-mint checks both)
#
# [esp.settings.terms]
# maturity_days = 365
#
# [esp.settings.stream]       # the keys esp-mint publishes on AIR (esp-mint 0.11.0); all reload live
# days_ahead = 2              # days of per-service keys published ahead, 1 to 30
# record_max_bytes = 4194304  # the most bytes in one record of the stream, 1 MiB to 15 MiB
# max_services = 1000         # the most services whose keys are published, 1 to 100000
";
