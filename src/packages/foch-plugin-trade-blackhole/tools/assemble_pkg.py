"""Assemble the trade-blackhole plugin package under target/plugin-pkg.

Copies the release DLL and the offline node-members table next to a fresh
foch-plugin.toml and fills the [[files]] sha256 digests, so
`foch plugin import` gets a self-contained native package. Run after
`cargo build -p foch-plugin-trade-blackhole --release --locked`.
"""

from __future__ import annotations

import hashlib
import shutil
from pathlib import Path

PACKAGE = Path(__file__).resolve().parent.parent
REPO = PACKAGE.parent.parent.parent
PKG = REPO / "target/plugin-pkg"
DLL = REPO / "target/release/foch_plugin_trade_blackhole.dll"
MEMBERS = PACKAGE / "tools/node_members.txt"
NAME = "trade_blackhole.dll"

MANIFEST = """\
schema = 1

[plugin]
id = "dev.foch.trade-blackhole"
name = "Dynamic Trade Blackhole"
version = "0.1.0"
authors = ["acturea"]
description = "Closes and reopens vanilla outgoing trade routes so trade converges toward wealthy nodes instead of the fixed vanilla end nodes."

[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "=1.37.5"
abi_major = 1

[entry]
kind = "native"
path = "{dll}"
phase = "entry"

[lifecycle]
restart_required = true
runtime_config = false
runtime_toggle = false

[config.enabled]
type = "bool"
default = true
description = "Master switch; false leaves the plugin an inert observer."

[config.mode]
type = "string"
default = "observe"
description = "observe, force_toggle or policy."

[config.force_node]
type = "string"
default = "Great Lakes"
description = "Node (display name) toggled in force_toggle mode."

[config.observer_log]
type = "bool"
default = true
description = "Write the per-node observer dump to the plugin log."

[config.sink_basis]
type = "string"
default = "mean"
description = "Sink signal from the members' tax: mean = per-province intensity, sum = raw mass."

[config.absorb_ref]
type = "int"
default = 3000
min = 100
max = 10000
description = "Tax intensity (thousandths) at which a node absorbs half of what pools in it; higher means only richer nodes sink."

[config.relay_ref]
type = "int"
default = 3000
min = 100
max = 10000
description = "Trade power (thousandths) at which a partner doubles its relay attractiveness; higher flattens hub preference."

[config.flow_epsilon_pct]
type = "int"
default = 5
min = 0
max = 50
description = "An edge whose net flow is below this percent of its source's pool counts as carrying nothing."

[config.reopen_pct]
type = "int"
default = 15
min = 1
max = 100
description = "A closed edge reopens only above this percent of the pool."

[config.evidence_months]
type = "int"
default = 6
min = 1
max = 60
description = "Consecutive months of sustained flow evidence before an edge closes or reopens."

[config.max_flips_total]
type = "int"
default = 16
min = 1
max = 256
description = "Edge flips the whole graph may apply per month."

[config.transit_fee]
type = "bool"
default = true
description = "Pay the transit fee: surplus nodes share a fee pool among the countries holding trade power there."

[config.transit_fee_pct]
type = "int"
default = 20
min = 0
max = 100
description = "The transit pool is this percent of the volume the node forwards for others."

[config.source_fee_pct]
type = "int"
default = 20
min = 0
max = 100
description = "The source bounty is this percent of the local production that leaves the node."
"""


def digest_of(path: Path) -> str:
	return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
	if not DLL.exists():
		raise SystemExit(f"missing {DLL}; build the crate first")
	if not MEMBERS.exists():
		raise SystemExit(f"missing {MEMBERS}; generate it first")
	PKG.mkdir(parents=True, exist_ok=True)
	for stale in PKG.iterdir():
		if stale.is_file():
			stale.unlink()
	shutil.copy2(DLL, PKG / NAME)
	shutil.copy2(MEMBERS, PKG / "node_members.txt")
	(PKG / "foch-plugin.toml").write_text(
		MANIFEST.format(dll=NAME)
		+ f'\n[[files]]\npath = "{NAME}"\nsha256 = "{digest_of(PKG / NAME)}"\n'
		+ f'\n[[files]]\npath = "node_members.txt"\nsha256 = "{digest_of(PKG / "node_members.txt")}"\n',
		encoding="utf-8",
		newline="\n",
	)
	print(f"package ready at {PKG}")
	print(f"  {NAME}: {digest_of(PKG / NAME)[:16]}...")
	print(f"  node_members.txt: {digest_of(PKG / 'node_members.txt')[:16]}...")


if __name__ == "__main__":
	main()
