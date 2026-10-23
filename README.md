# spi

One config and one command line over `psp-mint`, `esp-mint` and `bitcoind`. It does not link the mints or change them: it writes their config files and the files that run them as services, starts and stops their binaries, and reads their `GET /health` and `GET /audit`. It never manages fsp-mint: spi is psp-mint, esp-mint and bitcoind only, and an FSP mint is run through the operator control interface of the AIR service it belongs to. Version 0.1.0. Why it is built this way is in [DESIGN.md](DESIGN.md).

## Getting the binaries

A machine needs four programs: `spi`, `psp-mint`, `esp-mint` and `bitcoind`. Nothing else needs to be cloned or built: the service definitions are inside the `spi` binary (see "Running as services"). The Rust ones are installed with `cargo` (get it from <https://rustup.rs>), straight from the git tags:

```
cargo install --git https://github.com/dav-anderson/psp-mint.git --tag v0.3.0
cargo install --git https://github.com/dav-anderson/esp-mint.git --tag v0.5.0
cargo install --git https://github.com/dav-anderson/SPI.git --tag v0.1.0
```

(Or `cargo install --path .` in a checkout of this repository.) Each puts its binary in `~/.cargo/bin`. Use the tag of the release you mean to run; the tags are in each repository's CHANGELOG. **esp-mint 0.5.0 is the first release that gives `spi audit` the matured / unmatured split**; an older one still works and `audit` says it has no split.

`bitcoind` is Bitcoin Core: download a release from <https://bitcoincore.org/en/download/>, check its signature as the site describes, and put `bitcoind` (it is in the `bin` folder of the archive) on `PATH`, or use your system's package manager. If it is somewhere else, name it in `spi.toml` (`[bitcoind] bin`). `spi` looks for each of the three on `PATH` (or at the path given in `spi.toml`), and `spi install` copies what it finds to `/usr/local/bin` for the services, because a service user can not run programs out of someone's home folder.

## The config

One file, `spi.toml`, in `~/.spi` (`SPI_DIR`, or `--dir`; `SPI_CONFIG` or `--config` name the file). `spi install` writes a commented starter if there is none. Strongest first: the environment, the file, the default. `spi config show` prints every setting with its value and where it came from. An unknown key or a bad value stops with the list of every problem.

| Setting (`spi.toml`) | Environment | Default | What it does |
|---|---|---|---|
| `network` | `SPI_NETWORK` (or `--network`) | `regtest` | `main`, `regtest`, `signet` or `testnet`. **Regtest unless you write another one down.** Main is never a default. |
| `[bitcoind] rpc_url` | `SPI_BITCOIND_RPC_URL` | per network (`http://127.0.0.1:18443` on regtest) | bitcoind's RPC address. |
| `[bitcoind] cookie` | `SPI_BITCOIND_COOKIE` | per network and OS, as psp-mint's | The RPC cookie file. Cookie authentication only. |
| `[bitcoind] wallet` | `SPI_BITCOIND_WALLET` | `psp-<network>` | The wallet psp-mint uses. |
| `[bitcoind] create_wallet` | | `true` | `install` creates the wallet if it does not exist. |
| `[bitcoind] bin` | `SPI_BITCOIND_BIN` | `bitcoind` | The binary `start` runs. |
| `[bitcoind] datadir` | `SPI_BITCOIND_DATADIR` | none; in the service, `<data_dir>/bitcoind` | Passed as `-datadir=`. |
| `[bitcoind] args` | | none | Extra arguments for `bitcoind` (`["-txindex"]`). |
| `[bitcoind] manage` | | `true` | `start` starts bitcoind when it is not answering, and `install` writes a service for it. `false`: spi never starts one and writes no service for it (use this for a bitcoind that runs some other way). |
| `[bitcoind] sync_wait_secs` | | `600` | How long `start` waits for bitcoind to finish syncing before it gives up. 0 to 604800. |
| `[psp] bind`, `[esp] bind`, `[esp] admin_bind` | `SPI_PSP_BIND`, `SPI_ESP_BIND`, `SPI_ESP_ADMIN_BIND` | `127.0.0.1:3000`, `:3100`, `:3101` | The addresses of the mints. The ESP mint's PSP URL follows `[psp] bind`. |
| `[psp] bin`, `[esp] bin` | `SPI_PSP_BIN`, `SPI_ESP_BIN` | `psp-mint`, `esp-mint` | The mints' binaries. |
| `[service] user`, `group` | `SPI_SERVICE_USER`, `SPI_SERVICE_GROUP` | `spmint` (Linux), `_spmint` (macOS); the group is the user's | The account the services run as: no login, no password, owns the data and the logs. Lowercase letters, digits, `_` and `-`. |
| `[service] bin_dir`, `etc_dir`, `data_dir`, `log_dir` | `SPI_SERVICE_BIN_DIR`, `..._ETC_DIR`, `..._DATA_DIR`, `..._LOG_DIR` | `/usr/local/bin`, `/etc/sp-mint`, `/var/lib/sp-mint`, `/var/log/sp-mint` | Where the services find their binaries, settings and env files, data and logs. Absolute paths. |
| `[service] health` | | `true` | Whether a health monitor is installed for each mint. |

**Any other mint setting** goes under `[psp.settings]` or `[esp.settings]`, in the mint's own sections, and is copied to the generated file as it is:

```toml
[psp.settings.settle]
min_confirmations = 3

[esp.settings.terms]
maturity_days = 365
```

A key that spi sets itself (`bitcoin.network`, `bitcoin.rpc_url`, `bitcoin.wallet`, `bitcoin.cookie` and `server.bind` for psp-mint; `server.bind`, `server.admin_bind`, `server.network` and `psp.url` for esp-mint) cannot also be passed through: that would be two places for one value, and `install` refuses it, naming the key. The mints' own environment variables (`PSP_...`, `ESP_...`) still win over the generated files, as they do for any mint; `ESP_ADMIN_TOKEN` is read from spi's environment, not from any file.

## Commands

| Command | What it does |
|---|---|
| `spi install [--service-files DIR \| --no-service-files] [--ops DIR]` | Makes the data folders under the spi folder, writes `generated/psp-mint.toml` and `generated/esp-mint.toml` (each starts with "Generated by spi. Don't edit"), checks that bitcoind is reachable, on the right chain, and that its wallet is loaded (loads it from disk, or creates it). A bitcoind on the wrong chain fails the command. **It also writes everything that runs the mints as services** (see "Running as services"), into `<spi folder>/service` or `DIR`, and prints the `sudo` steps; it installs nothing itself. A bitcoind that is not running yet is not a failure while service files are being written and spi manages bitcoind, because the service starts it; with `--no-service-files`, or `manage = false`, an unreachable bitcoind fails the command after the files are written. A bitcoind that is still syncing does not: it is reported, and the wallet is still set up. `--ops DIR` replaces any embedded definition with the file of the same name from `DIR`. |
| `spi start` | Writes the config files again. Starts bitcoind if it is not answering (and `manage` allows it). **Waits for bitcoind to finish syncing.** Then starts psp-mint, then esp-mint, each as a background process of the current user, and waits for `/health`. |
| `spi stop` | Stops esp-mint, then psp-mint (SIGTERM, as the service definitions do), then bitcoind only if `spi start` started it. |
| `spi status [--json]` (also `health`) | See "Status". Exit 0 only when everything is ok. |
| `spi config show` | The settings in use, where each came from, and the settings passed through. |
| `spi audit [--json]` | See "Audit". |

A flag a command does not take is refused, naming it, with exit status 2 and before anything is read or written; global flags (`--dir`, `--config`, `--network`) go before the command and each needs a value.

For a machine that starts the mints at boot and restarts them, use the services that `install` writes, not `start`; see "Running as services".

## Running as services

`spi install` is the bootstrap for a machine that starts the mints at boot, starts them again when they fail, runs them as a user that is not root, and watches them. On a fresh Linux (systemd) or macOS (launchd) machine that has the four programs from "Getting the binaries", it needs nothing else: not a checkout of any other repository, and no folder of definitions. With no `spi.toml` it writes a starter one (regtest) and carries on.

**What it writes**, all from `spi.toml`, into `<spi folder>/service` (`~/.spi/service`) or the folder after `--service-files`:

- for psp-mint and esp-mint each: its settings file `<mint>.toml` (the generated one, except that the cookie is the service's own), its env file `<mint>.env` (the data folder, the settings file, the network) and an empty `<mint>.secret.env` for secrets;
- the systemd units (Linux) or launchd property lists (macOS): one for each mint, one for bitcoind unless `[bitcoind] manage` is `false` (`sp-bitcoind.service` on Linux, so that it can not replace a `bitcoind.service` from a package), and a health monitor for each mint unless `[service] health` is `false`, with `sp-health-monitor.sh` and a settings file `<mint>-health.conf` for each;
- on Linux, a logrotate file;
- `install-system.sh`, the script that puts all of it in place as root.

**The definitions are generated from `spi.toml`, not copied.** The templates are in [`ops/`](ops/) in this repository and are compiled into the binary, so the binary alone is enough. The binary folder, the user and group, the data, log and settings folders, the bitcoind data folder and arguments, the chain, the RPC port and the mints' addresses are filled in from `spi.toml`, so changing `[service]` changes every file that mentions it. An unknown or unclosed placeholder stops `install` with an error that names it. A path, user name or bitcoind argument that holds a space, a quote, `%` or `$` is refused with the setting named, because it would need a different escape in systemd, launchd and sh. `--ops DIR` (or `SPI_OPS_DIR`) is optional: a file in `DIR` named as spi names it (`systemd/psp-mint.service`, `systemd/sp-bitcoind.service`, `launchd/org.servicepoints.psp-mint.plist`, `sp-health-monitor.sh`, ...) replaces the embedded one, with the same placeholders; any it does not have comes from the binary.

**What it does not do: run anything as root.** `spi install` stays an ordinary user's command. It prints the exact steps and leaves the root part to a script you can read first, rather than doing it behind a `--system` flag. DESIGN.md has the reasons.

### The steps

On Linux, on a machine with `psp-mint`, `esp-mint` and `bitcoind` on `PATH`:

```
spi install
sudo sh ~/.spi/service/install-system.sh
sudo systemctl enable --now sp-bitcoind.service
spi install --no-service-files
sudo systemctl enable --now psp-mint.service esp-mint.service psp-mint-health.service esp-mint-health.service
spi status
```

The first `spi install` finds no bitcoind running: that is expected here and not a failure, because the service it just described starts it. The script creates the account and the folders, installs the three binaries it found on `PATH` into `bin_dir`, installs the files and the definitions, and starts nothing. Once bitcoind answers, the second `spi install --no-service-files` creates the wallet psp-mint uses; then the mints are started. Put `ESP_ADMIN_TOKEN` in `/etc/sp-mint/esp-mint.secret.env` before you start esp-mint if you use the operator routes (see "The files" for why it is a file of its own). On macOS the script is the same and the starting lines are `sudo launchctl bootstrap system /Library/LaunchDaemons/org.servicepoints.<name>.plist` for `bitcoind`, then (after the wallet step) `psp-mint`, `esp-mint`, `psp-mint-health` and `esp-mint-health`. `spi install` prints the exact lines for the system it ran on.

Bitcoin Core needs its initial block download before psp-mint has anything to scan, which takes days on main. The services do not wait for it: psp-mint is started after bitcoind (systemd orders them with `After=`, which means bitcoind has been started, not that its RPC is answering yet; launchd has no ordering at all), so psp-mint can start before the node answers, and what it does then is psp-mint's own (the service manager starts it again if it exits). `spi status` reports it as **not ready** while the node is syncing or unreachable. Unlike `spi start`, the service manager does not hold psp-mint back until the sync is finished; if you need that, start the mints yourself when `spi status` says `ok`.

**Running spi against the service bitcoind.** The service bitcoind keeps its data, and so its RPC cookie, under `data_dir` (`/var/lib/sp-mint/bitcoind/<chain>/.cookie`), owned by the service user; psp-mint, which runs as the same user, reads it without any change of permissions. bitcoind is started with `-rpccookieperms=group`, so the cookie is also readable by the service group. To run `spi install` and `spi status` as yourself, add yourself to that group and name the cookie in `spi.toml`; `spi install` prints this with the exact path:

```
sudo usermod -aG spmint "$USER"
```

On macOS: `sudo dscl . -append /Groups/_spmint GroupMembership "$USER"`. Then log in again and set `cookie = "/var/lib/sp-mint/bitcoind/regtest/.cookie"` under `[bitcoind]` (the chain's own folder, as `install` prints). The cookie is the node's RPC password, so it lets whoever reads it use the node's wallet: add only people who may. If you set `bitcoind.cookie` to somewhere else, psp-mint reads it there, and `install` warns when that is not where the service bitcoind writes it. If `bitcoind.manage` is `false`, the cookie is whatever you point it at, and a cookie in a home folder gets a warning (the service user can not read it, and the unit then uses `ProtectHome=read-only`).

### The layout

| What | Where (the defaults of `[service]`) |
|---|---|
| The binaries | `/usr/local/bin/psp-mint`, `esp-mint`, `bitcoind` and `sp-health-monitor.sh`: copied by the script from where `spi` found them. The units run these, not `~/.cargo/bin`: a service user can not read a home folder, and `ProtectHome` hides it. |
| The user | `spmint` on Linux, `_spmint` on macOS: no login, no password, owns the data and the logs. One account for all services, so that psp-mint reads bitcoind's cookie. |
| A mint's data | `/var/lib/sp-mint/<mint>` (`PSP_DATA_DIR`, `ESP_DATA_DIR`). bitcoind's is `/var/lib/sp-mint/bitcoind` unless `[bitcoind] datadir` is set. |
| A mint's settings | `/etc/sp-mint/<mint>.toml` (`PSP_CONFIG`, `ESP_CONFIG`): generated, root:group, mode 0640. |
| The env file | `/etc/sp-mint/<mint>.env`: generated, root:group, 0640. The data folder, the settings file, the network. |
| The secrets file | `/etc/sp-mint/<mint>.secret.env`: root:group, 0640. Written once, empty, and **never overwritten**: put `ESP_ADMIN_TOKEN` and any other secret here, not in `spi.toml`. Read after the env file, so it wins. |
| The monitor's settings | `/etc/sp-mint/<mint>-health.conf`: root, 0644. Written once and **never overwritten**; edit it freely. |
| Logs | `/var/log/sp-mint/<mint>.log`, `<mint>-health.log` and `bitcoind.log` (bitcoind's own log is `debug.log` in its data folder). |
| The definitions | `/etc/systemd/system/<mint>.service`, `<mint>-health.service`, `sp-bitcoind.service`; `/Library/LaunchDaemons/org.servicepoints.<mint>.plist`, `...<mint>-health.plist`, `org.servicepoints.bitcoind.plist`. |

**The files spi writes and the files that are yours.** Everything generated from `spi.toml` (settings, env files, definitions, the monitor script, the binaries) is replaced each time the script runs, so a change in `spi.toml` reaches the system. A person edits only the two files that are never overwritten: the secrets file and the monitor settings. The monitor's address, name and restart command are not in its settings file: the definition sets them from `spi.toml`, and they win over the file. For a manual check, `HEALTH_URL=http://127.0.0.1:3000/health sp-health-monitor.sh --once` polls once and exits 0 or 1.

Start order is bitcoind, psp-mint, then esp-mint (each needs the one before). systemd orders them with `After=`. The mints do not depend on it: esp-mint waits for the PSP at startup and asks again every `psp.startup_retry_secs` (5 by default) for as long as it takes.

### Changing settings, and upgrading

Edit `spi.toml`, run `spi install` (it writes the same folder again), run the script again, then restart what changed: `sudo systemctl restart psp-mint.service esp-mint.service` (Linux), or on macOS `sudo launchctl kickstart -k system/org.servicepoints.psp-mint` and the same for the others. If a definition changed, run `sudo systemctl daemon-reload` first on Linux (the script does); on macOS take the service out and put it back, `sudo launchctl bootout system/org.servicepoints.psp-mint` then `bootstrap` again, because launchd reads a property list only when it loads it. To upgrade a mint, `cargo install` the new tag (add `--force` if the same version is being reinstalled), run `spi install` and the script (which copies the new binary), and restart that service. A restart is safe at any time (see "What a restart does and does not do"). Mind the data: `data_dir` holds each mint's master secret and every note's history, and no step here removes it.

### Using the services

On Linux, `systemctl status psp-mint`, `tail -f /var/log/sp-mint/psp-mint.log`, and `sudo systemctl kill -s HUP psp-mint.service` make psp-mint or esp-mint read their settings again (see their READMEs for which settings reload). The mints keep their log file open, so logrotate needs `copytruncate`; the logrotate file the script installs sets it (weekly, 8 kept, compressed). On macOS, `sudo launchctl print system/org.servicepoints.psp-mint` shows a service and `sudo launchctl kill HUP system/org.servicepoints.psp-mint` reloads it. launchd keeps the log file open, so a rotated log is written to until the service is restarted: rotate by hand or with `newsyslog` and restart the service after.

### The health monitor

`sp-health-monitor.sh` asks the mint's `GET /health` every `HEALTH_INTERVAL_SECS` (30). Healthy is `200`. Anything else, including no answer within `HEALTH_TIMEOUT_SECS` (5), is a failure; `HEALTH_FAILURES` (3) in a row trigger the action. With `HEALTH_ACTION=report` it logs and runs `HEALTH_REPORT_CMD` (if set; `$HEALTH_MESSAGE` holds the text). With `HEALTH_ACTION=restart` (what the settings file starts with) it also runs the restart command, then leaves the mint alone for `HEALTH_GRACE_SECS` (60) so a slow start is not counted. It reports again when the mint is healthy. It runs as root only because restarting a system service needs it; with `report` it can run as the service user.

What `/health` means: psp-mint answers 503 when its scanner has not finished a pass for `scan.health_intervals` intervals plus the grace, or its lock is poisoned; esp-mint when its locks are poisoned. A mint that hits one of these on its own also exits with status 1, so the service manager restarts it before the monitor does; the monitor is for the mint that is up but wrong, or hung.

### What a restart does and does not do

A mint exits with status 1 for a fatal fault and the service manager starts it again: systemd after 5 s growing to 5 min (an older systemd than 254 ignores the growth and waits 5 s each time), launchd after 10 s every time. Neither gives up. A stop (`systemctl stop`, `launchctl bootout`, or Ctrl-C) sends SIGTERM (or SIGINT): the mint stops taking connections, lets the requests it is answering finish, and exits with status 0, which the managers leave alone; after 30 s it is killed (bitcoind gets 10 minutes to write its state out). A kill, a crash or a power cut is the same as any other failure: the next start picks up from what is on disk (each mint's DESIGN.md, "Crash safety"). A restart never opens a mint's data twice: the database refuses to be opened by a second process, so if a new process starts while the old one is still stopping, it fails to open and the manager starts it again.

### Removing the services

Linux:

```
sudo systemctl disable --now psp-mint-health.service esp-mint-health.service esp-mint.service psp-mint.service sp-bitcoind.service
sudo rm /etc/systemd/system/psp-mint.service /etc/systemd/system/esp-mint.service /etc/systemd/system/psp-mint-health.service /etc/systemd/system/esp-mint-health.service /etc/systemd/system/sp-bitcoind.service
sudo systemctl daemon-reload
```

macOS: `sudo launchctl bootout system/org.servicepoints.<name>` for each of the five, then `sudo rm /Library/LaunchDaemons/org.servicepoints.<name>.plist`. This leaves the binaries, `/etc/sp-mint`, the logs, the account and the data in `/var/lib/sp-mint` where they are. The data holds the master secrets and every note's history; remove it only when you mean to lose them.

## Status, and bitcoind's sync

`status` asks bitcoind `getblockchaininfo` and keeps three cases apart:

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

The columns are narrower here than in the real output. PSP is in PSP (whole PSP), ESP in ESP units (10^11 to the PSP), and every line is also given in PSP and in USD at **10 PSP = 1 USD**. Matured and unmatured ESP both count at full value. **FSP is not counted**: it is backed by ESP the FSP mint holds, which the ESP figure already contains. All sums are exact integers (128-bit, checked: a figure too large to add is an error, not a wrapped number); the PSP and USD decimals are worked out from them. The rate is one constant, `PSP_PER_USD` in `src/audit.rs`, with a comment that **it must match the payout rate**. `--json` prints the same numbers as strings.

**The split comes from esp-mint 0.5.0 and later.** Its `GET /audit` gives `outstanding_matured` (redeemable-after days that have come) and `outstanding_unmatured` (days still ahead), which add up to `outstanding`; `spi audit` checks that and refuses a reply where they do not. Any other field in the reply is ignored. Reading it:

- esp-mint adds a request to the split when it finishes it, and reports `requests_unfinished`. If that is above zero the parts may differ from the total for a moment, and `spi audit` accepts the gap and says so in a warning. With none unfinished, a gap is an error.
- An esp-mint before 0.5.0 gives no split: ESP is shown as one figure with a warning, as before.

How the split is kept, and why it survives restarts, is in esp-mint's DESIGN.md, "The audit split".

## Tests

```
cargo test
cargo fmt --check
```

Unit and integration tests: the config (defaults, regtest unless written down, environment over file, every problem listed), the generated files (header, the named values, pass-through of nested tables, arrays and floats, a pass-through of a named key refused), the audit arithmetic (rate, a single ESP unit, `u64::MAX` figures, overflow, a split that does not add up, unfinished requests), the service files (the Linux units and the macOS plists that `install` writes with no other folder present, the custom `[service]` settings, the cookie and bitcoind arguments, a missing binary, a refused value, a template with a bad placeholder, the `--ops` override, and `sh -n` on the generated script), the command line run as a process (an unknown command, a flag a command does not take, `install` with both service-file options, `config` without `show`, a bad `--network`, and `--dir`, `--config` or `--network` with no value, all refused before anything is read or written), and `status`, `audit`, `install` and the sync wait against **stand-ins** for the mints and bitcoind: small HTTP servers in the test that answer with the replies the real ones give. The sync cases are bitcoind syncing, synced and unreachable (and a fresh regtest node, another chain, an unreadable cookie).

They are not the real mints. A real psp-mint needs a running bitcoind (REGTEST.md in psp-mint is the end-to-end run), and the esp-mint state a test needs to start one in-process is not in a released tag. **The service script has not been run by the tests**: running it needs root and a real machine, so it is only syntax-checked with `sh -n`, and the units are not run through `systemd-analyze verify` or the plists through `plutil -lint` (the macOS test checks that the tags balance). Do a first install on a test machine before one that matters. To try spi against real ones on regtest: start `bitcoind -regtest -daemon`, then `spi install`, `spi start`, `spi status`, `spi audit`, `spi stop`.

## Documentation

- [DESIGN.md](DESIGN.md): why spi drives the binaries, the config generation, the start order, the service files and why they are a script, the audit and the esp-mint split, what is not done.
- [CHANGELOG.md](CHANGELOG.md).
