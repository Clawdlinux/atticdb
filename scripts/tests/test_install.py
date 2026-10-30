"""Tests for install.sh. Each test serves fake releases from 127.0.0.1 and installs into tmp dirs.

The real HOME is never touched: HOME, ATTICDB_INSTALL_DIR and TMPDIR all point under tmp_path.
"""

import functools
import hashlib
import http.server
import io
import os
import platform
import shutil
import subprocess
import tarfile
import threading
from pathlib import Path

import pytest

INSTALL = Path(__file__).resolve().parents[2] / "install.sh"
# Enough for curl, tar, awk, shasum and sysctl. Leaves out any real atticdb on the caller's PATH.
SYSTEM_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"
FAKE = b'#!/bin/sh\necho "atticdb 9.9.9"\n'


def host_target():
    """The target install.sh picks on this host, found the same way."""
    system, machine = platform.system(), platform.machine()
    if system == "Darwin" and machine in ("arm64", "aarch64"):
        return "aarch64-apple-darwin"
    if system == "Darwin" and machine == "x86_64":
        out = subprocess.run(["sysctl", "-n", "sysctl.proc_translated"],
                             capture_output=True, text=True, check=False).stdout.strip()
        return "aarch64-apple-darwin" if out == "1" else "x86_64-apple-darwin"
    if system == "Linux" and machine == "x86_64":
        return "x86_64-unknown-linux-musl"
    if system == "Linux" and machine in ("aarch64", "arm64"):
        return "aarch64-unknown-linux-musl"
    pytest.skip(f"install.sh has no target for {system} {machine}", allow_module_level=True)


TARBALL = f"atticdb-{host_target()}.tar.gz"


def tarball(script=FAKE):
    """A release tarball: a top-level atticdb that prints its version, plus a README."""
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w:gz") as tar:
        for name, data, mode in (("atticdb", script, 0o755), ("README.md", b"# AtticDB\n", 0o644)):
            info = tarfile.TarInfo(name)
            info.size, info.mode = len(data), mode
            tar.addfile(info, io.BytesIO(data))
    return out.getvalue()


def publish(release_dir, sums=None, script=FAKE):
    """Writes the tarball and SHA256SUMS into release_dir, like the release workflow."""
    release_dir.mkdir(parents=True)
    data = tarball(script)
    (release_dir / TARBALL).write_bytes(data)
    if sums is None:
        sums = f"{hashlib.sha256(data).hexdigest()}  {TARBALL}\n"
    (release_dir / "SHA256SUMS").write_text(sums)


@pytest.fixture
def server(tmp_path):
    """Serves tmp_path/www on 127.0.0.1. Yields (root dir, base URL, list of requested paths)."""
    root, requests = tmp_path / "www", []
    root.mkdir()

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args):
            requests.append(self.path)

    httpd = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0), functools.partial(Handler, directory=str(root)))
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield root, f"http://127.0.0.1:{httpd.server_address[1]}/releases", requests
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join(timeout=10)


@pytest.fixture
def dirs(tmp_path):
    """HOME, the install dir and TMPDIR, all under tmp_path. Only HOME and TMPDIR exist."""
    home, tmp = tmp_path / "home", tmp_path / "tmp"
    home.mkdir()
    tmp.mkdir()
    return home, tmp_path / "bin", tmp


def install(dirs, base, path=SYSTEM_PATH, shell="sh", **env):
    """Runs install.sh with a clean env. A None value leaves that variable unset."""
    home, bin_dir, tmp = dirs
    full = {"PATH": path, "HOME": str(home), "TMPDIR": str(tmp),
            "ATTICDB_INSTALL_DIR": str(bin_dir), "ATTICDB_DOWNLOAD_BASE": base}
    full.update(env)
    full = {key: value for key, value in full.items() if value is not None}
    return subprocess.run([shell, str(INSTALL)], env=full, capture_output=True, text=True,
                          timeout=60, check=False)


