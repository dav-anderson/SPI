# spi

One config and one command line over `psp-mint`, `esp-mint` and `bitcoind`, and over the two programs the SPI needs beside them: the AIR server (`air_server`, the stock one) that the mints and the wallets reach each other through, and the SPI's own `ecash-wallet`. It does not link the mints or change them: it writes their config files, the AIR server's settings, the wallet's config and the files that run them as services, starts the binaries, and reads their `GET /health` and `GET /audit`. It never manages fsp-mint: an FSP mint is run through the operator control interface of the AIR service it belongs to. Version 0.4.0. Why it is built this way is in [DESIGN.md](DESIGN.md).

**spi is designed to run on a Linux system** (systemd). macOS is supported only as a development machine: the commands and the service files for it exist so that spi can be tried and tested on a Mac, and it is not meant to run a production SPI. (The code spi shares with `aso`, the part that downloads and upgrades programs, schedules an upgrade and keeps logs under a cap, is the [air-minterface](https://github.com/dav-anderson/air-minterface) crate.)

**Nothing stops a mint except an upgrade, a setting that needs a restart, and `spi stop`.** An upgrade (of a mint, of air_server or of bitcoind) and a change to a setting marked "needs a restart" are a **brief downtime**: the old process stops, then the new one starts. spi says so, with the reason, **warns that it will shut down the AIR service and every mint it runs, and asks you to confirm before it changes anything** (`spi upgrade`, `spi restart`); it can also do an upgrade later, at a time you choose (`spi upgrade --at 03:00`, `--in 6h`). Upgrades with no downtime are not built (deferred by decision, 2026-10-23). Everything else leaves the mints running: `spi reload` (the settings marked "reloads live"), `status`, `audit`, `setup`, `collect`, `versions`, `upgrade check`, `snapshot` and `publish-balance` only read or signal; the payout is esp-mint's own `pay`, which goes through the running server. `spi stop` is for trying spi out on a machine that has no services and for taking an SPI down for good, and nothing touches fsp-mint. A restart after a crash (the service's `Restart=on-failure`, or `HEALTH_ACTION=restart`) is fault recovery and stays.

## Getting the binaries

A machine needs six programs: `spi`, `psp-mint`, `esp-mint`, `bitcoind`, `air_server` and `ecash-wallet`. Nothing else needs to be cloned or built: the service definitions are inside the `spi` binary (see "Running as services"). The Rust ones are installed with `cargo` (get it from <https://rustup.rs>), straight from the git tags:

```
cargo install --git https://github.com/dav-anderson/psp-mint.git --tag v0.9.0
cargo install --git https://github.com/dav-anderson/esp-mint.git --tag v0.11.0
cargo install --git https://github.com/dav-anderson/ecash-wallet.git --tag v0.9.0
cargo install --git https://github.com/dav-anderson/SPI.git --tag v0.4.0
```

Those are the newest tags that exist today, and **none of them has the work this README describes**: the AIR server, the SPI's wallet, the reload, the snapshot, the burn rule and `spi upgrade` came after them. Until the next tag of each repository is cut, build from a checkout of the branch that has them (`git clone https://github.com/dav-anderson/<repo>.git`, then `cargo install --path .` in it, for `SPI`, `psp-mint`, `esp-mint` and `ecash-wallet`); the release notes of each repository (`CHANGELOG.md`) say which tag has what. Each puts its binary in `~/.cargo/bin`. **esp-mint 0.5.0 is the first release that gives `spi audit` the matured / unmatured split**; an older one still works and `audit` says it has no split.

`bitcoind` is Bitcoin Core, and **spi downloads it**: with `[bitcoind] manage = true` (the default) `spi install` and `spi upgrade` fetch the release for this OS and CPU from bitcoincore.org (the newest, or the `version` you pin), check the **signatures** on the release's `SHA256SUMS` and then the archive's SHA-256 against it, and install it as they do our programs. They need `curl` and **`gpg`**; `spi install` says if either is missing, and without gpg spi refuses to download bitcoind (Debian and Ubuntu: `sudo apt install gnupg`; macOS: `brew install gnupg`). The signature check is always on and has no opt-out. A checksum from the same server as the file does not stop a hijacked download; the signatures do. spi imports the builders' public keys (`builder-keys/<name>.gpg` of `github.com/bitcoin-core/guix.sigs`) into a private keyring, `<spi folder>/gnupg` (mode 0700), and counts a good signature only when the key's **primary fingerprint** is on the list compiled into spi (18 builders who signed Bitcoin Core 29.2, 30.3, 31.0 and 31.1; `[bitcoind] signers` replaces it), each builder once. At least `[bitcoind] min_signatures` (default 7) must have signed; any bad signature fails the download; a signature by a key spi does not have, or by a revoked key, is not counted; a key that has since expired still counts. A release candidate (a folder with no `SHA256SUMS`, or a version with letters in it) is never the newest and is refused as a pin. A bitcoind of your own is the override: write `[bitcoind] bin` and spi installs that file as it is and never downloads. With `manage = false` spi never installs, starts or upgrades bitcoind and only connects to it over RPC, as for a node that is already set up and syncing. To hand such a node to spi later, set `manage = true`, point `datadir` at the existing chain data and leave `bin` out: no new sync is needed. `air_server` is the stock AIR server, <https://github.com/ramp-stack/air_server>, tag `v1.0.1` (commit `efef621`). There is no install command here, because none was checked: its `deploy/README.md` says it is compiled from a checkout with the checked-in `Cargo.lock` (`cargo build --release --locked` is the ordinary way to do that; the binary is `target/release/air_server`) and installed under a new file name that is then renamed into place. Put the binary on `PATH` (or name it in `spi.toml`, `[air] bin`). spi does not change it and does not build it, and `air_server` prints no version, so `spi install` records which file it installed (its hash) and `[air] version` says what you meant to install (see "Versions"). `spi` looks for each of the programs on `PATH` (or at the path given in `spi.toml`), and `spi install` copies what it finds (all but `ecash-wallet`, which spi runs as you) to `/usr/local/bin` for the services, because a service user can not run programs out of someone's home folder.

## The config

One file, `spi.toml`, in `~/.spi` (`SPI_DIR`, or `--dir`; `SPI_CONFIG` or `--config` name the file). `spi install` writes a commented starter if there is none. Strongest first: the environment, the file, the default. `spi config show` prints every setting with its value and where it came from. An unknown key or a bad value stops with the list of every problem.

| Setting (`spi.toml`) | Environment | Default | What it does |
|---|---|---|---|
| `network` | `SPI_NETWORK` (or `--network`) | `regtest` | `main`, `regtest`, `signet` or `testnet`. **Regtest unless you write another one down.** Main is never a default. |
| `[bitcoind] rpc_url` | `SPI_BITCOIND_RPC_URL` | per network (`http://127.0.0.1:18443` on regtest) | bitcoind's RPC address. |
| `[bitcoind] cookie` | `SPI_BITCOIND_COOKIE` | per network and OS, as psp-mint's | The RPC cookie file. Cookie authentication only. |
| `[bitcoind] wallet` | `SPI_BITCOIND_WALLET` | `psp-<network>` | The wallet psp-mint uses. |
| `[bitcoind] create_wallet` | | `true` | `install` creates the wallet if it does not exist. |
| `[bitcoind] bin` | `SPI_BITCOIND_BIN` | none (`start` runs `bitcoind` from `PATH`) | **An override.** Written down, it is your own bitcoind: `install` and `upgrade` use that file as it is and never download. Left out with `manage = true`, `install` and `upgrade` download Bitcoin Core from bitcoincore.org (the service runs the download; `spi start`, the try-out, still runs `bitcoind` from `PATH`). |
| `[bitcoind] datadir` | `SPI_BITCOIND_DATADIR` | none; in the service, `<data_dir>/bitcoind` | Passed as `-datadir=`. |
| `[bitcoind] args` | | none | Extra arguments for `bitcoind` (`["-txindex"]`). |
| `[bitcoind] manage` | | `true` | `start` starts bitcoind when it is not answering, and `install` writes a service for it. `false`: spi never installs, starts or upgrades bitcoind, writes no service for it and only connects to it over RPC (use this for a bitcoind that is already set up). |
| `[bitcoind] sync_wait_secs` | | `600` | How long `start` waits for bitcoind to finish syncing before it gives up. 0 to 604800. |
| `[bitcoind] version` | | none | **A pin, optional.** With a version here `spi upgrade` installs exactly that one, even when a newer release exists (and says so: pins are never silent); without one it installs the newest release. Two or three numbers (`29.1`). See "Upgrading". |
| `[air] name` | `SPI_AIR_NAME` | none | The AIR server's public key (66 hex characters). It becomes the mints' `air.server` and the wallet's. **Without it neither mint can start**, and `status` and `install` say so. |
| `[air] host` | `SPI_AIR_HOST` | `127.0.0.1` | Where the mints and the wallet dial the server: the mints' `air.host`. They always use port 5702 of it. |
| `[air] manage` | | `true` | spi runs the AIR server (`start`, a service). `false`: it runs some other way and spi only reads its health. |
| `[air] bin` | `SPI_AIR_BIN` | `air_server` | The binary. |
| `[air] ws_addr`, `health_addr` | `SPI_AIR_WS_ADDR`, `SPI_AIR_HEALTH_ADDR` | `127.0.0.1:5702`, `127.0.0.1:5701` | `AIR_WS_ADDR` and `AIR_HEALTH_ADDR` of the server. spi warns when the mints would dial a port that is not 5702. |
| `[air] identity`, `storage` | | `<spi folder>/air/identity.json`, `.../air/STORAGE.db` (in the service, under `data_dir`) | `AIR_SECRET_FILE` and `AIR_STORAGE`. **The identity is yours to make and to keep an offline copy of**: spi does neither, and losing it changes the server's name. |
| `[air] tls_addr`, `tls_cert`, `tls_key` | | none | The server's own TLS listener (`AIR_TLS_ADDR`, `AIR_TLS_CERT`, `AIR_TLS_KEY`); all three or none. A port below 1024 needs more than the service user has: use nginx (next row). |
| `[air] public_name`, `public_cert`, `public_key` | | none | With all three, `install` writes `nginx-air-server.conf` into the service folder: nginx answers on `public_name` and forwards to `ws_addr`. spi does not install it. |
| `[air] fee_read`, `fee_write` | | `0` | Refused unless 0: stock air_server 1.0.1 has no fee hook (upstream ROADMAP item 1, the air_server fee hook). They are there so the setting has one named place when the hook exists. |
| `[air] version` | | none | What you installed (`x.y.z`). air_server publishes no releases upstream, so this is your word for the binary `[air] bin` names; `spi upgrade` installs that binary and records it. |
| `[psp.settings.settle] retention_days`, `prune_horizon_days` | | `90`, `210` (psp-mint's own) | How long psp-mint keeps a finished payment in `settlements.json` (days after it finished), and how long it then remembers the payment's txid (days after its block). **0 keeps everything** for either. psp-mint prunes its own records and checks both values (the horizon must be 0 or at least twice the retention plus 30); spi builds nothing for it, copies them as written, and `config show` names them (`psp.settings.settle.retention_days`, with `psp-mint's own` when they are not set). See psp-mint's README, "Retention and the horizon". |
| `[esp.settings.stream] days_ahead`, `record_max_bytes`, `max_services` | | `2`, `4194304`, `1000` (esp-mint's own) | The keys esp-mint publishes on AIR (esp-mint 0.11.0), so that a wallet can check the keys it is signed with: how many days of per-service keys are published ahead (1 to 30), the most bytes in one record of the stream (1 MiB to 15 MiB; past it the services are split into buckets by a hash of the service ID), and the most services whose keys are published (1 to 100,000; once esp-mint knows that many, an exchange for a new service is refused). **They reload live.** A day costs 2,145 bytes for each service in every record, so 1,000 services are 2.1 MB a day for each key set. esp-mint checks the values; spi builds nothing for them, copies them as written, and `config show` names them (`esp.settings.stream.days_ahead`, with `esp-mint's own` when they are not set). See esp-mint's README, "The key stream". |
| `[psp] burn`, `burn_threshold_psp` | | `false`, `0` | See "The PSP the SPI is paid". Off: every PSP paid to the SPI is reissued to its wallet. With burn on, the threshold is measured against the SPI wallet's current PSP balance. |
| `[psp] burn_balance_file` | | `wallet_balance.json` in psp-mint's network folder; `<data_dir>/spi-share/wallet_balance.json` for a service | Where `spi collect` and `spi audit` publish the wallet's PSP balance, and where psp-mint reads it (`burn.balance_file`). Only used with `burn = true`. How old the figure may be is psp-mint's `burn.balance_max_age_secs` (86400 s, 24 hours; a setting that reloads live, under `[psp.settings.burn]`). There is no timer (see "The PSP the SPI is paid"). |
| `[psp] did`, `[esp] did` | | read from the mint's `mint_did_key.json` | The mints' DIDs, for the wallet's `air.psp_did` and `air.esp_did`. Write them here if spi can not read the file. |
| `[psp] version`, `[esp] version` | | none | **A pin, optional**, as `[bitcoind] version`: the exact version `spi upgrade` installs; without one, the newest release. `spi upgrade --reset-pins` removes the line (a backup is kept). |
| `[psp] data_dir` | | the mint's own | Where psp-mint's data is, for the revenue folder. |
| `[wallet] bin`, `file` | `SPI_WALLET_BIN`, `SPI_WALLET_FILE` | `ecash-wallet`, `<spi folder>/wallet/wallet.json` | The SPI's own ecash-wallet. |
| `[wallet] version` | | none | What `spi versions` expects. |
| `[psp] bind`, `[esp] bind`, `[esp] admin_bind` | `SPI_PSP_BIND`, `SPI_ESP_BIND`, `SPI_ESP_ADMIN_BIND` | `127.0.0.1:3000`, `:3100`, `:3101` | The addresses of the mints. The ESP mint's PSP URL follows `[psp] bind`. |
| `[psp] bin`, `[esp] bin` | `SPI_PSP_BIN`, `SPI_ESP_BIN` | `psp-mint`, `esp-mint` | The mints' binaries. |
| `[service] user`, `group` | `SPI_SERVICE_USER`, `SPI_SERVICE_GROUP` | `spmint` (Linux), `_spmint` (macOS); the group is the user's | The account the services run as: no login, no password, owns the data and the logs. Lowercase letters, digits, `_` and `-`. |
| `[service] bin_dir`, `etc_dir`, `data_dir`, `log_dir` | `SPI_SERVICE_BIN_DIR`, `..._ETC_DIR`, `..._DATA_DIR`, `..._LOG_DIR` | `/usr/local/bin`, `/etc/sp-mint`, `/var/lib/sp-mint`, `/var/log/sp-mint` | Where the services find their binaries, settings and env files, data and logs. Absolute paths. |
| `[service] health` | | `true` | Whether a health monitor is installed for each mint. |
| `[service] log_keep`, `log_max_size_mb` | | `4`, `50` | How many rotated copies of each log are kept (0 to 52), and the size in MB (0 to 10240) past which a log is moved aside. **0 keeps everything**: `log_keep = 0` keeps every rotated copy, and `log_max_size_mb = 0` never moves a log aside. Every service log is held to this on Linux (see "Logs" under "Running as services"). No effect on macOS, which is a development machine. |
| `[release] url`, `bitcoin_url` | | `https://api.github.com`, `https://bitcoincore.org` | Where `spi upgrade` lists and downloads releases: the GitHub API for psp-mint and esp-mint, and Bitcoin Core's site. Must start with `http://` or `https://`. |
| `[bitcoind] min_signatures`, `signers` | | `7`, the 18 builders compiled into spi | How many trusted builders must have signed Bitcoin Core's `SHA256SUMS` (1 to 100, and no more than the signers), and, optionally, a list that replaces the builders: each entry is `"<40 hex digits of the primary key fingerprint> <name of the key in guix.sigs builder-keys>"`. |
| `[release] builder_keys_url` | | `https://raw.githubusercontent.com/bitcoin-core/guix.sigs/main/builder-keys` | Where each builder's `<name>.gpg` is. Only the fingerprints compiled into spi (or `signers`) count, so a wrong key file cannot add a signer. |
| `[psp] revenue_keep_days` | | `90` | `spi collect` removes a reissued-PSP file this many days after it was taken in (by the file's age) and drops its line from `wallet/received.txt`; 0 keeps every file. A file not yet taken in is never removed. |
| `[release] keep` | | `2` | Downloaded versions kept per program under `<spi folder>/releases/` (0 to 100). After a successful upgrade the oldest ones are removed first, but never the installed version or the one it replaced. **0 keeps every release.** |

**Any other mint setting** goes under `[psp.settings]` or `[esp.settings]` (and any other wallet setting under `[wallet.settings]`), in the program's own sections, and is copied to the generated file as it is:

```toml
[psp.settings.settle]
base_confirmations = 3
retention_days = 120
prune_horizon_days = 400

[esp.settings.terms]
maturity_days = 365
```

A key that spi sets itself (`bitcoin.network`, `bitcoin.rpc_url`, `bitcoin.wallet`, `bitcoin.cookie`, `server.bind`, `air.server`, `air.host`, `burn.enabled`, `burn.threshold_psp` and `burn.balance_file` for psp-mint; `server.bind`, `server.admin_bind`, `server.network`, `psp.url`, `air.server` and `air.host` for esp-mint; `air.server`, `air.host`, `air.psp_did` and `air.esp_did` for the wallet) cannot also be passed through: that would be two places for one value, and `install` refuses it, naming the key. The mints' own environment variables (`PSP_...`, `ESP_...`) still win over the generated files, as they do for any mint; `ESP_ADMIN_TOKEN` is read from spi's environment, not from any file.

## Commands

| Command | What it does |
|---|---|
| `spi install [--service-files DIR \| --no-service-files] [--ops DIR]` | Makes the data folders under the spi folder, writes `generated/psp-mint.toml`, `generated/esp-mint.toml` and `generated/ecash-wallet.toml` (each starts with "Generated by spi. Don't edit"), checks that bitcoind is reachable, on the right chain, and that its wallet is loaded (loads it from disk, or creates it). A bitcoind on the wrong chain fails the command. **It also writes everything that runs the mints as services** (see "Running as services"), into `<spi folder>/service` or `DIR`, and prints the `sudo` steps and a `Next: spi setup` line; it installs nothing itself, and stops nothing. A bitcoind that is not running yet is not a failure while service files are being written and spi manages bitcoind, because the service starts it; with `--no-service-files`, or `manage = false`, an unreachable bitcoind fails the command after the files are written. A bitcoind that is still syncing does not: it is reported, and the wallet is still set up. `--ops DIR` replaces any embedded definition with the file of the same name from `DIR`. |
| `spi start` | **For trying spi out; a machine with services uses them.** Writes the config files again. Refuses to start if `[air] name` is not set or the AIR server's identity file is missing. Starts bitcoind if it is not answering (and `manage` allows it; in the foreground, its console in `logs/bitcoind.log`). Before it opens a program's log, it moves one over `[service] log_max_size_mb` aside as `<name>.log.<epoch>.bak` and keeps the newest `log_keep` of those. **Waits for bitcoind to finish syncing.** Then starts the AIR server, psp-mint and esp-mint, each as a background process of the current user, and waits for `/health`. |
| `spi stop` | **Stops the mints: for a try-out and for decommission only.** Stops esp-mint, psp-mint and the AIR server (SIGTERM), then bitcoind only if `spi start` started it. |
| `spi reload` | Writes the config files again and sends psp-mint and esp-mint SIGHUP, so they read them again **without stopping**. A setting that reloads live applies at once. For each setting marked "needs a restart" that you changed, it prints the old and the new value, says it waits for a restart and how to do one, and restarts nothing. See "Changing settings". |
| `spi restart psp-mint\|esp-mint` | Restarts one mint through the service manager (a brief downtime), so a setting that needs a restart takes effect. It **warns and asks you to confirm first**, restarts only that mint, and waits for its `/health`. |
| `spi snapshot` | Asks psp-mint and esp-mint for a live snapshot of their data (SIGUSR1 through the service manager). They keep serving; each one's log says where the copy is. |
| `spi publish-balance` | Publishes the SPI wallet's PSP balance for psp-mint's burn threshold (by hand; `spi collect` and `spi audit` already do it). Does nothing with `burn = false`. |
| `spi status [--json]` (also `health`) | See "Status". Exit 0 only when everything is ok. |
| `spi setup` | The steps to a working SPI, which are done and what to run next. Starts nothing. See "Setup". |
| `spi collect` | Takes the PSP psp-mint reissued to the SPI into the SPI's wallet. See "The PSP the SPI is paid". |
| `spi versions [--json]` | The version of every program and library. See "Versions". |
| `spi upgrade [NAME...] [--dry-run] [--at HH:MM \| --in DURATION] [--keep-pins \| --reset-pins]` | Brings `bitcoind`, `air_server`, `psp-mint` and `esp-mint` (or the ones named) to their target version, one at a time, each by a stop and a start (a brief downtime). **It warns that it will shut down the AIR service and every mint it runs, and asks you to confirm before it changes anything**; `--dry-run` asks nothing and changes nothing. `--at` / `--in` schedule it. See "Upgrading". |
| `spi upgrade check [--json]` | One line per program: installed, the `version` in spi.toml, the newest release, and whether an upgrade is available. Reads only. |
| `spi upgrade status` / `spi upgrade cancel` | Shows, or cancels, the one scheduled upgrade. |
| `spi config show` | The settings in use, where each came from, and the settings passed through. |
| `spi audit [--json]` | See "Audit". Reads the mints and the SPI's wallet; stops nothing. |
| `spi --version` | spi's version and what it was built with. |

A flag a command does not take is refused, naming it, with exit status 2 and before anything is read or written; global flags (`--dir`, `--config`, `--network`) go before the command and each needs a value.

For a machine that starts the mints at boot and restarts them, use the services that `install` writes, not `start`; see "Running as services".

## Running as services

`spi install` is the bootstrap for a machine that starts the mints at boot, starts them again when they fail, runs them as a user that is not root, and watches them. On a fresh Linux (systemd) or macOS (launchd) machine that has the programs from "Getting the binaries", it needs nothing else: not a checkout of any other repository, and no folder of definitions. With no `spi.toml` it writes a starter one (regtest) and carries on.

**What it writes**, all from `spi.toml`, into `<spi folder>/service` (`~/.spi/service`) or the folder after `--service-files`:

- for the AIR server (unless `[air] manage` is `false`): `air-server.env` (its `AIR_*` settings, from `[air]`), its unit (`sp-air-server.service`) or plist (`org.servicepoints.air-server`), a health monitor for it, and, with `[air] public_name`, `nginx-air-server.conf`;
- for psp-mint and esp-mint each: its settings file `<mint>.toml` (the generated one, except that the cookie is the service's own), its env file `<mint>.env` (the data folder, the settings file, the network) and an empty `<mint>.secret.env` for secrets;
- the systemd units (Linux) or launchd property lists (macOS): one for each mint, one for bitcoind unless `[bitcoind] manage` is `false` (`sp-bitcoind.service` on Linux, so that it can not replace a `bitcoind.service` from a package), one for the AIR server, and a health monitor for each mint unless `[service] health` is `false`, with `sp-health-monitor.sh` and a settings file `<mint>-health.conf` for each;
- on Linux, the job that keeps the logs under their size cap: `sp-logs.service` and `sp-logs.timer`;
- `install-system.sh`, the script that puts all of it in place as root.

**The definitions are generated from `spi.toml`, not copied.** The templates are in [`ops/`](ops/) in this repository and are compiled into the binary, so the binary alone is enough. The binary folder, the user and group, the data, log and settings folders, the bitcoind data folder and arguments, the chain, the RPC port and the mints' addresses are filled in from `spi.toml`, so changing `[service]` changes every file that mentions it. An unknown or unclosed placeholder stops `install` with an error that names it. A path, user name or bitcoind argument that holds a space, a quote, `%` or `$` is refused with the setting named, because it would need a different escape in systemd, launchd and sh. `--ops DIR` (or `SPI_OPS_DIR`) is optional: a file in `DIR` named as spi names it (`systemd/psp-mint.service`, `systemd/sp-bitcoind.service`, `launchd/org.servicepoints.psp-mint.plist`, `sp-health-monitor.sh`, ...) replaces the embedded one, with the same placeholders; any it does not have comes from the binary.

**What it does not do: run anything as root.** `spi install` stays an ordinary user's command. It prints the exact steps and leaves the root part to a script you can read first, rather than doing it behind a `--system` flag. DESIGN.md has the reasons.

### The steps

On Linux, on a machine with `psp-mint`, `esp-mint`, `bitcoind`, `air_server` and `ecash-wallet` on `PATH`, and the AIR server's identity file made and backed up (see `[air] identity`):

```
spi install
sudo sh ~/.spi/service/install-system.sh
sudo systemctl enable --now sp-bitcoind.service
spi install --no-service-files
sudo systemctl enable --now sp-air-server.service
sudo systemctl enable --now psp-mint.service esp-mint.service sp-air-server-health.service psp-mint-health.service esp-mint-health.service
spi setup
```

The first `spi install` finds no bitcoind running: that is expected here and not a failure, because the service it just described starts it. Set `[air] name` to the AIR server's public key before the mints are started: until it is set, the generated mint files have no `air.server`, and psp-mint refuses to start (`install` warns). The script creates the account and the folders, installs the four binaries it found on `PATH` into `bin_dir`, installs the files and the definitions, and starts nothing. Once bitcoind answers, the second `spi install --no-service-files` creates the wallet psp-mint uses; then the mints are started. Put `ESP_ADMIN_TOKEN` in `/etc/sp-mint/esp-mint.secret.env` before you start esp-mint if you use the operator routes (see "The files" for why it is a file of its own). On macOS the script is the same and the starting lines are `sudo launchctl bootstrap system /Library/LaunchDaemons/org.servicepoints.<name>.plist` for `bitcoind`, then (after the wallet step) `air-server`, `psp-mint`, `esp-mint`, and the health monitors. `spi install` prints the exact lines for the system it ran on.

Bitcoin Core needs its initial block download before psp-mint has anything to scan, which takes days on main. The services do not wait for it: psp-mint is started after bitcoind (systemd orders them with `After=`, which means bitcoind has been started, not that its RPC is answering yet; launchd has no ordering at all), so psp-mint can start before the node answers, and what it does then is psp-mint's own (the service manager starts it again if it exits). `spi status` reports it as **not ready** while the node is syncing or unreachable. Unlike `spi start`, the service manager does not hold psp-mint back until the sync is finished; if you need that, start the mints yourself when `spi status` says `ok`.

**Running spi against the service bitcoind.** The service bitcoind keeps its data, and so its RPC cookie, under `data_dir` (`/var/lib/sp-mint/bitcoind/<chain>/.cookie`), owned by the service user; psp-mint, which runs as the same user, reads it without any change of permissions. bitcoind is started with `-rpccookieperms=group`, so the cookie is also readable by the service group. To run `spi install` and `spi status` as yourself, add yourself to that group and name the cookie in `spi.toml`; `spi install` prints this with the exact path:

```
sudo usermod -aG spmint "$USER"
```

On macOS: `sudo dscl . -append /Groups/_spmint GroupMembership "$USER"`. Then log in again and set `cookie = "/var/lib/sp-mint/bitcoind/regtest/.cookie"` under `[bitcoind]` (the chain's own folder, as `install` prints). The cookie is the node's RPC password, so it lets whoever reads it use the node's wallet: add only people who may. If you set `bitcoind.cookie` to somewhere else, psp-mint reads it there, and `install` warns when that is not where the service bitcoind writes it. If `bitcoind.manage` is `false`, the cookie is whatever you point it at, and a cookie in a home folder gets a warning (the service user can not read it, and the unit then uses `ProtectHome=read-only`).

### The layout

| What | Where (the defaults of `[service]`) |
|---|---|
| The binaries | `/usr/local/bin/psp-mint`, `esp-mint`, `bitcoind`, `air_server` and `sp-health-monitor.sh`: copied by the script from where `spi` found them. The units run these, not `~/.cargo/bin`: a service user can not read a home folder, and `ProtectHome` hides it. |
| The user | `spmint` on Linux, `_spmint` on macOS: no login, no password, owns the data and the logs. One account for all services, so that psp-mint reads bitcoind's cookie. |
| A mint's data | `/var/lib/sp-mint/<mint>` (`PSP_DATA_DIR`, `ESP_DATA_DIR`). bitcoind's is `/var/lib/sp-mint/bitcoind` unless `[bitcoind] datadir` is set. |
| A mint's settings | `/etc/sp-mint/<mint>.toml` (`PSP_CONFIG`, `ESP_CONFIG`): generated, root:group, mode 0640. |
| The env file | `/etc/sp-mint/<mint>.env`: generated, root:group, 0640. The data folder, the settings file, the network. |
| The secrets file | `/etc/sp-mint/<mint>.secret.env`: root:group, 0640. Written once, empty, and **never overwritten**: put `ESP_ADMIN_TOKEN` and any other secret here, not in `spi.toml`. Read after the env file, so it wins. |
| The monitor's settings | `/etc/sp-mint/<mint>-health.conf`: root, 0644. Written once and **never overwritten**; edit it freely. |
| The AIR server | `/etc/sp-mint/air-server.env` (generated, root:group, 0640) and its data under `/var/lib/sp-mint/air-server` unless `[air] storage` says otherwise. Its identity file is where `[air] identity` says. |
| Logs | `/var/log/sp-mint/<mint>.log`, `<mint>-health.log`, `air_server.log`, `spi-upgrade.log` and `bitcoind.log` (spi runs bitcoind with `-nodebuglogfile -printtoconsole`, so its whole log is `bitcoind.log` here, under the same limits as the others: no log grows forever; an old `debug.log` left in its data folder can be deleted. With `[bitcoind] manage = false` spi does not start bitcoind: add those two flags to your own bitcoind, or its `debug.log`, trimmed only when it starts, grows without limit. macOS is a development machine: its service logs are not rotated and are cleared by hand between tests). |
| The definitions | `/etc/systemd/system/<mint>.service`, `<mint>-health.service`, `sp-bitcoind.service`, `sp-air-server.service`, `sp-air-server-health.service`; `/Library/LaunchDaemons/org.servicepoints.<mint>.plist`, `...<mint>-health.plist`, `org.servicepoints.bitcoind.plist`, `org.servicepoints.air-server.plist`. |

**The files spi writes and the files that are yours.** Everything generated from `spi.toml` (settings, env files, definitions, the monitor script, the binaries) is replaced each time the script runs, so a change in `spi.toml` reaches the system. A person edits only the two files that are never overwritten: the secrets file and the monitor settings. The monitor's address, name and restart command are not in its settings file: the definition sets them from `spi.toml`, and they win over the file. For a manual check, `HEALTH_URL=http://127.0.0.1:3000/health sp-health-monitor.sh --once` polls once and exits 0 or 1.

Start order is bitcoind, the AIR server, psp-mint, then esp-mint (each needs the one before). systemd orders them with `After=`. The mints do not depend on it: esp-mint waits for the PSP at startup and asks again every `psp.startup_retry_secs` (5 by default) for as long as it takes.

### Changing settings

Every setting of a mint is one of two kinds, and each mint's README marks each one: **reloads live** or **needs a restart**.

Edit `spi.toml`, run `spi install` (it writes the same folder again) and the script again, then run `spi reload`, or on a service `sudo systemctl reload psp-mint.service esp-mint.service` (on macOS `sudo launchctl kill HUP system/org.servicepoints.psp-mint`, and the same for esp-mint). Each mint reads its settings again **without stopping**, applies the live ones for the next request or scan, and **keeps the running value of any that needs a restart**. `spi reload` names each of those that you changed (key, old value, new value), says that it waits for a restart and how to do one, and restarts nothing:

```
psp-mint: 1 setting changed that needs a restart (a brief downtime), so the running mint keeps the old value until it is restarted; this reload restarts nothing:
  rotation.period_hours: 720 -> 1440
psp-mint: to apply it, run `spi restart psp-mint` (a mint started with `spi start`: `spi stop`, then `spi start`). BRIEF DOWNTIME: restart. One process holds ...
```

`spi restart psp-mint` (or `esp-mint`) is that restart: it warns, asks you to confirm, restarts that one mint and waits for its `/health`. It is a brief downtime because one process holds the mint's data (sled's exclusive lock), so the new process can start only after the old one has stopped; wallets' AIR requests wait in the mailbox slots on the AIR server, which stores them with no expiry, and are answered when the mint is back.

**psp-mint, reloads live:** `server.melt_max_request_bytes`, `server.rollover_max_request_bytes`, `server.burns_max_request_bytes`, `server.check_state_max_request_bytes`, `rollover.max_items`, `bitcoin.rpc_url`, `bitcoin.cookie`, `bitcoin.rpc_timeout_secs`, `scan.interval_secs`, `scan.health_intervals`, `scan.health_grace_secs`, `price.*`, `settle.*` (so `settle.retention_days` and `settle.prune_horizon_days`, which psp-mint checks), `claim.*`, `delivery.read_fee` and `burn.*`. Each is read at the next request or scan.

**psp-mint, needs a restart**, and why: `server.bind` (the API's listening socket is bound once, when the mint starts), `bitcoin.network` and `bitcoin.wallet` (the chain and the wallet the data belong to are fixed when the mint opens), `rotation.activation_hours` and `rotation.period_hours` (the key schedule is worked out when the mint opens), `storage.mint_state` (a setting only: nothing reads it yet), and `air.server`, `air.host`, `air.held_reads` and `air.timeout_secs` (the AIR connection and its listener are built when the mint opens).

**esp-mint, reloads live:** `psp.timeout_secs`, `psp.startup_retry_secs`, `terms.maturity_days`, `stream.*` (`days_ahead`, `record_max_bytes`, `max_services`), `exchange.*`, `transfer.*`, `rollover.*`, `redeem.*` and `queue.max_payment_ref_bytes`.

**esp-mint, needs a restart**, and why: `server.network` (checked against the PSP when the mint starts), `server.bind` and `server.admin_bind` (listening sockets, bound once), `psp.url` (the PSP client is built when the mint starts), `admin.max_request_bytes` (the operator listener's limit is set when it is built), `rotation.activation_hours` and `rotation.period_hours`, `storage.mint_state`, and `air.server`, `air.host`, `air.held_reads` and `air.timeout_secs`, for the same reasons as psp-mint. Each mint's DESIGN.md gives the reason for every setting. spi keeps its own list of these keys (`RESTART_ONLY_PSP` and `RESTART_ONLY_ESP` in `src/run.rs`) to name them in `spi reload`; **it must be kept in step with the mints by hand**, and a mint's own SIGHUP log names the same settings.

A file that does not parse changes nothing and is logged. If a definition changed (the script says), `sudo systemctl daemon-reload` first on Linux (the script does); on macOS launchd reads a property list only when it loads it. Mind the data: `data_dir` holds each mint's master secret and every note's history, and no step here removes it.

### Upgrading

`spi upgrade [psp-mint] [esp-mint] [bitcoind] [air_server] [--dry-run]` brings the named programs (all four with no name), one at a time in the order bitcoind, air_server, psp-mint, esp-mint, to their target version. It works on the service install (`spi install` and its script), so run it as root (`sudo spi upgrade`).

**It warns and asks first.** Before it changes anything it prints

> WARNING: spi upgrade shuts down the AIR service and every mint it runs (psp-mint, esp-mint), one program at a time, each for a brief downtime. Nothing is changed unless you confirm.

then the plan (which programs, from which version to which, and the method for each), and asks you to type `yes`. Anything else, and a run with no terminal, changes nothing: `bin_dir` is untouched and no service command is run. `--dry-run` prints the warning and the plan and asks nothing. A program that is already at its target is left alone, and if every program is, nothing is stopped and nothing is asked.

**What it does to each program:**

1. It works out the target (below) and gets the build: a release binary downloaded and checked against the SHA-256 the release publishes (a download that fails is deleted and nothing is changed), or, for a program with no release to download, the build `bin` in spi.toml names. Either way the new binary's `--version` must print the target before anything is changed. spi never builds from source.
2. It copies the build beside the installed one, swaps it in and keeps the old one as `<name>.previous`.
3. It restarts the service (`systemctl restart`, or `launchctl kickstart -k`) and waits for it.
4. **For a mint, `/health` must report the `version` that was installed** before spi says "now x.y.z, healthy". A mint that comes back healthy but on the old version is not a success.
5. If the program does not become healthy on the target, it puts the old binary back, restarts it, **stops there** and says so. It never goes on to the next program after a failure.

**The brief downtime, as each line says it:**

- **psp-mint, esp-mint**: `BRIEF DOWNTIME: restart. One process holds the mint's data (sled's exclusive lock), so the new build starts only after the old one has stopped; wallets' AIR requests wait in the mailbox slots and are answered when it is back.` A mint that is not running is started by the restart, and the line says so. The service's stop timeout is 120 s: the worst case for a mint to stop is the scan's RPC call (30 s), the AIR listener's slot (3 reads of 10 s = 30 s), the API drain (30 s), the runtime's shutdown (5 s) and the watch (1 s), 96 s, and 120 s leaves room for the data to flush. The sum is in the unit's comment.
- **bitcoind**: restart; the mints keep serving while it is down (psp-mint's scan waits for the node).
- **air_server 1.0.1**: a brief downtime, and the line says why (stock air_server cannot be replaced without a stop; file and line are in the text of the line and in DESIGN.md). Nothing is lost: it stores a request before it answers, and a wallet's request waiting in its mailbox slot is still there afterwards; the mints' AIR clients reconnect.

**The target.** `version` in spi.toml is an optional **pin**. With no pin the target is the newest tagged release; with a pin it is exactly the pin. Pre-release tags are not releases. When spi installs a downloaded release it records it (`<spi folder>/installed/<program>.json`: version and SHA-256). **spi only downloads release binaries and never builds.** (`spi install` still copies the binaries it finds on `PATH` or at `bin`; having it download the newest release as well is part of what is not built, below.) air_server publishes no releases, so its `version` and `bin` are yours, as before.

**Pins are never silent.** When a program is pinned to a version older than the newest release (or the newest cannot be found), `spi upgrade`, `spi upgrade check` and `spi upgrade status` all say so:

```
WARNING: these programs are pinned and are not at the newest release:
  psp-mint: pinned at 0.6.0 (newest 0.7.0)
`spi upgrade` installs the pin unless it is reset. ...
```

`spi upgrade` asks about each such pin: keep it, or reset it and install the newest. `--keep-pins` keeps them all; `--reset-pins` resets them all; with no terminal the pins are kept and it says so. A reset removes the `version` line of that program from spi.toml, keeping the rest and the comments, **after copying the file to `spi.toml.<time>.bak`** (the newest 5 copies are kept, older ones removed), and only after you confirmed the upgrade. A pin set in the environment is yours to unset.

**Scheduling.** `--at HH:MM` (the next time this machine's clock shows it) or `--in 6h` (also `90m`, `1d`, `1h30m`) schedules the upgrade instead of running it. The warning and the plan are shown and **confirmed now**; the scheduled run asks nothing. It prints, for example, `Upgrade scheduled for Sat Oct 24 03:00 EDT (in 15h 28m)`. One upgrade can be pending at a time (a second is refused before anything is asked: `spi upgrade cancel` first). `spi upgrade status` shows it; `spi upgrade cancel` removes it. A `--reset-pins` given when scheduling is done by the scheduled run, not before.

- Linux: a systemd timer and service, `sp-upgrade.timer` and `sp-upgrade.service`, in `/etc/systemd/system`, enabled so they survive a reboot. The time is written in UTC, so a change of the zone cannot move it. The service runs `spi upgrade run-scheduled` as root and logs to `<log_dir>/spi-upgrade.log`.
- macOS: a launchd job, `org.servicepoints.upgrade`, in `/Library/LaunchDaemons`, with `StartCalendarInterval` for the time and `RunAtLoad`.
- **A time that was missed** (the machine was off or asleep): if the run starts no more than one hour late it goes ahead; later than that it **skips**, logs that it did, changes nothing and removes itself. When it is done it removes the timer, the service file and the record. Scheduling needs root (it writes the service manager's folder), and says so when it cannot.

**`spi upgrade check [--json]`** reads only: one line per program with the installed version, the `version` in spi.toml, the newest release, and `up to date`, `upgrade available`, `pinned`, `unknown (reason)` or, for air_server, `no releases published upstream`, then a summary line. It exits 0 even when a line is unknown.

**Where releases come from.** psp-mint and esp-mint: the GitHub releases of `dav-anderson/psp-mint` and `dav-anderson/esp-mint`, listed through `[release] url`. A release carries `<program>-<version>-<os>-<arch>.tar.gz` (`os` is `linux` or `macos`, `arch` is `x86_64` or `aarch64`) and `SHA256SUMS`. spi downloads the archive for this machine and `SHA256SUMS` with `curl`, checks the archive's SHA-256, unpacks it with `tar` and installs the binary; a release with no archive for this machine is refused and names what it has, and a download that fails its checksum is deleted and nothing is changed. Downloads are kept under `<spi folder>/releases/<program>/<version>/` (`[release] keep` versions per program). bitcoind: bitcoincore.org (`bin/bitcoin-core-<version>/bitcoin-<version>-<cpu>-<os>.tar.gz` and `SHA256SUMS`; a folder that has no `SHA256SUMS` yet, as `bitcoin-core-32.0/` had while it held only `test.rc*` folders, is not a release and is skipped), unless you supply `bin`. air_server publishes no releases (`[air] version` is your word for what you installed). **Not built:** the signature check of our own releases (psp-mint and esp-mint: the SHA-256 beside the archive only), and the newest-release default of `spi install` for psp-mint and esp-mint (it installs the build `bin` names). A pinned program whose release list cannot be read installs the build `bin` names.

The health monitor restarts a mint only after `HEALTH_FAILURES` (3) polls in a row fail, 30 s apart, so an upgrade restart that takes less than about a minute is not mistaken for a fault.

### Using the services

On Linux, `systemctl status psp-mint`, `tail -f /var/log/sp-mint/psp-mint.log`, and `sudo systemctl reload psp-mint.service` makes psp-mint read its settings again (see "Changing settings"; esp-mint does the same). `sudo systemctl kill -s USR1 --kill-whom=main psp-mint.service` asks it for a snapshot of its data (a consistent copy under `<data folder>/snapshots/`, taken while it serves; the mint's README says how long the pause is and what the copy holds); `spi snapshot` does that for both mints. **Logs are kept under their cap by spi, on Linux.** Each unit runs `spi logs rotate` before its program starts, a restart after a crash included: a log over `log_max_size_mb` is moved aside as `<log>.<epoch>.bak` and only the newest `log_keep` of those stay. The services are meant never to stop, so `sp-logs.timer` also runs `spi logs rotate --running` every 5 minutes (as root, because the health monitors and the scheduled upgrade write their logs as root) on every log: a log over the cap is copied aside and emptied in place, because a rename would leave the running service writing to the moved file. A log can pass the cap by what is written in those 5 minutes. A copy of `spi` is installed in `bin_dir` for these units. Older versions of spi wrote a logrotate file (`/etc/logrotate.d/sp-mint`); the install script removes it, if spi wrote it, so that two rotations do not work on one log. On macOS, `sudo launchctl print system/org.servicepoints.psp-mint` shows a service and `sudo launchctl kill HUP system/org.servicepoints.psp-mint` reloads psp-mint (`USR1` takes the snapshot). launchd keeps the log file open, so a rotated log is written to until the service is restarted: **spi does not rotate service logs on macOS, and `log_keep` and `log_max_size_mb` do nothing there**: macOS is a development machine (spi is designed to run on Linux), so clear them by hand between tests and restart the service after.

### The health monitor

`sp-health-monitor.sh` asks the mint's `GET /health` every `HEALTH_INTERVAL_SECS` (30). Healthy is `200`. Anything else, including no answer within `HEALTH_TIMEOUT_SECS` (5), is a failure; `HEALTH_FAILURES` (3) in a row trigger the action. With `HEALTH_ACTION=report` it logs and runs `HEALTH_REPORT_CMD` (if set; `$HEALTH_MESSAGE` holds the text). With `HEALTH_ACTION=restart` (what the settings file starts with) it also runs the restart command, then leaves the mint alone for `HEALTH_GRACE_SECS` (60) so a slow start is not counted. It reports again when the mint is healthy. It runs as root only because restarting a system service needs it; with `report` it can run as the service user.

What `/health` means: psp-mint answers 503 when its scanner has not finished a pass for `scan.health_intervals` intervals plus the grace, or its lock is poisoned; esp-mint when its locks are poisoned. A mint that hits one of these on its own also exits with status 1, so the service manager restarts it before the monitor does; the monitor is for the mint that is up but wrong, or hung.

### What a restart does and does not do

A mint exits with status 1 for a fatal fault and the service manager starts it again: systemd after 5 s growing to 5 min (an older systemd than 254 ignores the growth and waits 5 s each time), launchd after 10 s every time. Neither gives up. A stop (`systemctl stop`, `launchctl bootout`, or Ctrl-C) sends SIGTERM (or SIGINT): the mint stops taking connections, lets the requests it is answering finish, and exits with status 0, which the managers leave alone; after the stop timeout (120 s for a mint: the sum is in the unit's comment) it is killed (bitcoind gets 10 minutes to write its state out). A kill, a crash or a power cut is the same as any other failure: the next start picks up from what is on disk (each mint's DESIGN.md, "Crash safety"). A restart never opens a mint's data twice: the database refuses to be opened by a second process, so if a new process starts while the old one is still stopping, it fails to open and the manager starts it again.

### Removing the services

This is decommission: it stops the mints for good. Linux:

```
sudo systemctl disable --now psp-mint-health.service esp-mint-health.service sp-air-server-health.service esp-mint.service psp-mint.service sp-air-server.service sp-bitcoind.service
sudo rm /etc/systemd/system/psp-mint.service /etc/systemd/system/esp-mint.service /etc/systemd/system/psp-mint-health.service /etc/systemd/system/esp-mint-health.service /etc/systemd/system/sp-air-server.service /etc/systemd/system/sp-air-server-health.service /etc/systemd/system/sp-bitcoind.service
sudo systemctl daemon-reload
```

macOS: `sudo launchctl bootout system/org.servicepoints.<name>` for each of the seven (bitcoind, air-server, psp-mint, esp-mint and the three health monitors), then `sudo rm /Library/LaunchDaemons/org.servicepoints.<name>.plist`. This leaves the binaries, `/etc/sp-mint`, the logs, the account and the data in `/var/lib/sp-mint` where they are. The data holds the master secrets and every note's history; remove it only when you mean to lose them.

## Status, and bitcoind's sync

`status` reports bitcoind, the AIR server (its health address, `ok` or not), psp-mint and esp-mint, then the setup steps (see "Setup") and `ready`. A mint that is up while the AIR server is down is reported as it answers, and the AIR line says why wallets can not reach it. `--json` has the same, with a `steps` list and `ready`. It asks bitcoind `getblockchaininfo` and keeps three cases apart:

| bitcoind | Shown as | PSP | Service |
|---|---|---|---|
| no answer, or the cookie cannot be read | `unreachable`: "bitcoind unreachable at ..." | not ready: "bitcoind unreachable" | `down` |
| answers, other chain than the config | `wrong_chain` | not ready | `down` |
| answers, still in initial block download | `syncing`: "bitcoind syncing 63% (blocks 630 of 1000 headers)" | **not ready: "bitcoind syncing 63%"** | `degraded` |
| answers, synced | `ok`: "synced, N blocks" | as its own `/health` | `ok` |

Syncing is `initialblockdownload` true **and** either headers ahead of blocks or `verificationprogress` under 99.99%. The flag alone is not trusted: Bitcoin Core keeps it set on a chain whose last block is more than a day old, which is every fresh regtest node until its first block, and that node is caught up. The percentage is `verificationprogress` rounded down, and never 100 while syncing. The wallet is checked too (`wallet ... loaded` or `NOT loaded`).

PSP is "not ready" while the node is not synced even if psp-mint answers `/health` with 200, because psp-mint reads the chain through the node and would be settling payments on a chain that is behind. The ESP mint is reported on its own `/health`.

`spi start` and the sync: it starts bitcoind first, then waits (up to `sync_wait_secs`) for the sync to finish, and only then starts psp-mint. If the wait runs out it stops with the progress ("bitcoind syncing 63% after waiting 600 s: psp-mint and esp-mint were NOT started") and starts neither mint; bitcoind keeps syncing, and `spi start` can be run again. Why it waits and does not start psp-mint to report "not ready" is in DESIGN.md.

## Audit

`spi audit` reads psp-mint's and esp-mint's `GET /audit` and prints what they owe:

```
spi audit on regtest (10 PSP = 1 USD; FSP not counted)
  PSP outstanding                            60 PSP  =   60.00000000000 PSP  = $6.000000000000
  ESP matured       500000000000 ESP  = 5.00000000000 PSP  = $0.500000000000
  ESP unmatured    1500000000000 ESP  = 15.00000000000 PSP  = $1.500000000000
  ESP total        2000000000000 ESP  = 20.00000000000 PSP  = $2.000000000000
  TOTAL                                      80 PSP  = 80.00000000000 PSP  = $8.000000000000
```

Two more things are shown when the mint and the wallet answer: psp-mint's own books, "PSP paid to the SPI: N PSP, of which burned (revenue) X, reissued to the SPI wallet Y" (`paid`, `burned` and `reissued` of its `/audit`), and the SPI wallet's balance ("SPI wallet PSP", "SPI wallet ESP", from `ecash-wallet balance`). An unreadable wallet is a warning and the rest of the report is printed; audit stops nothing, and the wallet is only read. The columns are narrower here than in the real output. PSP is in PSP (whole PSP), ESP in ESP units (10^11 to the PSP), and every line is also given in PSP and in USD at **10 PSP = 1 USD**. Matured and unmatured ESP both count at full value. **FSP is not counted**: it is backed by ESP the FSP mint holds, which the ESP figure already contains. All sums are exact integers (128-bit, checked: a figure too large to add is an error, not a wrapped number); the PSP and USD decimals are worked out from them. The rate is one constant, `PSP_PER_USD` in `src/audit.rs`, with a comment that **it must match the payout rate**. `--json` prints the same numbers as strings.

**The split comes from esp-mint 0.5.0 and later.** Its `GET /audit` gives `outstanding_matured` (redeemable-after days that have come) and `outstanding_unmatured` (days still ahead), which add up to `outstanding`; `spi audit` checks that and refuses a reply where they do not. Any other field in the reply is ignored. Reading it:

- esp-mint adds a request to the split when it finishes it, and reports `requests_unfinished`. If that is above zero the parts may differ from the total for a moment, and `spi audit` accepts the gap and says so in a warning. With none unfinished, a gap is an error.
- An esp-mint before 0.5.0 gives no split: ESP is shown as one figure with a warning, as before.

How the split is kept, and why it survives restarts, is in esp-mint's DESIGN.md, "The audit split".

## Setup

`spi setup` lists the steps to a working SPI, marks the ones that are done, and prints what to run for the first that is not. It writes the generated config files (the wallet's too) and starts nothing.

| Step | Done when |
|---|---|
| a. bitcoind | running, synced, wallet loaded |
| b. AIR server | its health answers |
| c. psp-mint | running |
| d. esp-mint | running |
| e. PSP in the SPI wallet | the SPI wallet holds PSP (**required**) |
| f. FSP bought from another service, for backups | not tracked (recommended: an SPI that wants backups buys FSP elsewhere) |

The SPI is **not ready until step e is done**; `spi status` shows `ready` and the steps. The wallet is asked only when a to d are done, since it needs the AIR server and the PSP mint to answer. There is no "open channels" step. To fund the wallet there are two ways, and `setup` prints both with the DID and the paths filled in:

- **Buy PSP.** The first SPI buys from itself, with every fee off (a fee it pays itself only eats into what it buys; `setup` notes a non-zero `claim.write_fee` or `delivery.read_fee` in `[psp.settings]`): `ecash-wallet buy-psp --mint-key <the PSP's DID>`, pay the payload it prints once, as the OP_RETURN of a Bitcoin payment to the PSP mint's wallet, then `ecash-wallet claim-psp` once the payment has its confirmations. The wallet claims what the PSP mint says the payment is owed; there are no amounts to choose.
- **Take a hand-off** from another SPI: `ecash-wallet handoff import STRING-OR-FILE --did-key-file FILE`.

`spi` prefixes the wallet command with its `--wallet` and `--config` and the network, so `setup` prints a line you can paste. `spi config show` prints the wallet's settings and where each came from. Both paths need the PSP's DID: spi reads it from `<data_dir>/<network>/mint_did_key.json` when the file exists (it asks `psp-mint pubkey` only then, which would make the file) or from `[psp] did`.

## The PSP the SPI is paid

A spend that pays the SPI (`pay` in a kind 14 request) is not burned by default. psp-mint reissues the paid amount as notes for the SPI, one file for each request, in `<PSP data folder>/<network>/revenue/<request hash>.json`. **Whoever holds a file holds the notes**: it is written readable by the mint's account and group only, and is cash until the SPI's wallet has taken it in. `spi collect` runs `ecash-wallet receive-psp FILE` for each file not yet taken in, records the names in `<spi folder>/wallet/received.txt`, and reports each. A file that was taken in already is recorded and not an error; a file the wallet refuses is reported, and the others are still taken. `spi audit` shows `paid`, `burned` and `reissued`.

With `[psp] burn = true`, psp-mint measures `burn_threshold_psp` against **the SPI wallet's current PSP balance**, not a running total over the mint's life. psp-mint cannot see the wallet, so spi publishes the figure: `spi collect` (after it has taken the reissued files in) and `spi audit` write `{"psp": W, "reissued": R, "at": T}` to `[psp] burn_balance_file`. `W` is `ecash-wallet balance`, read first; `R` is psp-mint's `reissued` total from `/audit`, read after; `T` is when `W` was read. The file is written to a temporary name and renamed, mode 0640. At each spend psp-mint takes `held = W + (its reissued now - R)`, so PSP it reissued since the reading counts as held even though the wallet has not taken it in yet, keeps `min(pay, threshold - held)` for the wallet and burns the rest; the burned amount counts as revenue (`burned`). When the wallet spends and spi publishes a lower balance, reissuing starts again. **No usable figure means no burning**: a missing, malformed, stale (older than `burn.balance_max_age_secs`, 24 hours by default) or future-dated figure makes psp-mint reissue the whole of `pay`, and log why once. So a spi that is not run costs the burning and never the revenue; the figure is refreshed by `spi collect` and `spi audit` (and by hand with `spi publish-balance`); there is no timer, because each publish runs `ecash-wallet balance`, which talks to the AIR server and the PSP, and the figure only has to be close. Older than the maximum age, psp-mint reissues everything and burns nothing, which is safe. **A spend made with `ecash-wallet` directly is not seen by psp-mint until the next `collect` or `audit`**; until then psp-mint believes the wallet holds more than it does, which can only mean it keeps less for the wallet and burns more than the threshold asks. No spi command spends from the SPI's wallet. On a service, the figure lives in `<data_dir>/spi-share/`, a folder `install-system.sh` creates group-writable and setgid, so `spi` (run by a member of the service group) can write it and the service user can read it. Off by default: with burn off, nothing is burned.

## The key check

From psp-mint 0.9.0, esp-mint 0.11.0 and ecash-wallet 0.9.0 (wire version 5) every mint publishes the keys it signs with on AIR, under its own DID, and the SPI's wallet checks every reply against them before it stores a note. A mint that signed a user with keys other users do not see could recognise that user's notes when they came back; this is how that is caught. spi does nothing to make it work and has no setting for it. What it means for an SPI:

- The wallet reads the PSP mint's and the ESP mint's streams on its own, holding one read on each, and caches what it reads. Nothing is needed from the operator.
- If psp-mint or esp-mint fails the check, the wallet **warns loudly** (it names the mint, what failed and where) and keeps its notes and the pending request. It **never blacklists** the SPI's PSP or ESP mint, whatever the wallet's `fsp.blacklist_on_key_mismatch` is: that setting only acts on FSP mints, so it does nothing for the SPI. It is in the wallet's settings, and can be set under `[wallet.settings.fsp]`, but spi does not name it.
- esp-mint's stream grows with the services it knows (see `[esp.settings.stream]` above). Its README says what a stream costs and what the residual is: a connection that keeps reading the same few buckets of a large day can still be recognised, less precisely.
- An SPI whose wallet or mints are older than these releases cannot talk to the new ones (wire version 4 against 5). Upgrade the mints and the wallet together (`spi upgrade`; the wallet is installed by `cargo install`, as above).

## Versions

`spi versions [--json]` prints one line for every program and library, each a version or "unknown" with the reason:

- the programs: `spi`, `psp-mint`, `esp-mint`, `ecash-wallet` and `bitcoind` are asked (`--version`); the installed copy in `bin_dir` is a second `(service)` line when it exists, since it can be a different build;
- the libraries each program was built with (`psp-mint/sp-wire`, `esp-mint/sp-wire`, `spi/sp-wire` and so on), read from the `Cargo.lock` the program was built against; this is exact only for a build with `--locked`, and a program built without it can say something else;
- `air_server`: it prints no version, so the line is the version spi installed (`<spi folder>/air/installed.json`, written by `spi install` and `spi upgrade` with the file's hash) if the file at `bin_dir` or on `PATH` still has that hash, and "unknown" with that reason when it does not. A copy somebody replaced by hand is therefore not reported as the version spi installed.

## Tests

```
cargo test
cargo fmt --check
```

Unit and integration tests: the config (defaults, regtest unless written down, environment over file, every problem listed), the generated files (header, the named values, pass-through of nested tables, arrays and floats, a pass-through of a named key refused), the audit arithmetic (rate, a single ESP unit, `u64::MAX` figures, overflow, a split that does not add up, unfinished requests), the service files (the Linux units, with the rotation of each log before its program starts and the logs job, and the macOS plists that `install` writes with no other folder present, the custom `[service]` settings, the cookie and bitcoind arguments, a missing binary, a refused value, a template with a bad placeholder, the `--ops` override, and `sh -n` on the generated script), the command line run as a process (an unknown command, a flag a command does not take, `install` with both service-file options, `config` without `show`, a bad `--network`, and `--dir`, `--config` or `--network` with no value, all refused before anything is read or written), and `status`, `audit`, `install` and the sync wait against **stand-ins** for the mints and bitcoind: small HTTP servers in the test that answer with the replies the real ones give. The sync cases are bitcoind syncing, synced and unreachable (and a fresh regtest node, another chain, an unreadable cookie). The AIR server, the wallet and the other programs are run through a fake runner that records the command line it is given (`tests/common`), so the tests check what spi would run, with which arguments and environment, and what it does with the answers: the AIR settings reaching both mints and the server's environment, a non-zero AIR fee refused, the wallet config and its sources, `setup` and `ready`, `collect` (once, twice, a refused file), `reload` (a real SIGHUP to a child process), `start` and `stop` of the AIR server, `versions`, `upgrade` (the warning and the confirmation, a dry run that asks nothing, a run that is not confirmed changing nothing, a restart waiting for `/health` to report the target version, a rollback, the exact downtime lines, the macOS commands, pins kept and reset, a download checked against its SHA-256, scheduling, `status`, `cancel`, the scheduled run on time, early and too late, and `upgrade check`), `restart`, `snapshot`, and a GET that retries a lost connection. `spi logs rotate` is run as a process with no `HOME` and no `spi.toml`, as a service manager runs it. What spi shares with aso (the release source, the checksum and the swap, the schedule, the log rotation) is tested in air-minterface, with a stub operator; spi's tests keep its wording, its restart commands and its settings.

They are not the real mints. A real psp-mint needs a running bitcoind (REGTEST.md in psp-mint is the end-to-end run), and the esp-mint state a test needs to start one in-process is not in a released tag. **The service script has not been run by the tests**: running it needs root and a real machine, so it is only syntax-checked with `sh -n`, and the units are not run through `systemd-analyze verify` or the plists through `plutil -lint` (the macOS test checks that the tags balance). Do a first install on a test machine before one that matters. **The happy path of starting a real `air_server`, a real wallet and a real restart under systemd or launchd is not run by the tests**, and `spi upgrade` works on the service install only, not on programs `spi start` started. To try spi against real ones on regtest: start `bitcoind -regtest -daemon`, then `spi install`, `spi start`, `spi status`, `spi setup`, `spi audit`, `spi stop`.

## Documentation

- [DESIGN.md](DESIGN.md): why spi drives the binaries, the config generation, the AIR server and the wallet, the start order, the service files and why they are a script, the audit and the esp-mint split, uptime, and what a no-downtime upgrade would need (deferred), what is not done.
- [CHANGELOG.md](CHANGELOG.md).
