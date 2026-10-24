#!/usr/bin/env bash
# One-command compose launcher with optional host-ACP auto-detection.
#
# - copies the tracked .example files to the ignored working files when missing
# - detects a globally installed Dsh on the Docker host
# - generates docker-compose.host-agent.yml so the container can run the host
#   Dsh; the web host then registers "Dsh" automatically (no UI setup)
# - pulls the GHCR image and starts the stack
#
# Usage: ./scripts/docker-up.sh [prod]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="${1:-dev}"
COMPOSE_FILE="docker-compose.yml"
if [[ "$MODE" == "prod" ]]; then
  COMPOSE_FILE="docker-compose.prod.yml"
fi

for pair in "docker-compose.yml:docker-compose.yml.example" \
            "docker-compose.prod.yml:docker-compose.prod.yml.example" \
            ".env:.env.example"; do
  dst="${pair%%:*}"
  src="${pair##*:}"
  if [[ ! -f "$dst" ]]; then
    cp "$src" "$dst"
    echo "created $dst from $src"
  fi
done

mkdir -p vault data

compose_args=(-f "$COMPOSE_FILE")
shopt -s nullglob
if [[ -f docker-compose.host-agent.yml ]]; then
    compose_args+=(-f docker-compose.host-agent.yml)
  elif command -v dsh >/dev/null 2>&1; then
    dsh_real="$(readlink -f "$(command -v dsh)")"
    dsh_pkg=""
    case "$dsh_real" in
      */@deepseek-ai/dsh/lib/bin.js)
        dsh_pkg="${dsh_real%/dsh/lib/bin.js}"
        ;;
      */dsh/lib/bin.js)
        # Fallback for a package outside an @deepseek-ai scope.
        dsh_pkg="${dsh_real%/lib/bin.js}"
        ;;
    esac
    dsh_home="${DSH_HOME:-$HOME/.dsh}"
    if [[ -n "$dsh_pkg" && -d "$dsh_pkg" && -d "$dsh_home" && -f "$dsh_real" ]]; then
      cp docker-compose.host-agent.yml.example docker-compose.host-agent.yml
      if ! grep -q '^AGENTERO_HOST_DSH_PKG=' .env; then
        {
          echo
          echo "# Auto-detected by scripts/docker-up.sh"
          echo "AGENTERO_HOST_DSH_PKG=$dsh_pkg"
          echo "AGENTERO_DSH_HOME=$dsh_home"
        } >> .env
      fi
      compose_args+=(-f docker-compose.host-agent.yml)
      echo "host Dsh detected: $dsh_real"
      echo "wrote docker-compose.host-agent.yml (agent auto-registers as Dsh)"
    else
      echo "host Dsh found but its package/home could not be resolved; starting without it"
    fi
  else
    echo "no host dsh found; starting without a host ACP agent"
fi

docker compose "${compose_args[@]}" pull
docker compose "${compose_args[@]}" up -d --no-build
