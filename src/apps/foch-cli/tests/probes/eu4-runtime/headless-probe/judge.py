"""One-off EU4 probe controller; exit 0 pass, 1 failed check, 2 incomplete."""
import argparse
import json
from pathlib import Path
import re
import time

parser = argparse.ArgumentParser()
parser.add_argument("profile", type=Path)
parser.add_argument("--checks", nargs="+", required=True)
parser.add_argument("--wait", type=int, default=0)
parser.add_argument("--stop", action="store_true")
args = parser.parse_args()
deadline = time.monotonic() + args.wait
pattern = re.compile(r"EVENT \[([^]]+)\]:FOCH_CI_(BEGIN|END|PASS|FAIL)(?: ([a-zA-Z0-9_]+))?\s*$")
while True:
    log_path = args.profile / "logs/game.log"
    content = log_path.read_text(errors="replace") if log_path.exists() else ""
    records = [m.groups() for line in content.splitlines() if (m := pattern.search(line))]
    completed = any(kind == "END" for _, kind, _ in records)
    started = any(kind == "BEGIN" for _, kind, _ in records)
    passed = [name for _, kind, name in records if kind == "PASS"]
    failed = [name for _, kind, name in records if kind == "FAIL"]
    missing = sorted(set(args.checks) - set(passed) - set(failed))
    if completed or time.monotonic() >= deadline:
        break
    time.sleep(1)
code = 1 if failed else 0 if started and completed and not missing else 2
result = {
    "status": {0: "pass", 1: "fail", 2: "incomplete"}[code],
    "exit_code": code,
    "started": started,
    "completed": completed,
    "passed": passed,
    "failed": failed,
    "missing": missing,
    "records": records,
    "evaluated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "termination": "controller-requested" if args.stop else "not-requested",
}
(args.profile / "probe-result.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result))
if args.stop:
    (args.profile / "controller-stop").touch()
raise SystemExit(code)
