.PHONY: help dev dev-cloud dev-hybrid dev-onprem down lint test build clean docker-up docker-down deploy deploy-local \
        bootstrap deploy-all deploy-rust deploy-web deploy-brain deploy-gateway proxy-reload health logs-prod cleanup-old

# === Configurazione deploy produzione ========================================
# Override possibile: make deploy PROD_HOST=192.168.1.99
PROD_HOST  ?= 192.168.0.6
PROXY_HOST ?= 192.168.0.3
SSH_USER   ?= administrator
DEPLOY_DIR ?= /opt/ideai
PUBLIC_URL ?= https://nexus.cobracco.it

export PROD_HOST PROXY_HOST SSH_USER DEPLOY_DIR PUBLIC_URL

help:
	@echo "IDEAI — comandi disponibili"
	@echo ""
	@echo "Nexus LLM Gateway (profili):"
	@echo "  make dev              - Start all services (cloud profile)"
	@echo "  make dev-cloud        - Start cloud profile stack"
	@echo "  make dev-hybrid       - Start hybrid profile stack (requires GPU)"
	@echo "  make dev-onprem       - Start on-premise profile stack (requires GPU)"
	@echo "  make down             - Stop Nexus Docker services"
	@echo ""
	@echo "Build & test:"
	@echo "  make lint             - ESLint su tutti i package"
	@echo "  make typecheck        - TypeScript type check"
	@echo "  make test             - Test suite completa"
	@echo "  make build            - Build tutti i package"
	@echo "  make clean            - Pulisce dist/ e node_modules"
	@echo ""
	@echo "Deploy locale (Linux):"
	@echo "  make deploy-local     - Build + restart tutti i servizi in locale"
	@echo ""
	@echo "Deploy produzione ($(PROD_HOST) <- $(PROXY_HOST), $(PUBLIC_URL)):"
	@echo "  make bootstrap        - Setup one-shot di $(PROD_HOST) da zero"
	@echo "  make deploy           - Rebuild + restart TUTTO su $(PROD_HOST) (alias di deploy-all)"
	@echo "  make deploy-rust      - Solo binari Rust (mcp-core + microservizi)"
	@echo "  make deploy-web       - Solo web-ide (Next.js)"
	@echo "  make deploy-brain     - Solo Python brain"
	@echo "  make deploy-gateway   - Solo nexus-gateway (Node)"
	@echo "  make proxy-reload     - Aggiorna nginx su $(PROXY_HOST)"
	@echo "  make health           - Smoke test post-deploy"
	@echo "  make logs-prod        - Tail journalctl dei servizi su $(PROD_HOST)"
	@echo "  make cleanup-old      - DISMETTE il vecchio Nexus su $(PROXY_HOST) (irreversibile)"
	@echo ""
	@echo "Docker locale:"
	@echo "  make docker-up-local  - Avvia solo i Docker locali (redis, qdrant, monitoring)"
	@echo "  make docker-down-local - Ferma Docker locali"
	@echo "  make docker-up-cloud  - Start Docker services (cloud profile)"
	@echo "  make docker-down      - Stop Nexus Docker services"
	@echo "  make logs             - Tail Docker logs Nexus"

docker-up-local:
	@docker compose -f docker-compose.local.yml up -d

docker-down-local:
	@docker compose -f docker-compose.local.yml down

# Deploy locale (rinominato: era 'deploy' prima dei target produzione)
deploy-local:
	@bash deploy/deploy-local.sh

# === Deploy produzione =======================================================

bootstrap:
	@bash deploy/bootstrap-prod.sh

deploy: deploy-all

deploy-all:
	@bash deploy/deploy-prod.sh --all

deploy-rust:
	@bash deploy/deploy-prod.sh --rust

deploy-web:
	@bash deploy/deploy-prod.sh --web

deploy-brain:
	@bash deploy/deploy-prod.sh --brain

deploy-gateway:
	@bash deploy/deploy-prod.sh --gateway

proxy-reload:
	@bash deploy/reload-proxy.sh

health:
	@bash deploy/health-check.sh

logs-prod:
	@ssh $(SSH_USER)@$(PROD_HOST) 'sudo journalctl -fu nexus-core -u nexus-webide -u nexus-neural'

cleanup-old:
	@bash deploy/cleanup-old-host.sh

dev: docker-up-cloud
	@pnpm dev

dev-cloud: docker-up-cloud
	@NEXUS_PROFILE=cloud pnpm dev

dev-hybrid: docker-up-hybrid
	@NEXUS_PROFILE=hybrid pnpm dev

dev-onprem: docker-up-onprem
	@NEXUS_PROFILE=onprem pnpm dev

docker-up-cloud:
	@echo "Starting cloud profile stack..."
	@cd infra/docker && docker-compose -f docker-compose.cloud.yml up -d

docker-up-hybrid:
	@echo "Starting hybrid profile stack..."
	@cd infra/docker && docker-compose -f docker-compose.hybrid.yml up -d

docker-up-onprem:
	@echo "Starting on-premise profile stack..."
	@cd infra/docker && docker-compose -f docker-compose.onprem.yml up -d

down: docker-down
	@echo "Stopped"

docker-down:
	@echo "Stopping Docker services..."
	@cd infra/docker && docker-compose -f docker-compose.cloud.yml down || true
	@cd infra/docker && docker-compose -f docker-compose.hybrid.yml down || true
	@cd infra/docker && docker-compose -f docker-compose.onprem.yml down || true

lint:
	@pnpm lint

typecheck:
	@pnpm typecheck

test:
	@pnpm test

build:
	@pnpm build

clean:
	@find . -name node_modules -type d -exec rm -rf {} + 2>/dev/null || true
	@find . -name dist -type d -exec rm -rf {} + 2>/dev/null || true
	@rm -rf .turbo/

logs:
	@cd infra/docker && docker-compose logs -f

ps:
	@cd infra/docker && docker-compose ps

.DEFAULT_GOAL := help
