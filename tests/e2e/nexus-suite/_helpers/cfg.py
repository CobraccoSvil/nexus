"""Configurazione condivisa per gli E2E test."""
import os
from dataclasses import dataclass


@dataclass(frozen=True)
class Cfg:
    mcp_core_url: str = os.environ.get("MCP_CORE_URL", "http://localhost:4000")
    brain_url: str = os.environ.get("BRAIN_URL", "http://localhost:8001")
    web_ide_url: str = os.environ.get("WEB_IDE_URL", "http://localhost:3000")
    admin_service_url: str = os.environ.get("ADMIN_SERVICE_URL", "http://localhost:4010")
    database_url: str = os.environ.get(
        "DATABASE_URL", "postgres://nexus:nexus@localhost:5433/nexus"
    )
    jwt_path: str = os.environ.get("NEXUS_TEST_JWT_PATH", "/tmp/nexus_jwt.txt")
    # Timeout massimo per scenari long-running (default 8 min).
    scenario_timeout_s: int = int(os.environ.get("NEXUS_E2E_TIMEOUT", "480"))
    # Provider che ci aspettiamo siano funzionanti (almeno uno).
    expected_providers: tuple = tuple(
        (os.environ.get("NEXUS_E2E_PROVIDERS") or "deepseek,mistral").split(",")
    )


cfg = Cfg()
