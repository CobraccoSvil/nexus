"""Compile .proto files into Python gRPC stubs."""
import subprocess
import sys
from pathlib import Path

PROTO_DIR = Path(__file__).resolve().parent.parent.parent / "proto"
OUT_DIR = Path(__file__).resolve().parent / "generated"


def main() -> None:
    OUT_DIR.mkdir(exist_ok=True)
    (OUT_DIR / "__init__.py").touch()

    protos = list(PROTO_DIR.glob("*.proto"))
    if not protos:
        print("No .proto files found in", PROTO_DIR)
        sys.exit(1)

    cmd = [
        sys.executable, "-m", "grpc_tools.protoc",
        f"--proto_path={PROTO_DIR}",
        f"--python_out={OUT_DIR}",
        f"--grpc_python_out={OUT_DIR}",
        *[str(p) for p in protos],
    ]
    print("Running:", " ".join(cmd))
    subprocess.check_call(cmd)

    # Post-process: i file `*_pb2_grpc.py` generati da grpc_tools usano
    # import assoluti top-level (`import foo_pb2`), che non funzionano
    # quando la cartella `generated/` non e' in sys.path. Riscriviamo in
    # import relativi di pacchetto.
    import re

    for path in OUT_DIR.glob("*_pb2_grpc.py"):
        text = path.read_text()
        new_text = re.sub(
            r"^import (\w+_pb2) as (\w+)$",
            r"from . import \1 as \2",
            text,
            flags=re.MULTILINE,
        )
        if new_text != text:
            path.write_text(new_text)
            print(f"Patched imports in {path.name}")
    print(f"Generated stubs in {OUT_DIR}")


if __name__ == "__main__":
    main()
