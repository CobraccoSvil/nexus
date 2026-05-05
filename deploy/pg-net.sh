#!/bin/bash
PG_CONF=$(find /etc/postgresql -name postgresql.conf 2>/dev/null | head -1)
PG_HBA=$(find /etc/postgresql -name pg_hba.conf 2>/dev/null | head -1)
if [ -n "$PG_CONF" ]; then
  sed -i "s/#listen_addresses = .*/listen_addresses = '*'/" "$PG_CONF"
  echo "listen_addresses set"
fi
if [ -n "$PG_HBA" ]; then
  grep -q "192.168.0.0/24" "$PG_HBA" || echo "host all all 192.168.0.0/24 scram-sha-256" >> "$PG_HBA"
  echo "LAN access enabled"
fi
systemctl restart postgresql
echo "PG_NET_OK"