def assert_installed(dirs, result):
    home, bin_dir, tmp = dirs
    assert result.returncode == 0, result.stderr
    exe = bin_dir / "atticdb"
    assert exe.read_bytes() == FAKE
    assert exe.stat().st_mode & 0o777 == 0o755
    assert sorted(os.listdir(bin_dir)) == ["atticdb"], "no temp file left beside it"
    assert f"installed atticdb 9.9.9 at {exe}" in result.stdout
    assert "  atticdb setup" in result.stdout
    assert os.listdir(tmp) == [], "the temp dir is removed"
    assert os.listdir(home) == [], "nothing written to HOME"


def assert_nothing_installed(dirs, result):
    _, bin_dir, tmp = dirs
    assert result.returncode != 0, result.stdout
    assert not bin_dir.exists()
    assert os.listdir(tmp) == [], "the temp dir is removed"


def test_latest(server, dirs):
    root, base, requests = server
    publish(root / "releases/latest/download")
    result = install(dirs, base)
    assert_installed(dirs, result)
    assert requests == [f"/releases/latest/download/{TARBALL}",
                        "/releases/latest/download/SHA256SUMS"]
    # The install dir is not on PATH, so it prints the line to add.
    assert f"""  export PATH='{dirs[1]}':"$PATH"\n""" in result.stdout


@pytest.mark.parametrize("version", ["9.9.9", "v9.9.9"])
def test_pinned_version(server, dirs, version):
    root, base, requests = server
    publish(root / "releases/download/v9.9.9")
    result = install(dirs, base + "/", ATTICDB_VERSION=version)
    assert_installed(dirs, result)
    assert requests == [f"/releases/download/v9.9.9/{TARBALL}",
                        "/releases/download/v9.9.9/SHA256SUMS"]


def test_default_dir_is_home_local_bin(server, dirs):
    root, base, _ = server
    publish(root / "releases/latest/download")
    home = dirs[0]
    result = install(dirs, base, ATTICDB_INSTALL_DIR=None)
    assert result.returncode == 0, result.stderr
    assert (home / ".local/bin/atticdb").read_bytes() == FAKE


def test_checksum_mismatch_installs_nothing(server, dirs):
    root, base, _ = server
    publish(root / "releases/latest/download", sums=f"{'0' * 64}  {TARBALL}\n")
    result = install(dirs, base)
    assert_nothing_installed(dirs, result)
    assert f"checksum mismatch for {TARBALL}" in result.stderr


def test_missing_sums_entry_fails(server, dirs):
    root, base, _ = server
    publish(root / "releases/latest/download", sums=f"{'0' * 64}  atticdb-other.tar.gz\n")
    result = install(dirs, base)
    assert_nothing_installed(dirs, result)
    assert f"SHA256SUMS has no entry for {TARBALL}" in result.stderr


def test_missing_release_fails(server, dirs):
    _, base, _ = server
    result = install(dirs, base, ATTICDB_VERSION="1.2.3")
    assert_nothing_installed(dirs, result)
    assert "download failed" in result.stderr


def test_reinstall_replaces_the_file(server, dirs):
    root, base, _ = server
    publish(root / "releases/latest/download")
    bin_dir = dirs[1]
    bin_dir.mkdir()
    (bin_dir / "atticdb").write_bytes(b'#!/bin/sh\necho "atticdb 0.0.1"\n')
    (bin_dir / "atticdb").chmod(0o700)
    assert_installed(dirs, install(dirs, base))
    assert_installed(dirs, install(dirs, base))


def test_printed_export_line_keeps_an_odd_dir_literal(server, dirs, tmp_path):
    root, base, _ = server
    publish(root / "releases/latest/download")
    odd = tmp_path / "a $x `b` \"c\" \\d 'e'"
    result = install(dirs, base, ATTICDB_INSTALL_DIR=str(odd))
    assert result.returncode == 0, result.stderr
    assert (odd / "atticdb").read_bytes() == FAKE
    lines = [line for line in result.stdout.splitlines() if line.startswith("  export PATH=")]
    assert len(lines) == 1, result.stdout
    out = subprocess.run(["sh", "-c", f'{lines[0]}\nprintf %s "$PATH"'],
                         env={"PATH": SYSTEM_PATH, "x": "EXPANDED"}, capture_output=True,
                         text=True, timeout=10, check=True).stdout
    assert out == f"{odd}:{SYSTEM_PATH}"


