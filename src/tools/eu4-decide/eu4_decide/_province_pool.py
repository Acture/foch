"""Export the EU4 province graph into the flat pool format the env reads.

Input: target/eu4_provinces.json — {provinces: {id: {development, owner,
x, y, neighbors}}, straits: [[a, b], ...]} plus the game's own country
histories for each nation's capital.
Output: target/eu4_provinces.txt — one line per province, one strait per
line, one capital per line, everything whitespace-separated so the Rust
side parses it with `split_whitespace` and no JSON dependency.
"""
import json
import re
from pathlib import Path

# The game's country histories: files named `TAG - Name.txt`, each with a
# `capital = <province_id>` line naming that nation's capital province.
COUNTRIES = Path("G:/SteamLibrary/steamapps/common/Europa Universalis IV/history/countries")
# Anchored at line start so commented-out lines (`#capital = ...`) and
# `fixed_capital = ...` never match; tolerant of tabs and missing spaces.
CAPITAL = re.compile(r"^[ \t]*capital[ \t]*=[ \t]*([0-9]+)", re.MULTILINE)


def capitals() -> dict[str, int]:
	"""TAG -> capital province id, read from the game's own files.

	The first match per file is the start-of-game capital; dated moves
	further down a file never precede it. Unreadable or missing files
	(and a missing install) simply contribute nothing.
	"""
	found: dict[str, int] = {}
	for path in sorted(COUNTRIES.glob("*.txt")):
		tag = path.name[:3]
		if not re.fullmatch(r"[A-Z0-9]{3}", tag):
			continue
		try:
			text = path.read_text(encoding="utf-8", errors="replace")
		except OSError:
			continue
		if match := CAPITAL.search(text):
			found[tag] = int(match.group(1))
	return found


def main() -> None:
	root = Path(__file__).resolve().parents[4] / "target"
	source = root / "eu4_provinces.json"
	sink = root / "eu4_provinces.txt"
	data = json.loads(source.read_text(encoding="utf-8"))
	provinces = data["provinces"]
	straits = data.get("straits", [])

	ids = sorted(int(pid) for pid in provinces)
	lines = [f"PROVINCE_POOL {len(ids)}"]
	for pid in ids:
		entry = provinces[str(pid)]
		neighbors = " ".join(str(int(n)) for n in entry["neighbors"])
		owner = entry.get("owner") or "NONE"
		lines.append(
			f"{pid} {int(entry['development'])} {owner} "
			f"{float(entry['x']):.1f} {float(entry['y']):.1f} {neighbors}".rstrip()
		)
	lines.append(f"STRAITS {len(straits)}")
	for a, b in straits:
		lines.append(f"{int(a)} {int(b)}")
	caps = capitals()
	lines.append(f"CAPITALS {len(caps)}")
	for tag in sorted(caps):
		lines.append(f"{tag} {caps[tag]}")
	sink.write_text("\n".join(lines) + "\n", encoding="utf-8")
	print(f"{sink}: {len(ids)} provinces, {len(straits)} straits, {len(caps)} capitals")


if __name__ == "__main__":
	main()
