//! The bitcoind connection `spi` checks: is the node reachable, is it on the
//! network the config says, and is the wallet psp-mint uses loaded (created
//! if it is not and the config allows it). Cookie authentication only, as
//! psp-mint does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

pub struct Rpc {
    pub url: String,
    pub cookie: PathBuf,
}

/// What `getblockchaininfo` says about the node.
#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    pub chain: String,
    pub blocks: u64,
    pub headers: u64,
    /// `verificationprogress`, 0 to 1.
    pub progress: f64,
    /// `initialblockdownload`.
    pub ibd: bool,
}

impl Chain {
    /// Still catching up. The node's own `initialblockdownload` flag is not
    /// enough by itself: Bitcoin Core keeps it set on a quiet chain whose last
    /// block is more than a day old, which is every fresh regtest node until
    /// its first block. So the flag counts only when the node is also behind
    /// (it knows of headers it has no block for) or reports less than 99.99%
    /// verified.
    pub fn syncing(&self) -> bool {
        self.ibd && (self.headers > self.blocks || self.progress < 0.9999)
    }

    /// Verification progress as a whole percent, rounded down (never 100
    /// while syncing).
    pub fn percent(&self) -> u32 {
        let whole = (self.progress.clamp(0.0, 1.0) * 100.0 + 1e-9).floor() as u32;
        if self.syncing() { whole.min(99) } else { whole }
    }

    pub fn describe(&self) -> String {
        if self.syncing() {
            format!(
                "syncing {}% (blocks {} of {} headers)",
                self.percent(),
                self.blocks,
                self.headers.max(self.blocks)
            )
        } else {
            format!("synced, {} blocks", self.blocks)
        }
    }
}

/// The node, as `spi` sees it. Unreachable and syncing are different things
/// and are never reported as each other.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// No answer, or the cookie cannot be read, or the answer is refused.
    Unreachable(String),
    /// Reachable, on another chain than the config says.
    WrongChain {
        found: String,
    },
    /// Reachable and still in initial block download.
    Syncing(Chain),
    Synced(Chain),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletAction {
    AlreadyLoaded,
    Loaded,
    Created,
}

fn read_cookie(path: &Path) -> Result<(String, String), String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "cannot read the bitcoind cookie {}: {e}. Is bitcoind running on this network?",
            path.display()
        )
    })?;
    let (user, pass) = text
        .trim()
        .split_once(':')
        .ok_or_else(|| format!("{} is not a cookie (user:password)", path.display()))?;
    Ok((user.to_string(), pass.to_string()))
}

impl Rpc {
    pub fn new(url: &str, cookie: &Path) -> Rpc {
        Rpc {
            url: url.trim_end_matches('/').to_string(),
            cookie: cookie.to_path_buf(),
        }
    }

    /// One JSON-RPC call, on the node or (with `wallet`) on a wallet.
    pub fn call(&self, wallet: Option<&str>, method: &str, params: Value) -> Result<Value, String> {
        let (user, pass) = read_cookie(&self.cookie)?;
        let url = match wallet {
            Some(w) => format!("{}/wallet/{w}", self.url),
            None => self.url.clone(),
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let response = client
            .post(&url)
            .basic_auth(user, Some(pass))
            .json(&json!({"jsonrpc": "1.0", "id": "spi", "method": method, "params": params}))
            .send()
            .map_err(|e| format!("cannot reach bitcoind at {}: {e}", self.url))?;
        let status = response.status().as_u16();
        let body: Value = response.json().unwrap_or(Value::Null);
        if let Some(err) = body.get("error").filter(|e| !e.is_null()) {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("an error");
            return Err(format!("bitcoind {method}: {message}"));
        }
        if status == 401 {
            return Err("bitcoind refused the cookie (401)".into());
        }
        body.get("result")
            .cloned()
            .ok_or_else(|| format!("bitcoind {method}: unexpected reply (HTTP {status})"))
    }

    pub fn chain(&self) -> Result<Chain, String> {
        let info = self.call(None, "getblockchaininfo", json!([]))?;
        let blocks = info.get("blocks").and_then(Value::as_u64).unwrap_or(0);
        Ok(Chain {
            chain: info
                .get("chain")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            blocks,
            headers: info
                .get("headers")
                .and_then(Value::as_u64)
                .unwrap_or(blocks),
            progress: info
                .get("verificationprogress")
                .and_then(Value::as_f64)
                .unwrap_or(1.0),
            ibd: info
                .get("initialblockdownload")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// The node's state against the chain the config expects (`getblockchaininfo`
    /// `chain` name: see `Network::chain`).
    pub fn node(&self, expected_chain: &str) -> Node {
        match self.chain() {
            Err(e) => Node::Unreachable(e),
            Ok(c) if c.chain != expected_chain => Node::WrongChain { found: c.chain },
            Ok(c) if c.syncing() => Node::Syncing(c),
            Ok(c) => Node::Synced(c),
        }
    }

    fn names(value: &Value) -> Vec<String> {
        value
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn loaded_wallets(&self) -> Result<Vec<String>, String> {
        Ok(Self::names(&self.call(None, "listwallets", json!([]))?))
    }

    /// Make sure `name` is loaded: loaded already, or on disk and loaded now,
    /// or (when `create` is set) created.
    pub fn ensure_wallet(&self, name: &str, create: bool) -> Result<WalletAction, String> {
        if self.loaded_wallets()?.iter().any(|w| w == name) {
            return Ok(WalletAction::AlreadyLoaded);
        }
        let dir = self.call(None, "listwalletdir", json!([]))?;
        let on_disk = dir
            .get("wallets")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .any(|w| w.get("name").and_then(Value::as_str) == Some(name))
            })
            .unwrap_or(false);
        if on_disk {
            self.call(None, "loadwallet", json!([name]))?;
            return Ok(WalletAction::Loaded);
        }
        if !create {
            return Err(format!(
                "wallet `{name}` does not exist and create_wallet is false: create it, or set create_wallet = true"
            ));
        }
        self.call(None, "createwallet", json!([name]))?;
        Ok(WalletAction::Created)
    }
}
