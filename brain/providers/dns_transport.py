"""Custom httpx transport che risolve i nomi host via dnspython.

Usato quando il resolver DNS di sistema non funziona (es. WSL2 senza DNS configurato).
Configurato tramite la setting 'network_dns_servers' nel database admin.
"""
from __future__ import annotations

import logging
from typing import Any

import httpx

logger = logging.getLogger(__name__)

# Istanza globale del transport (None = usa il default di httpx)
_dns_transport_instance: "DNSOverrideTransport | None" = None


class DNSOverrideTransport(httpx.AsyncHTTPTransport):
    """Transport httpx che risolve i hostname tramite dnspython con nameserver personalizzati.
    
    Permette di bypassare il resolver DNS di sistema (es. /etc/resolv.conf) 
    e usare nameserver specificati (es. 8.8.8.8).
    """

    def __init__(self, nameservers: list[str], **kwargs: Any) -> None:
        super().__init__(**kwargs)
        import dns.resolver
        self._dns_resolver = dns.resolver.Resolver(configure=False)
        self._dns_resolver.nameservers = nameservers
        self._dns_resolver.timeout = 5.0
        self._dns_resolver.lifetime = 10.0
        self._host_cache: dict[str, str] = {}
        logger.info("DNSOverrideTransport inizializzato con nameserver: %s", nameservers)

    def _resolve_host(self, host: str) -> str | None:
        """Risolve un hostname in IP. Prima tenta il resolver di sistema (/etc/hosts incluso),
        poi come fallback usa dnspython con nameserver personalizzati."""
        if host in self._host_cache:
            return self._host_cache[host]

        # Tentativo 1: resolver di sistema (legge /etc/hosts, NSS, systemd-resolved...)
        import socket as _sock
        try:
            ip = _sock.gethostbyname(host)
            self._host_cache[host] = ip
            logger.debug("DNS risolto via sistema: %s -> %s", host, ip)
            return ip
        except _sock.gaierror:
            pass

        # Tentativo 2: dnspython con nameserver personalizzati
        try:
            answers = self._dns_resolver.resolve(host, "A")
            ip = str(answers[0])
            self._host_cache[host] = ip
            logger.debug("DNS risolto via dnspython: %s -> %s", host, ip)
            return ip
        except Exception as e:
            logger.warning("Risoluzione DNS fallita per %s: %s", host, e)
            return None

    async def handle_async_request(self, request: httpx.Request) -> httpx.Response:
        host = request.url.host
        # httpx può restituire il host come bytes in alcune versioni — normalizza a str
        if isinstance(host, bytes):
            host = host.decode("ascii")

        # Se è già un IP, non fare nulla
        import socket as _socket
        for af in (_socket.AF_INET, _socket.AF_INET6):
            try:
                _socket.inet_pton(af, host)
                return await super().handle_async_request(request)
            except _socket.error:
                pass

        ip = self._resolve_host(host)
        if ip is not None:
            # Costruisci nuovo URL con IP
            new_url = request.url.copy_with(host=ip)
            # Aggiorna headers con Host originale
            new_headers = dict(request.headers)
            new_headers["host"] = host
            # SNI extension per il TLS handshake corretto
            new_extensions = {**request.extensions, "sni_hostname": host.encode("ascii")}
            # Clona la request con la nuova URL mantenendo lo stesso stream
            # In httpx, _content è il body già letto; stream è per body streaming
            new_request = request.__class__(
                method=request.method,
                url=new_url,
                headers=httpx.Headers(new_headers),
                extensions=new_extensions,
            )
            # Copia il stream dalla request originale
            new_request.stream = request.stream  # type: ignore[assignment]
            request = new_request

        return await super().handle_async_request(request)


def get_global_dns_transport() -> "DNSOverrideTransport | None":
    """Ritorna il transport DNS globale se configurato, altrimenti None."""
    return _dns_transport_instance


def configure_dns_transport(nameservers: list[str]) -> None:
    """Configura il transport DNS globale. Chiamato da _load_keys_from_db()."""
    global _dns_transport_instance
    if nameservers:
        _dns_transport_instance = DNSOverrideTransport(nameservers)
        logger.info("Transport DNS globale configurato: %s", nameservers)
    else:
        _dns_transport_instance = None
        logger.info("Transport DNS globale rimosso")
