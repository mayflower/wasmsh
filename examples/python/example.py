"""
wasmsh Python Example

Demonstrates using the wasmsh shell runtime from Python.
Uses Node.js as the wasm runtime via the wasm-pack nodejs package.

Prerequisites:
    cd ../.. && wasm-pack build crates/wasmsh-browser --target nodejs \\
        --release --out-dir ../../pkg/nodejs
    npm install (in examples/typescript, to verify node works)

Run:
    python example.py
"""

import json
import subprocess
import sys
from pathlib import Path


class WasmShell:
    """Python wrapper around the wasmsh shell via Node.js subprocess."""

    def __init__(self) -> None:
        pkg_dir = Path(__file__).parent.parent.parent / "pkg" / "nodejs"
        if not (pkg_dir / "wasmsh_browser.js").exists():
            raise FileNotFoundError(
                f"wasmsh nodejs package not found at {pkg_dir}.\n"
                "Build it first: wasm-pack build crates/wasmsh-browser "
                "--target nodejs --release --out-dir ../../pkg/nodejs"
            )
        self._pkg_dir = str(pkg_dir.resolve())
        self._process: subprocess.Popen | None = None  # type: ignore[type-arg]
        self._start()

    def _start(self) -> None:
        node_code = (
            "const {WasmShell}=require('"
            + self._pkg_dir.replace("\\", "/")
            + "/wasmsh_browser.js');"
            "const rl=require('readline').createInterface({input:process.stdin});"
            "const s=new WasmShell();"
            "rl.on('line',l=>{"
            "try{const c=JSON.parse(l);let r;"
            "if(c.t==='i')r=s.init(BigInt(c.b||0),'[]');"
            "else if(c.t==='e')r=s.exec(c.v);"  # noqa: E501
            "else if(c.t==='w')r=s.write_file(c.p,new Uint8Array(c.d));"
            "else if(c.t==='r')r=s.read_file(c.p);"
            "else if(c.t==='l')r=s.list_dir(c.p);"
            "console.log(r);"
            "}catch(e){console.log(JSON.stringify([{Diagnostic:['Error',e.message]}]));}"
            "});"
        )
        self._process = subprocess.Popen(
            ["node", "-e", node_code],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

    def _send(self, cmd: dict) -> list:
        assert self._process and self._process.stdin and self._process.stdout
        self._process.stdin.write(json.dumps(cmd) + "\n")
        self._process.stdin.flush()
        line = self._process.stdout.readline().strip()
        if not line:
            return []
        return json.loads(line)

    def init(self, step_budget: int = 0) -> list:
        """Initialize the shell with a step budget (0 = unlimited)."""
        return self._send({"t": "i", "b": step_budget})

    def run(self, command: str) -> list:
        """Run a shell command. Returns list of event dicts."""
        return self._send({"t": "e", "v": command})

    def write_file(self, path: str, data: bytes) -> list:
        """Write bytes to a file in the virtual filesystem."""
        return self._send({"t": "w", "p": path, "d": list(data)})

    def read_file(self, path: str) -> list:
        """Read a file from the virtual filesystem."""
        return self._send({"t": "r", "p": path})

    def list_dir(self, path: str) -> list:
        """List a directory in the virtual filesystem."""
        return self._send({"t": "l", "p": path})

    def close(self) -> None:
        """Shut down the Node.js subprocess."""
        if self._process and self._process.stdin:
            self._process.stdin.close()
            self._process.wait()
            self._process = None


# -- Helpers --


def get_stdout(events: list) -> str:
    """Extract stdout text from shell events (decodes UTF-8 byte arrays)."""
    buf = bytearray()
    for evt in events:
        if "Stdout" in evt:
            buf.extend(evt["Stdout"])
    return buf.decode("utf-8", errors="replace")


def get_stderr(events: list) -> str:
    """Extract stderr text from shell events."""
    buf = bytearray()
    for evt in events:
        if "Stderr" in evt:
            buf.extend(evt["Stderr"])
    return buf.decode("utf-8", errors="replace")


def get_exit_code(events: list) -> int:
    """Extract exit code from shell events (-1 if not found)."""
    for evt in events:
        if "Exit" in evt:
            return evt["Exit"]
    return -1


def sh(shell: WasmShell, cmd: str) -> str:
    """Run a command and return stdout text (convenience)."""
    return get_stdout(shell.run(cmd))


# -- Main --


def main() -> None:
    shell = WasmShell()

    # Initialize
    init_events = shell.init(0)
    for evt in init_events:
        if "Version" in evt:
            print(f"wasmsh protocol version: {evt['Version']}")
    print()

    # 1. Basic commands
    print("=== 1. Basic Commands ===")
    print(sh(shell, "echo 'Hello from wasmsh!'").rstrip())
    print(sh(shell, "echo one; echo two; echo three").rstrip())
    print()

    # 2. Pipelines
    print("=== 2. Pipelines ===")
    print(sh(shell, "echo 'banana apple cherry' | tr ' ' '\n' | sort").rstrip())
    print()

    # 3. Variables and parameter expansion
    print("=== 3. Variables & Expansion ===")
    shell.run('NAME="World"')
    print(sh(shell, 'echo "Hello, ${NAME}!"').rstrip())
    print(sh(shell, 'FILE="/path/to/report.tar.gz"; echo "${FILE##*/}"').rstrip())
    print()

    # 4. Arrays
    print("=== 4. Arrays ===")
    shell.run("fruits=(apple banana cherry mango)")
    print(sh(shell, 'echo "count: ${#fruits[@]}"').rstrip())
    print(sh(shell, 'for f in "${fruits[@]}"; do echo "  - $f"; done').rstrip())
    print()

    # 5. Arithmetic
    print("=== 5. Arithmetic ===")
    fib = sh(
        shell,
        'a=0; b=1; for ((i=0; i<10; i++)); do echo -n "$a "; '
        "((tmp=a+b, a=b, b=tmp)); done; echo",
    )
    print(f"Fibonacci: {fib.rstrip()}")
    print(f"Bitwise: {sh(shell, 'echo $(( 0xFF & 0x0F )) $(( 1 << 8 ))').rstrip()}")
    print()

    # 6. Virtual filesystem
    print("=== 6. Virtual Filesystem ===")
    shell.write_file(
        "/data/config.toml",
        b'[server]\nhost = "localhost"\nport = 8080\n',
    )
    print(sh(shell, "cat /data/config.toml").rstrip())
    shell.run("mkdir -p /data/logs")
    shell.run('echo "2024-01-01 INFO started" > /data/logs/app.log')
    shell.run('echo "2024-01-02 ERROR failed" >> /data/logs/app.log')
    print(f"Grep: {sh(shell, 'grep ERROR /data/logs/app.log').rstrip()}")
    print()

    # 7. Functions
    print("=== 7. Functions ===")
    shell.run(
        "factorial() { local n=$1; if (( n <= 1 )); then echo 1; "
        "else local sub=$(factorial $((n-1))); echo $((n * sub)); fi; }"
    )
    print(f"10! = {sh(shell, 'echo $(factorial 10)').rstrip()}")
    print()

    # 8. Extended test [[ ]]
    print("=== 8. Extended Test [[ ]] ===")
    print(
        sh(
            shell,
            'file="report.csv"; if [[ $file == *.csv ]]; then echo "$file is CSV"; fi',
        ).rstrip()
    )
    print()

    # 9. Error handling
    print("=== 9. Error Handling ===")
    events = shell.run("nonexistent_command 2>&1")
    print(f"Exit code: {get_exit_code(events)}")
    print()

    # 10. Utilities
    print("=== 10. Utilities ===")
    shell.write_file("/data/hello.txt", b"hello\n")
    print(f"MD5: {sh(shell, 'md5sum /data/hello.txt').rstrip()}")
    b64_result = sh(shell, "echo -n 'wasmsh' | base64").rstrip()
    print(f"Base64: {b64_result}")
    print()

    print("All examples completed successfully!")
    shell.close()


if __name__ == "__main__":
    main()