def test_on_path_and_shadowed(server, dirs, tmp_path):
    root, base, _ = server
    publish(root / "releases/latest/download")
    bin_dir = dirs[1]
    result = install(dirs, base, path=f"{bin_dir}:{SYSTEM_PATH}")
    assert_installed(dirs, result)
    assert "export PATH" not in result.stdout and "warning" not in result.stdout

    other = tmp_path / "other"
    other.mkdir()
    (other / "atticdb").write_bytes(b"#!/bin/sh\n")
    (other / "atticdb").chmod(0o755)
    result = install(dirs, base, path=f"{other}:{bin_dir}:{SYSTEM_PATH}")
    assert_installed(dirs, result)
    assert f"warning: {other / 'atticdb'} comes first on PATH" in result.stdout


# dash's command -v prints $dir//atticdb for the PATH entry $dir/. That is the same file.
@pytest.mark.parametrize("shell", ["/bin/sh"] + [s for s in ("dash",) if shutil.which(s)])
def test_trailing_slash_on_path_is_not_a_shadow(server, dirs, shell):
    root, base, _ = server
    publish(root / "releases/latest/download")
    bin_dir = dirs[1]
    result = install(dirs, base, path=f"{bin_dir}/:{SYSTEM_PATH}", shell=shell)
    assert_installed(dirs, result)
    assert "warning" not in result.stdout and "export PATH" not in result.stdout


def test_binary_that_does_not_run_installs_nothing(server, dirs):
    root, base, _ = server
    publish(root / "releases/latest/download", script=b"#!/bin/sh\nexit 1\n")
    result = install(dirs, base)
    assert result.returncode == 1
    assert f"the downloaded atticdb did not run from {dirs[1]}" in result.stderr
    assert os.listdir(dirs[1]) == [], "the temp copy is removed"
    assert os.listdir(dirs[2]) == [], "the temp dir is removed"


def test_directory_at_the_target_fails(server, dirs):
    root, base, _ = server
    publish(root / "releases/latest/download")
    target = dirs[1] / "atticdb"
    target.mkdir(parents=True)
    (target / "keep").write_bytes(b"mine")
    result = install(dirs, base)
    assert result.returncode == 1, result.stdout
    assert f"{target} is a directory. Move it away" in result.stderr
    assert "installed" not in result.stdout
    assert os.listdir(dirs[1]) == ["atticdb"], "no temp copy left"
    assert os.listdir(target) == ["keep"] and (target / "keep").read_bytes() == b"mine"
    assert os.listdir(dirs[2]) == [], "the temp dir is removed"


def test_symlink_to_a_directory_at_the_target_fails(server, dirs, tmp_path):
    root, base, _ = server
    publish(root / "releases/latest/download")
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    dirs[1].mkdir()
    (dirs[1] / "atticdb").symlink_to(elsewhere)
    result = install(dirs, base)
    assert result.returncode == 1, result.stdout
    assert "is a directory. Move it away" in result.stderr
    assert os.listdir(dirs[1]) == ["atticdb"] and os.listdir(elsewhere) == []


def test_unsupported_platform_points_to_source(server, dirs, tmp_path):
    _, base, requests = server
    fake = tmp_path / "fake"
    fake.mkdir()
    uname = fake / "uname"
    uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo riscv64 ;; esac\n')
    uname.chmod(0o755)
    result = install(dirs, base, path=f"{fake}:{SYSTEM_PATH}")
    assert result.returncode == 1
    assert "no prebuilt binary for Linux riscv64" in result.stderr
    assert "cargo install --git https://github.com/Clawdlinux/atticdb atticdb --locked" \
        in result.stderr
    assert requests == [] and not dirs[1].exists()


def test_relative_install_dir_fails(server, dirs):
    _, base, requests = server
    result = install(dirs, base, ATTICDB_INSTALL_DIR="bin")
    assert result.returncode == 1
    assert "ATTICDB_INSTALL_DIR must be an absolute path" in result.stderr
    assert requests == []
