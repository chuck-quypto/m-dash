# Native lifecycle regression; requires a graphical Ubuntu session, xdotool and wmctrl.
import os, signal, subprocess, time, sys, tempfile
from pathlib import Path

root = Path(__file__).resolve().parent.parent
(root / '.local/evidence').mkdir(parents=True, exist_ok=True)
fixture = tempfile.TemporaryDirectory(prefix='no-tray-', dir=root / '.local')
home = Path(fixture.name)
runtime = home / 'runtime'
runtime.mkdir(mode=0o700)
binary = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else root / 'src-tauri/target/debug/m-dash'
def windows():
    r = subprocess.run(['xdotool','search','--name','^m-dash · AI resources$'],capture_output=True,text=True)
    return set(r.stdout.split())
before = windows()
env = {**os.environ, 'HOME':str(home), 'XDG_DATA_HOME':str(home/'data'), 'XDG_CONFIG_HOME':str(home/'config'),
       'XDG_RUNTIME_DIR':str(runtime), 'GNOME_KEYRING_CONTROL':str(runtime/'keyring'), 'CLAUDE_CONFIG_DIR':str(home/'claude')}
with open(root/'.local/evidence/no-tray-native.log','w') as log:
    process = subprocess.Popen(['dbus-run-session','--', str(binary)], env=env,
                               stdout=log, stderr=log, start_new_session=True)
    window = None
    try:
        for _ in range(100):
            for candidate in windows()-before:
                pid = int(subprocess.check_output(['xdotool','getwindowpid',candidate]))
                # GNOME's separate frame client has the same title as the app.
                if os.getpgid(pid) == process.pid:
                    window = candidate
                    break
            if window:break
            time.sleep(.1)
        assert window, 'Native no-tray fixture did not open its own app window'
        assert os.getpgid(pid) == process.pid, 'Refuse to close a process outside the fixture'
        subprocess.run(['wmctrl','-ic',hex(int(window))],check=True)
        time.sleep(1)
        assert process.poll() == 0, 'Closing without a tray must quit, rather than leave a hidden collector'
        print('PASS real native close without a tray exits cleanly')
    finally:
        try:os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:pass
        process.wait(timeout=5)

fixture.cleanup()
