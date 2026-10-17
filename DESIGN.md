# spi: design notes

## What it is, and what it is not

A thin layer over `psp-mint`, `esp-mint` and `bitcoind` for the person who runs them: one config, one command line. It has no mint logic and no keys. It does not hold the mints' state; it writes their config files, starts and stops their processes, and reads their public endpoints.

**It drives the binaries; it does not link the mints.** Either way worked, since both mints are libraries and binaries. Driving the binaries needs no change to either mint, no git dependency on them (so spi builds even while a mint's tag is moving), and gives the same behaviour as the service definitions, which also run `psp-mint serve` and `esp-mint serve`. Linking would have meant calling the mints' internals (`Config::load`, the server startup) that are not an interface. The only dependency on the family is `sp-wire`, for `ESP_PER_PSP`, so the unit constant exists in one place.

## One config, generated files

`spi.toml` is the only file an operator edits. `install` and `start` write `generated/psp-mint.toml` and `generated/esp-mint.toml` from it, each starting with a header that says it is generated and will be overwritten, and start each mint with `PSP_CONFIG`/`ESP_CONFIG`, `..._DATA_DIR` and `..._NETWORK` pointing at them. Values spi names (network, the bitcoind connection, the addresses, the PSP URL the ESP mint burns at) are set from the spi config. **Everything else is pass-through**: whatever is under `[psp.settings]` or `[esp.settings]` is merged into the generated file as it is, so a new mint setting needs no change in spi, and spi never validates a mint setting (the mint does, at start, with its own list of problems).

A key spi sets is refused under `settings`, naming the key. The alternative, letting the pass-through win, would let the generated file disagree with what `status` and `audit` assume (the network, the addresses). Both files are rendered before either is written, so one wrong key leaves the old files whole.

Defaults that depend on the network (RPC port, cookie path, wallet name) are computed by spi and **written into the generated file explicitly**, so spi and psp-mint cannot disagree on them. They are the same values as psp-mint's README "Networks" table.

## Regtest by default

The network setting defaults to regtest and is only ever main if the file or `SPI_NETWORK`/`--network` says so. The default ports, cookie and wallet follow the network. `status`, `audit` and `install` compare what bitcoind and the mints report against the configured network and fail on a mismatch (bitcoind's `chain`; the mints' `network` in `/health` and `/audit`).

## Start order, and the sync

`start`: write configs, start bitcoind if it is not answering, wait for it to finish its initial block download, start psp-mint, start esp-mint.

**Why wait, and not start psp-mint and let it report not-ready.** psp-mint scans the wallet through the node, locks the price at the first confirmation it sees and settles at the configured depth. On a node that is behind it would see an old part of the chain: payments arrive late (no harm), but it would also be answering `/health` ok, and the ESP mint and the wallets point at it from the moment it is up. Not starting it means nothing is exposed that is behind. The cost is that on a fresh mainnet node `start` can take days; that is bounded (`bitcoind.sync_wait_secs`, 600 by default), and when it runs out `start` says how far the node is and starts neither mint. bitcoind is left running and `start` is run again. `status` reports the same state ("not ready: bitcoind syncing 63%") for a psp-mint that was started some other way, such as by a service manager, so the two paths agree on what "not ready" means.

**bitcoind is only stopped by spi if spi started it** (a marker file in `run/`). One that was already running is never started over, stopped or restarted. `bitcoind.manage = false` makes spi never start one.

**What "syncing" means.** `initialblockdownload` true and either headers ahead of blocks or `verificationprogress` under 0.9999. The flag alone would keep a fresh regtest node "syncing" until its first block (Core keeps the flag while the tip is more than a day old); on a caught-up chain that is not syncing. Unreachable (no answer, bad cookie) and wrong chain are separate states and never reported as syncing.

Processes are started in their own process group, so Ctrl-C at the terminal that ran `start` does not stop them, with their output appended to `logs/<mint>.log` and the pid in `run/<mint>.pid`. Stopping is SIGTERM, as in the service definitions, and waits for the process to leave. `kill` is the `kill` command, to keep spi free of a libc dependency.

## Service files

`install` is the whole bootstrap of a machine that runs the mints as services. The operator-facing steps are in the README ("Running as services"); this is why it is built the way it is.

**Embedded templates, generated per `spi.toml`.** An earlier version copied the definitions out of an internal folder of the family, which a fresh machine does not have and which is not published. The definitions are now in `ops/` in this repository and compiled in with `include_str!`, so the binary alone is enough. They are templates, not copies: `{{name}}` stands where a value from `spi.toml` goes (binary folder, user, data, log and settings folders, bitcoind's datadir and arguments, the addresses, the chain). One change in `[service]` therefore reaches every file that mentions it, and there is no fixed `/usr/local/bin` or `spmint` left in the text. The expander is about 30 lines: a placeholder it does not know, or one that is not closed, is an error that names it, so a typo can not end up in a unit as literal text; in a property list the values are XML-escaped.

**`--ops` is an override and never required.** A file in the folder under the name `install` writes (`systemd/psp-mint.service`, `launchd/org.servicepoints.psp-mint.plist`, `sp-health-monitor.sh`, ...) replaces the embedded one and goes through the same expansion; a file that is not there falls back to the embedded one. The old definitions, which had no placeholders, still work this way unchanged. A folder that does not exist is an error and not a silent fall-back to the embedded text.

**Values are restricted instead of escaped.** A path, an account name or a bitcoind argument that goes into a unit may hold only letters, digits and `/ . _ + @ : = , -`. A space or a quote would need a different escape in systemd (which also gives `%` and `$` a meaning), in launchd's XML and in sh, and a unit with a wrongly escaped path is a service that does not start, or starts something else. Refusing the value, naming the setting, is simpler than being sure of three escapers. `bitcoind.datadir` on a Mac in `~/Library/Application Support` is the case that hurts, and it is only checked when spi puts it in a service (a `spi start` is not affected).

**A script and printed steps, not a `--system` flag.** `install` could have done the root work itself behind `--system` (create the account, copy the files, load the services). It does not, for these reasons:

- spi stays an ordinary user's tool. Running as root, or calling `sudo` from inside a program, makes the program's every bug a root bug, and `spi start`, `status` and `audit` should never be run that way; an install that needs root in the middle of a command that otherwise does not is easy to run wrongly.
- Creating accounts, writing under `/etc` and loading services are changes that should be read before they are made. A plain sh script of `install` and `dscl`/`useradd` lines can be read, kept, copied to another machine (it takes its files from its own folder) and run again, which an in-process step can not.
- It is testable. The tests build the folder and check every file and every line of the script, for both systems, without a system to damage. A `--system` flag could only be tested by running it, as root.
- No password or secret passes through spi. The admin token is never in `spi.toml` or in a generated file; the script installs an empty file for it that it never overwrites.

The cost is one more command, `sudo sh ~/.spi/service/install-system.sh`. A `--system` that runs exactly that script could be added later without changing the files; it is not here because it adds the first place spi runs as root.

**Idempotent, and what it keeps.** Running the script again replaces what spi generates (settings, env files, definitions, the monitor script, the binaries) and keeps what a person edits: `<mint>.secret.env` and `<mint>-health.conf` are installed only when they are absent. Env files could not hold the admin token if they were overwritten, and the monitor's tunables would be lost on every settings change. What the monitor watches (its URL, its name, its restart command) is not in the kept file: the definition sets it with `Environment=` (systemd) or `EnvironmentVariables` (launchd) from `spi.toml`, and the monitor already lets the environment win over its file, so an address that changes in `spi.toml` is not stale in a file that nobody regenerates.

**Paths and the service user.** The units run the binaries from `bin_dir`, to which the script copies them from where spi found them (`PATH`, or the path in `spi.toml`), because a service user can not read `~/.cargo/bin` and `ProtectHome` hides it. A binary spi can not find is a warning and a guard in the script (it warns when the file is missing), not a failure: the folder is useful on a machine that gets its binaries another way. `StateDirectory` is not used, as it only means `/var/lib`; the units name their data and log folders with `ReadWritePaths`, so `data_dir` can be anywhere, and the script makes the folders.

**One account for all services.** psp-mint has to read bitcoind's RPC cookie. With bitcoind under the same account, as the service writes it, the cookie needs no change of permissions and no copy. The cost is that a compromise of one service has the others' files; separating them would mean a group for the cookie and a second set of files, and was not worth the added moving parts here. The mints' own data is 0750 and their processes run with a 0077 umask.

**The service bitcoind, and its cookie.** When `bitcoind.manage` is true the script installs a unit for bitcoind: in the foreground (no `-daemon`, which would make `Type=simple` lose it), `-datadir=` from `bitcoind.datadir` or `<data_dir>/bitcoind`, the chain flag, `-rpcport=` only when the URL's port is not the chain's own, `bitcoind.args`, and `-rpccookieperms=group`. It is `sp-bitcoind.service` and not `bitcoind.service` so that it can not replace a unit that a package installed. psp-mint's service settings point at the cookie that this bitcoind writes (`<datadir>/<chain folder>/.cookie`) unless `bitcoind.cookie` was written down, in which case that path is used and a warning says so if it differs. The group-readable cookie is for the operator: `spi install` and `spi status` run as a person need to read it, and the alternative (running spi as root, or as the service user) puts spi in the root path the previous section avoids. It does give whoever is in the group the node's RPC password, and the README says so. With `manage = false`, spi writes no bitcoind unit and the cookie is whatever the config says; a cookie in a home folder gets a warning and `ProtectHome=read-only` on the psp-mint unit, since the default `yes` would hide it.

**An absent bitcoind is not a failure of the install that starts it.** On a fresh machine bitcoind is not running when `spi install` first runs, and failing there would make the bootstrap exit non-zero when it has worked. When service files are being written and spi manages bitcoind, an unreachable node is reported as "not running yet", and the step that sets up the wallet is left to a second `spi install --no-service-files` once it answers (printed in the steps). A node on the wrong chain still fails, and `--no-service-files` and `manage = false` keep the old rule, because nothing then starts the node.

**The services do not wait for the sync.** `spi start` holds psp-mint back until bitcoind has synced (see "Start order, and the sync"). A service manager can not do that with its ordering alone: `After=` waits for bitcoind to be started, not for its RPC, let alone its sync, and launchd has no ordering. The units therefore order the mints after bitcoind where they can, and rely on psp-mint's own behaviour and the restart policy for the rest, and `status` says "not ready" while the node syncs. This is the one place the service path is weaker than `start`, and the README says so. A unit that waited (`ExecStartPre` polling `getblockchaininfo`) would need `curl` or a helper in the unit and the cookie in it; not done.

**What is not tested.** The tests check the files and the script text and `sh -n` the script; none runs the script, systemd or launchd. See the README, "Tests".

## The audit

`audit` is `psp-mint GET /audit` and `esp-mint GET /audit`, combined.

- PSP outstanding is psp-mint's `outstanding` (whole PSP).
- ESP is esp-mint's `outstanding` (ESP units), shown as matured, unmatured and unsplit when esp-mint reports them (0.5.0 and later). All three are at full value. Nothing is discounted for waiting. Unsplit is ESP issued before esp-mint kept the per-day ledger; its day is unknown, so it is neither of the other two. The three must add up to `outstanding`, or the reply is refused, unless esp-mint says it has unfinished requests, which it has not counted yet (then a warning says so).
- FSP is left out. It is issued 1:1 against ESP that the FSP mint then holds, and that ESP is in esp-mint's outstanding; counting FSP too would count it twice.
- Every line is in its own unit, in PSP (`units / ESP_PER_PSP`, or the PSP count) and in USD (PSP / `PSP_PER_USD`, 10). They are exact decimals from `u128` integers. Each addition and the PSP to ESP scaling is checked, and an overflow is an error. The largest PSP count that fits is about 3.4 x 10^27, far beyond any real figure but not infinite.
- `PSP_PER_USD` is the one place the rate is held, with a comment that it must match the payout rate. The economics (what a payout costs, fees on payout, any surplus) are not here or in any repo.
- A network mismatch in an `/audit` reply, a non-200, or a mint that does not answer is an error: a report from a mint on another network or no report at all is never shown as zero.

### The esp-mint split (made in esp-mint 0.5.0)

esp-mint's `GET /audit` used to count issued and redeemed from mint-core's totals, which do not know the redeemable-after day. Matured and unmatured outstanding ESP needs a figure by terms (service and day), and it could not be derived from what esp-mint stored: an exchange, a transfer and a decline were stored in the same `done` shape, and a redemption's per-terms record covered only what left.

esp-mint 0.5.0 keeps a ledger by redeemable-after day (issued and redeemed), changed in the same atomic write that finishes each request, and `/audit` adds `outstanding_matured`, `outstanding_unmatured`, `outstanding_unsplit`, `requests_unfinished` and `as_of_day` (esp-mint's DESIGN.md, "The audit split", has the rules and the crash safety). spi reads them as they are; the day rule is esp-mint's (`Terms::is_redeemable_at`), not spi's, so spi does no date arithmetic.

What spi does with them:

- **Unsplit is shown, not hidden or spread.** A mint upgraded from an earlier version has ESP the ledger never saw. esp-mint can not say which day it belongs to, and spi does not guess. `ESP unsplit` is its own line, counted at full value in the total, with a warning that gives the amount. The warning goes away by itself as that ESP is redeemed, and a mint that ran 0.5.0 from the start never has it.
- **The parts must add up.** matured + unmatured + unsplit has to equal `outstanding`. A reply where it does not is an error, because a report whose lines do not add up to its total is wrong somewhere and should not be read as right.
- **The one excuse is an unfinished request.** esp-mint's counters move as each note is burned or signed, and its ledger when a request finishes, so for a moment the two differ. esp-mint says how many requests are unfinished. If any are, spi accepts the gap and warns; the total is still esp-mint's `outstanding`.
- **An older esp-mint** gives none of this: ESP is one figure and a warning says the split needs 0.5.0.

## Not done

- The real mints are not started in the tests (see the README).
- The health monitor is a shell script (the same one the mints have always used, embedded unchanged); spi has no monitor of its own.
- The service script has not been run on a real Linux or macOS machine by the tests; run a first install on a test machine.
- No fsp-mint service files: it belongs to the service that runs it.
- `start` and `stop` manage processes of the current user. They do not talk to launchd or systemd.
- fsp-mint is not managed: it belongs to the service that runs it, and has its own `audit` (the figure here leaves FSP out).
