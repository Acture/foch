# EU4 directed pacts

This build contains the pact library, a Foch DLL entry point and native test
fixtures. **It does not activate gameplay.** By default the DLL reports `refused`, reason
`201`: verified war access, script dispatch and campaign save integration are
not connected. Installing the DLL does not create alliances or join wars.

The separate `dev.foch.directed-pacts.roster-probe` package, with explicit
`read_only_roster_probe = true`, installs the monthly callback to read war
rosters. The native entry checks this actual host-provided plugin ID; the
normal pact identity always refuses diagnostic activation. The probe's
`active` status means only
the read diagnostic is installed; pact gameplay remains unavailable. It
requires the exact pinned executable digest and the original hook gate. The
default remains refused. Set `probe_case`, `probe_request` and `probe_wars`
to correlate observations; their defaults are `roster`, `1` and
`HAB:FRA,ENG:FRA`. `probe_wars` accepts up to four original attacker/defender
pairs. No observations are emitted until every requested pair exists, and
ambiguous matches fail the diagnostic. This mode never calls a war-entry or
flag-writing function.

For the probe fixture, set `probe_case = "two_rosters"`, `probe_request = "1"`
and `probe_country_flag = "FRA:foch_dp_roster_flag"`. The fixture sets that flag
using a game script effect. With this option the diagnostic waits for the
flag too, then emits its actual name, country and stored hour value, plus the
campaign GUID and day read from the game context. These candidate fields
remain unverified in a game session; the GUID alone does not identify a save
or prove persistence. The flag option defaults to empty (disabled), accepts
one requested war principal and a flag key of at most 64 bytes, and provides
no flag mutation or command consumption.

`probe_request` is the first observation number. Each configured war uses a
distinct consecutive number, followed by the optional flag observation. The
fixture's `tests/probe.txt` requires three `expect_receipt` results with request
numbers `1`, `2` and `3` from the actual diagnostic plugin identity. It also
keeps both game war assertions and the game flag assertion. Missing or refused
plugins cannot satisfy these receipt checks; a read receipt still does not
prove war entry or save persistence.

Package the diagnostic with the normal package tool's `--roster-probe` switch.
It uses a separate manifest and `eu4_directed_pacts_roster_probe.dll` filename;
the ordinary package remains `dev.foch.directed-pacts`. The one-case
`fixtures/probe` mod establishes two wars and waits 35 days for the monthly
read. Use the Foch test runner with a playset containing the diagnostic
package. A diagnostic `active` event alone does not prove that either the
fixture or the field read occurred.

The library supports proposals, recipient acceptance or rejection, signatory
termination and exclusive expiry dates. A proposal names three distinct
countries. It records each directed obligation explicitly; reciprocal terms
create two obligations. Pending and active overlapping obligations are rejected.
Signatories require verified independent foreign policy. A missing country or
changed tag ends the pact. Existing wars at acceptance are excluded.

Attack cooperation and defense cooperation each name one target country.
For example, FRA and SWE can agree to cooperate defensively against HAB.
That agreement covers only a qualifying defensive war in which HAB actually
fights on the opposing side. An ENG attack on FRA without HAB does not trigger
it, even if HAB is fighting a separate war elsewhere. HAB fighting alongside
FRA does not trigger it either. Fulfillment selects that exact war; it never
joins all of FRA's defensive wars.

Attack terms match wars personally declared by the beneficiary. Defense terms
match wars declared directly on it. The target must currently participate on
the opposite side, and may be a secondary participant. Wars involving the
beneficiary only through a later allied call are outside this first rule.
Every matching obligation requests one exact war and side through
`foch_runtime::war::WarBridge`. Native eligibility, including truces and subject
restrictions, must be checked by the provider at actual entry. Unknown
eligibility is a refusal. The library never uses broad war-joining effects or
permanent alliances.

Save payloads include consent, terms and successful or blocked attempts. A
successful war identity is processed once across payload restoration. Payloads
from another campaign, future game state or invalid saved terms are rejected.
The caller must embed the payload in the matching game save; a shared JSON
sidecar cannot provide this guarantee. Actual EU4 save/reload is unverified.
Books are limited to 1,024 pacts and payloads to 1 MiB; callers must handle an
oversized-payload error before committing a save.

The command parser accepts `propose SWE HAB defense 365 [reciprocal]`, `accept 1`,
`reject 1`, `end 1` and `list`. The caller supplies the actual acting country.
The script flag form is `foch_dp_cmd_propose_SWE_HAB_defense_365`. These are
library/adapter contracts; the current DLL does not consume these flags and
there is no working in-game player command. AI acceptance is not implemented.

The adapter request form `foch_dp_req_7__case_name__COMMAND` carries a case and
positive request number; `COMMAND` uses underscores in place of spaces. Receipt
payloads use `FOCH_RECEIPT/1`, string values and the exact recorded war identity.
Successful ledger replay records zero native entry calls. These helpers are
not yet connected to engine dispatch or the host log callback.

Build and run the focused offline checks from the repository root:

```text
cargo test -p foch-plugin-directed-pacts
cargo build -p foch-plugin-directed-pacts
uv run --locked --project src/tools/foch-dev python plugins/eu4-directed-pacts/tools/package.py target/debug/foch_plugin_directed_pacts.dll target/directed-pacts-package
```

The package command requires a new output directory. It reuses the public Foch
maintenance package to check actual PE exports without loading or executing the
DLL, rejects a host VERSION proxy and binds the DLL digest. The crate is
unpublished (`publish = false`). The host builds independently as
`foch_runtime_host.dll`; mixed workspace builds must retain the plugin entry
and keep all host proxies out of this plugin.

The eight `fixtures/runtime/events/pacts.txt` cases use Foch annotations, actual
war-identity assertions and positive plugin-result checks, so an absent plugin
cannot pass the negative cases by doing nothing. Collect or compile them using
Foch; these commands do not launch EU4:

```text
foch test plugins/eu4-directed-pacts/fixtures/runtime --collect-only
foch test plugins/eu4-directed-pacts/fixtures/runtime --no-run --out target/directed-pacts-bundles
```

The Rust fixture test runs collection, planning and compilation through
`foch-test`; it does not simulate game logs or claim a gameplay verdict. Real
execution remains blocked on the native bridge and an isolated plugin session
with game-side observation. The fixtures cover exact defensive war selection,
unrelated wars, offensive/defensive scope, repeated processing, subject refusal,
expiry and rejection. Actual game save/reload additionally needs framework
support. Do not use manual game launches as an acceptance substitute.
