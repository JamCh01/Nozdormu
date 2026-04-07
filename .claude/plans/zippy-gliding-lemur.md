# Fix Windows Service Installation with WinSW

## Context

Windows install script (`downloads/install.ps1`) uses `New-Service` to register `netpulse-agent.exe` directly as a Windows service. This fails because Windows SCM requires binaries to implement the Service Control interface (`ServiceMain`, `HandlerEx` callbacks). The agent binary is a regular executable, so `Start-Service` throws `CouldNotStartService`.

Linux/macOS scripts work fine because systemd (`Type=simple`) and launchd can wrap any executable natively.

## Solution

Replace the `New-Service` approach with **WinSW** (Windows Service Wrapper) — a single-file service wrapper that bridges regular executables to the Windows SCM.

## Changes

### 1. Modify `downloads/install.ps1` (lines 84-116)

Replace the entire "Install as Windows Service" section:

**Remove:**
- `New-Service` call (lines 97-101)
- Registry `Environment` MultiString hack (lines 103-112)
- Direct `Start-Service` call (line 115)

**Add:**
1. Download `WinSW-net461.exe` from GitHub releases to `$InstallDir`
2. Rename it to `NetPulseAgent.exe` (WinSW requires config filename to match binary name)
3. Generate `NetPulseAgent.xml` with:
   - Service identity: `id=NetPulseAgent`, display name, description
   - `<executable>` pointing to `netpulse-agent.exe`
   - `<env>` elements for all 5 environment variables (replaces registry hack)
   - `<log mode="roll-by-size">` with 10MB rotation, 10 files
   - `<onfailure action="restart" delay="5 sec"/>` (matches systemd `RestartSec=5`)
   - `<startmode>Automatic</startmode>`
4. If existing service: `NetPulseAgent.exe stop` + `NetPulseAgent.exe uninstall`
5. Run `NetPulseAgent.exe install` + `NetPulseAgent.exe start`

**File layout after install:**
```
C:\Program Files\NetPulse\
    NetPulseAgent.exe       # WinSW wrapper
    NetPulseAgent.xml       # WinSW config
    netpulse-agent.exe      # actual agent binary
    logs\                   # stdout/stderr logs (auto-rotated)
```

### 2. No changes needed to other files

- `router.py:_build_install_command` — the PowerShell one-liner format is unchanged (it downloads and runs `install.ps1`)
- `test_router.py` — install command tests don't need changes
- `install.sh` — Linux/macOS script is unaffected

## WinSW Download

- URL: `https://github.com/winsw/winsw/releases/download/v3.0.0-alpha.11/WinSW-net461.exe`
- Variant: `net461` — targets .NET Framework 4.6.1, pre-installed on Windows 10/11 and Server 2016+
- No additional runtime dependencies

## Verification

1. Read the modified `install.ps1` and verify the WinSW download, XML generation, and service commands are correct
2. Run `uv run ruff format downloads/install.ps1` — N/A (PowerShell, not Python)
3. Run existing tests: `uv run pytest tests/modules/agents/test_router.py -v` — should still pass (install command format unchanged)
4. Manual test on a Windows machine:
   - Run the install command
   - Verify `NetPulseAgent` service appears in `services.msc`
   - Verify service starts and stays running
   - Verify `logs/` directory has output
