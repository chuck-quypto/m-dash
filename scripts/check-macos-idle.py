"""Native regression check: an idle installed dashboard must not spin its UI loop.

Run with the PID after the app has launched and finished its initial refresh.
This is host evidence, not part of the platform-independent source tests.
"""
import json
import subprocess
import sys
import time

pid = int(sys.argv[1])


def cpu_seconds():
    value = subprocess.check_output(
        ["ps", "-p", str(pid), "-o", "time="], text=True
    ).strip()
    return sum(float(part) * 60 ** index for index, part in enumerate(reversed(value.split(":"))))


before = cpu_seconds()
start = time.monotonic()
time.sleep(3)
elapsed = time.monotonic() - start
used = cpu_seconds() - before
result = {"pid": pid, "wallSeconds": round(elapsed, 2), "cpuSeconds": round(used, 2), "passed": used < elapsed / 6}
print(json.dumps(result))
assert result["passed"], "Idle m-dash is consuming sustained CPU; inspect its native event loop"
