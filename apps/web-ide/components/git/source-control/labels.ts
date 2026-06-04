import type {
  GitHubAccountStatus,
  GitHubRemoteStatus,
} from "../../../lib/api-client";

export function accountTone(status?: GitHubAccountStatus["status"]) {
  if (status === "connected") return "success";
  if (status === "upgrade_required") return "warning";
  if (status === "reconnect_required") return "error";
  return "neutral";
}

export function accountLabel(account?: GitHubAccountStatus | null) {
  if (!account) return "Caricamento stato GitHub...";
  if (account.status === "connected") {
    return `Connesso a GitHub come ${account.username ?? "account GitHub"}`;
  }
  if (account.status === "upgrade_required") {
    return `Permessi GitHub da aggiornare${account.username ? ` per ${account.username}` : ""}`;
  }
  if (account.status === "reconnect_required") {
    return `Connessione GitHub da riconfermare${account.username ? ` per ${account.username}` : ""}`;
  }
  return "Connetti GitHub per usare publish branch e pull request";
}

export function remoteReasonLabel(status?: GitHubRemoteStatus | null) {
  if (!status) return "Caricamento stato remote...";
  if (status.reason === "github_https") return "Remote GitHub HTTPS rilevato";
  if (status.reason === "non_github_remote") return "Remote non GitHub: restano disponibili solo operazioni Git locali";
  if (status.reason === "ssh_remote_unsupported") {
    return "Remote SSH rilevato: publish branch e pull request GitHub non sono supportati in v1";
  }
  if (status.reason === "missing_origin_remote") return "Nessun remote origin configurato";
  if (status.reason === "not_git_repo") return "Il progetto non e' un repository Git";
  return "Stato remote non disponibile";
}

export function readinessTone(isReady: boolean) {
  return isReady ? "success" : "warning";
}
