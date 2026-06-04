"""Regressione: autorizzazione root per-progetto del token terminale.

Bug originale: un progetto registrato con `repository_root_path` fuori da
`projects_base_root` (es. /home/administrator/e2e-provider-test) faceva fallire
la verifica del token terminale lato brain -> close(4403) -> il frontend
riconnetteva all'infinito.

Fix: il brain autorizza il `root` firmato se coincide con una
`repository_root_path` registrata (fonte di verita': tabella projects), oltre
che col perimetro admin `_allowed_roots()`. La firma HMAC col secret resta la
garanzia di autenticita'; questi test coprono la sola logica di autorizzazione.
"""
from __future__ import annotations

import base64
import hashlib
import json
import time

from brain.grpc_server import main
from brain.grpc_server import runtime

_SECRET = "test-terminal-secret"


def _make_token(claims: dict) -> str:
    payload_json = json.dumps(claims).encode()
    payload_b64 = base64.urlsafe_b64encode(payload_json).rstrip(b"=").decode()
    signature = hashlib.sha256(f"{_SECRET}:{payload_b64}".encode()).hexdigest()
    return f"{payload_b64}.{signature}"


def _claims(root: str, cwd: str) -> dict:
    return {
        "sid": "sess-1",
        "uid": "user-1",
        "pid": "proj-1",
        "root": root,
        "cwd": cwd,
        "shell": "bash",
        "exp": int(time.time()) + 600,
    }


def _patch(monkeypatch, allowed_roots, registered_roots):
    # Le funzioni di sicurezza terminale vivono ora in brain.grpc_server.runtime
    # (main le re-esporta). Si patcha il modulo dove sono effettivamente
    # definite/chiamate: _verify_terminal_token risolve i nomi nel namespace runtime.
    monkeypatch.setattr(runtime, "_terminal_secret", lambda: _SECRET)
    monkeypatch.setattr(runtime, "_allowed_roots", lambda: list(allowed_roots))
    monkeypatch.setattr(runtime, "_registered_project_roots", lambda: set(registered_roots))


def test_registered_root_outside_perimeter_is_authorized(monkeypatch, tmp_path):
    """Il caso del bug: progetto registrato fuori dal perimetro -> autorizzato."""
    perimeter = tmp_path / "projects"
    perimeter.mkdir()
    project = tmp_path / "outside" / "e2e-provider-test"
    project.mkdir(parents=True)
    project_resolved = str(project.resolve())

    _patch(monkeypatch, [perimeter.resolve()], {project_resolved})

    token = _make_token(_claims(project_resolved, project_resolved))
    payload = main._verify_terminal_token(token)
    assert payload is not None
    assert payload["root"] == project_resolved


def test_unregistered_root_outside_perimeter_is_rejected(monkeypatch, tmp_path):
    """Sicurezza preservata: root non registrata e fuori perimetro -> rifiutata."""
    perimeter = tmp_path / "projects"
    perimeter.mkdir()
    rogue = tmp_path / "rogue"
    rogue.mkdir()
    rogue_resolved = str(rogue.resolve())

    _patch(monkeypatch, [perimeter.resolve()], set())

    token = _make_token(_claims(rogue_resolved, rogue_resolved))
    assert main._verify_terminal_token(token) is None


def test_root_within_perimeter_still_authorized(monkeypatch, tmp_path):
    """Retrocompat: progetto sotto projects_base_root resta autorizzato."""
    perimeter = tmp_path / "projects"
    proj = perimeter / "e2e-test2"
    proj.mkdir(parents=True)
    proj_resolved = str(proj.resolve())

    # Nessun progetto registrato: deve passare comunque tramite il perimetro.
    _patch(monkeypatch, [perimeter.resolve()], set())

    token = _make_token(_claims(proj_resolved, proj_resolved))
    assert main._verify_terminal_token(token) is not None


def test_tampered_signature_rejected(monkeypatch, tmp_path):
    """Una firma manomessa non e' mai accettata, root registrata o meno."""
    proj = tmp_path / "p"
    proj.mkdir()
    proj_resolved = str(proj.resolve())

    _patch(monkeypatch, [tmp_path.resolve()], {proj_resolved})

    token = _make_token(_claims(proj_resolved, proj_resolved))
    tampered = token[:-1] + ("0" if token[-1] != "0" else "1")
    assert main._verify_terminal_token(tampered) is None
