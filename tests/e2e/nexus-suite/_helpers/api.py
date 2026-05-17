"""Thin HTTP wrapper con cookie JWT automatico."""
import json
import requests
from pathlib import Path
from .cfg import cfg


def _load_jwt() -> str:
    p = Path(cfg.jwt_path)
    if p.exists():
        return p.read_text(encoding="utf-8").strip()
    return ""


class _Api:
    def __init__(self):
        self.token = _load_jwt()
        self.session = requests.Session()
        if self.token:
            self.session.cookies.set("token", self.token)

    def post(self, base: str, path: str, json_body: dict | None = None, **kw):
        url = f"{base}{path}"
        return self.session.post(url, json=json_body, timeout=kw.pop("timeout", 30), **kw)

    def get(self, base: str, path: str, **kw):
        url = f"{base}{path}"
        return self.session.get(url, timeout=kw.pop("timeout", 30), **kw)

    def core_post(self, path: str, json_body: dict | None = None, **kw):
        return self.post(cfg.mcp_core_url, path, json_body, **kw)

    def core_get(self, path: str, **kw):
        return self.get(cfg.mcp_core_url, path, **kw)

    def brain_post(self, path: str, json_body: dict | None = None, **kw):
        return self.post(cfg.brain_url, path, json_body, **kw)

    def brain_get(self, path: str, **kw):
        return self.get(cfg.brain_url, path, **kw)

    def admin_get(self, path: str, **kw):
        return self.get(cfg.admin_service_url, path, **kw)


api = _Api()
