"""Shared daemon infrastructure for workers and coordinator.

Provides fork-based daemonization, PID file management, status heartbeat,
and log-file-based Rich console output. Both worker and coordinator daemons
use these primitives.
"""

from __future__ import annotations

import atexit
import json
import os
import signal
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

from rich.console import Console


def start_daemon(
    name: str,
    hive_dir: Path,
    target: Callable[..., Any],
    args: tuple = (),
) -> int:
    """Fork a daemon process, write PID file, redirect output to log.

    Args:
        name: Daemon name (e.g. "workers/42-backend" or "coordinator").
        hive_dir: Path to .hive directory.
        target: Function to call in the daemon process.
        args: Arguments to pass to target.

    Returns:
        Child PID.
    """
    daemon_dir = hive_dir / name
    daemon_dir.mkdir(parents=True, exist_ok=True)

    pid_path = daemon_dir / "pid"
    log_path = daemon_dir / "output.log"
    status_path = daemon_dir / "status.json"

    # Check if already running
    if is_daemon_running(name, hive_dir):
        existing = _read_pid(pid_path)
        raise RuntimeError(
            f"Daemon '{name}' already running (pid={existing})"
        )

    # First fork — detach from parent
    pid = os.fork()
    if pid > 0:
        # Parent: wait briefly for child to write PID, then return
        # The child will double-fork; we return the intermediate PID
        # but the real daemon PID is written to the PID file.
        _wait_for_pid_file(pid_path, timeout=3.0)
        real_pid = _read_pid(pid_path)
        return real_pid if real_pid else pid

    # First child: create new session
    os.setsid()

    # Second fork — prevent zombie and ensure no controlling terminal
    pid2 = os.fork()
    if pid2 > 0:
        os._exit(0)  # Intermediate child exits

    # Daemon process (grandchild)
    daemon_pid = os.getpid()

    # Write PID file
    pid_path.write_text(str(daemon_pid))

    # Register cleanup
    def _cleanup():
        try:
            pid_path.unlink(missing_ok=True)
            write_status(status_path, "stopped")
        except OSError:
            pass

    atexit.register(_cleanup)

    # Redirect stdout/stderr to log file
    log_file = open(log_path, "a")  # noqa: SIM115
    os.dup2(log_file.fileno(), sys.stdout.fileno())
    os.dup2(log_file.fileno(), sys.stderr.fileno())

    # Write initial status
    write_status(status_path, "starting", pid=daemon_pid)

    # Run the target
    try:
        target(*args)
    except Exception as e:
        sys.stderr.write(f"Daemon {name} crashed: {e}\n")
        write_status(status_path, "crashed", pid=daemon_pid, error=str(e))
    finally:
        _cleanup()
        os._exit(0)

    return daemon_pid  # unreachable, keeps type checker happy


def stop_daemon(name: str, hive_dir: Path) -> bool:
    """Send SIGTERM to daemon via PID file.

    Returns True if signal was sent successfully.
    """
    daemon_dir = hive_dir / name
    pid_path = daemon_dir / "pid"
    pid = _read_pid(pid_path)
    if pid is None:
        return False

    try:
        os.kill(pid, signal.SIGTERM)
        # Wait for process to exit (up to 5 seconds)
        for _ in range(50):
            try:
                os.kill(pid, 0)
                time.sleep(0.1)
            except OSError:
                break
        # Clean up PID file if process is gone
        try:
            os.kill(pid, 0)
        except OSError:
            pid_path.unlink(missing_ok=True)
        return True
    except OSError:
        # Process doesn't exist — clean up stale PID file
        pid_path.unlink(missing_ok=True)
        return False


def daemon_status(name: str, hive_dir: Path) -> dict[str, Any]:
    """Read status.json + check if PID is alive."""
    daemon_dir = hive_dir / name
    status_path = daemon_dir / "status.json"
    pid_path = daemon_dir / "pid"

    info: dict[str, Any] = {"name": name, "running": False}

    # Read status file
    if status_path.exists():
        try:
            with open(status_path) as f:
                info.update(json.load(f))
        except (json.JSONDecodeError, OSError):
            pass

    # Check if PID is actually alive
    pid = _read_pid(pid_path)
    if pid is not None:
        info["pid"] = pid
        try:
            os.kill(pid, 0)
            info["running"] = True
        except OSError:
            info["running"] = False
            info["state"] = "dead"
    else:
        info["running"] = False
        if info.get("state") not in ("stopped", "crashed"):
            info["state"] = "not_started"

    return info


def write_status(status_path: Path, state: str, **extra: Any) -> None:
    """Write status.json atomically."""
    data = {
        "state": state,
        "updated_at": datetime.now(timezone.utc).isoformat(),
        **extra,
    }
    # Write to temp file then rename for atomicity
    tmp_path = status_path.with_suffix(".tmp")
    try:
        with open(tmp_path, "w") as f:
            json.dump(data, f)
        tmp_path.replace(status_path)
    except OSError:
        # Fall back to direct write
        try:
            with open(status_path, "w") as f:
                json.dump(data, f)
        except OSError:
            pass


def is_daemon_running(name: str, hive_dir: Path) -> bool:
    """Check PID file + os.kill(pid, 0)."""
    daemon_dir = hive_dir / name
    pid_path = daemon_dir / "pid"
    pid = _read_pid(pid_path)
    if pid is None:
        return False
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        # Stale PID file — clean up
        pid_path.unlink(missing_ok=True)
        return False


def setup_daemon_logging(log_path: Path) -> Console:
    """Create Rich Console writing to log file instead of stdout."""
    log_file = open(log_path, "a")  # noqa: SIM115
    return Console(file=log_file, highlight=False, force_terminal=True)


class StatusHeartbeat:
    """Periodically updates status.json so clients know daemon is alive."""

    def __init__(self, status_path: Path, interval: float = 30.0):
        self._status_path = status_path
        self._interval = interval
        self._last_update = 0.0
        self._state = "idle"
        self._extra: dict[str, Any] = {}

    def set_state(self, state: str, **extra: Any) -> None:
        """Update state and write immediately."""
        self._state = state
        self._extra = extra
        self._write()

    def tick(self) -> None:
        """Call periodically — writes status if interval has elapsed."""
        now = time.monotonic()
        if now - self._last_update >= self._interval:
            self._write()

    def _write(self) -> None:
        self._last_update = time.monotonic()
        write_status(self._status_path, self._state, **self._extra)


def install_signal_handlers(
    cleanup: Callable[[], None] | None = None,
) -> None:
    """Install SIGTERM/SIGINT handlers for clean daemon shutdown."""
    def _handler(signum: int, frame: Any) -> None:
        if cleanup:
            cleanup()
        sys.exit(0)

    signal.signal(signal.SIGTERM, _handler)
    signal.signal(signal.SIGINT, _handler)


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _read_pid(pid_path: Path) -> int | None:
    """Read PID from file, returning None if missing or invalid."""
    if not pid_path.exists():
        return None
    try:
        text = pid_path.read_text().strip()
        return int(text)
    except (ValueError, OSError):
        return None


def _wait_for_pid_file(pid_path: Path, timeout: float = 3.0) -> None:
    """Wait for PID file to appear (used by parent after fork)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pid_path.exists():
            return
        time.sleep(0.05)
