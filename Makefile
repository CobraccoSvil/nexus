.PHONY: help dev dev-wsl dev-cloud dev-hybrid dev-onprem down stop status logs-wsl lint test build clean docker-up docker-down deploy

help:
	@echo "IDEAI — comandi disponibili"
	@echo ""
	@echo "Sviluppo locale (WSL):"
	@echo "  make dev-wsl          - Avvia stack completo in WSL"
	@echo "  make stop             - Ferma tutti i servizi locali"
	@echo "  make status           - Mostra stato servizi locali"
	@echo "  make logs-wsl svc=core - Tail log di un servizio (core|admin|chat|neural|webide)"
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
	@echo "Deploy:"
	@echo "  make deploy           - Build + restart tutti i servizi in locale"
	@echo ""
	@echo "Docker locale:"
	@echo "  make docker-up-local  - Avvia solo i Docker locali (redis, qdrant, monitoring)"
	@echo "  make docker-down-local - Ferma Docker locali"
	@echo "  make docker-up-cloud  - Start Docker services (cloud profile)"
	@echo "  make docker-down      - Stop Nexus Docker services"
	@echo "  make logs             - Tail Docker logs Nexus"

dev-wsl:
	@bash scripts/dev-wsl.sh

stop:
	@bash scripts/dev-wsl.sh stop

status:
	@bash scripts/dev-wsl.sh status

logs-wsl:
	@bash scripts/dev-wsl.sh logs $(svc)

docker-up-local:
	@docker compose -f docker-compose.local.yml up -d

docker-down-local:
	@docker compose -f docker-compose.local.yml down

deploy:
	@bash deploy/deploy-local.sh

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
