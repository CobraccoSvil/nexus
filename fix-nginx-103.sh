#!/bin/bash
# Script per risolvere il routing Nginx su app01 (192.168.0.103)
# Eseguire: sudo bash fix-nginx-103.sh

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${GREEN}[fix] Risolvimento routing Nginx per nexus.cobracco.it${NC}"
echo ""

# Controlla privilegi
if [[ $EUID -ne 0 ]]; then
   echo -e "${RED}[error] Questo script richiede privilegi root (sudo)${NC}"
   exit 1
fi

# ============================================================
# 1. Verifica configurazione nginx
# ============================================================
echo -e "${YELLOW}[1] Verifica configurazione nginx...${NC}"

NGINX_CONF=""
if [ -f /etc/nginx/sites-enabled/nexus ]; then
    NGINX_CONF="/etc/nginx/sites-enabled/nexus"
    echo "  ✓ Trovato: $NGINX_CONF"
elif [ -f /etc/nginx/sites-available/nexus ]; then
    NGINX_CONF="/etc/nginx/sites-available/nexus"
    echo "  ✓ Trovato: $NGINX_CONF"
elif [ -f /etc/nginx/conf.d/nexus.conf ]; then
    NGINX_CONF="/etc/nginx/conf.d/nexus.conf"
    echo "  ✓ Trovato: $NGINX_CONF"
else
    echo -e "${RED}  ✗ Configurazione nexus non trovata!${NC}"
    exit 1
fi

# ============================================================
# 2. Backup della configurazione
# ============================================================
echo -e "${YELLOW}[2] Backup configurazione...${NC}"
BACKUP="/etc/nginx/$(basename $NGINX_CONF).backup.$(date +%s)"
cp "$NGINX_CONF" "$BACKUP"
echo "  ✓ Backup: $BACKUP"

# ============================================================
# 3. Aggiungi le regole per _next/static
# ============================================================
echo -e "${YELLOW}[3] Aggiunta regole per _next/static...${NC}"

# Controlla se le regole sono già presenti
if grep -q "/_next/static/" "$NGINX_CONF"; then
    echo -e "${YELLOW}  ⚠ Regole _next/static già presenti, salto...${NC}"
else
    # Inserisci prima di "location /" usando sed
    cat > /tmp/nginx_rules.txt << 'EOFNGINX'

    # Forward dei static files Next.js (AGGIUNTO AUTOMATICAMENTE)
    location /_next/static/ {
        proxy_pass http://localhost:3000/_next/static/;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # Cache per assets (questi cambiano con ogni build)
        proxy_no_cache 1;
        add_header Cache-Control "public, max-age=31536000, immutable";
    }

    # Forward degli altri _next resources
    location /_next/ {
        proxy_pass http://localhost:3000/_next/;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_no_cache 1;
    }
EOFNGINX

    # Inserisci le regole prima di "location /" (usa GNU sed)
    sed -i '/^[[:space:]]*location \/ {/i\' "$NGINX_CONF"
    sed -i '/^[[:space:]]*location \/ {/e cat /tmp/nginx_rules.txt' "$NGINX_CONF" || {
        echo -e "${YELLOW}  ⚠ Inserimento automatico fallito, inserimento manuale richiesto...${NC}"
        echo ""
        echo "Aggiungi questo blocco PRIMA di 'location / {' nel file:"
        cat /tmp/nginx_rules.txt
    }

    echo "  ✓ Regole aggiunte"
fi

# ============================================================
# 4. Test configurazione nginx
# ============================================================
echo -e "${YELLOW}[4] Test configurazione nginx...${NC}"
if nginx -t 2>&1 | grep -q "successful"; then
    echo "  ✓ Configurazione valida"
else
    echo -e "${RED}  ✗ Errore di configurazione!${NC}"
    echo "  Ripristino backup..."
    cp "$BACKUP" "$NGINX_CONF"
    exit 1
fi

# ============================================================
# 5. Reload nginx
# ============================================================
echo -e "${YELLOW}[5] Reload Nginx...${NC}"
systemctl reload nginx && echo "  ✓ Nginx ricaricato" || {
    echo -e "${RED}  ✗ Errore durante reload!${NC}"
    cp "$BACKUP" "$NGINX_CONF"
    exit 1
}

# ============================================================
# 6. Test del routing
# ============================================================
echo -e "${YELLOW}[6] Test routing...${NC}"
sleep 2

TEST_URL="https://192.168.0.103/_next/static/chunks/webpack-44b5d9f52c3bd1b7.js"
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -k -H "Host: nexus.cobracco.it" "$TEST_URL" 2>/dev/null || echo "000")

if [ "$HTTP_CODE" = "200" ]; then
    echo "  ✓ Routing funzionante (HTTP $HTTP_CODE)"
elif [ "$HTTP_CODE" = "404" ]; then
    echo -e "${YELLOW}  ⚠ File non trovato (HTTP 404) - verificare backend localhost:3000${NC}"
else
    echo -e "${YELLOW}  ⚠ Risposta inattesa (HTTP $HTTP_CODE)${NC}"
fi

echo ""
echo -e "${GREEN}[success] Fix completato!${NC}"
echo ""
echo "Prossimi step:"
echo "1. Verifica: curl -k https://nexus.cobracco.it/_next/static/chunks/webpack-44b5d9f52c3bd1b7.js"
echo "2. Se errore, controllare: tail -50 /var/log/nginx/error.log"
echo "3. Se backup necessario: cp $BACKUP $NGINX_CONF && systemctl reload nginx"
