import { API_BASE, fetchJson } from "./_shared";

export interface GitFileChange {
  path: string;
  stagedStatus: string;
  worktreeStatus: string;
  kind: string;
  staged: boolean;
  unstaged: boolean;
  untracked: boolean;
}

export interface GitRepositoryState {
  isGitRepo: boolean;
  currentBranch?: string;
  staged: GitFileChange[];
  unstaged: GitFileChange[];
  untracked: GitFileChange[];
}

export interface GitBranchInfo {
  name: string;
  isCurrent: boolean;
  upstream?: string;
}

export interface GitLogEntry {
  commit: string;
  shortCommit: string;
  author: string;
  date: string;
  subject: string;
  body?: string;
}

export interface GitDiffResponse {
  path: string;
  staged: boolean;
  diff: string;
}

export interface GitHubAccountStatus {
  username?: string | null;
  avatarUrl?: string | null;
  status: "connected" | "upgrade_required" | "reconnect_required" | "not_connected";
  connected: boolean;
  scopes: string[];
  expiresAt?: string | null;
}

export interface GitHubPullRequestSummary {
  number: number;
  htmlUrl: string;
  title: string;
  state: string;
}

export interface GitHubRemoteStatus {
  available: boolean;
  reason: string;
  remoteName?: string;
  remoteUrl?: string;
  owner?: string;
  repo?: string;
  repoFullName?: string;
  branch?: string;
  upstream?: string;
  ahead: number;
  behind: number;
  published: boolean;
  defaultBranch?: string;
  canPushPull: boolean;
  suggestedPrTitle?: string;
  lastCommitTitle?: string;
  pullRequest?: GitHubPullRequestSummary | null;
  apiError?: string;
}

export interface GitHubRepositorySummary {
  id: number;
  name: string;
  fullName: string;
  ownerLogin: string;
  htmlUrl: string;
  cloneUrl: string;
  private: boolean;
  defaultBranch: string;
  updatedAt: string;
}

export interface GitUiPreferences {
  showHunkMap: boolean;
}

export async function getGitStatus(
  projectId: string,
): Promise<{ projectId: string; canManageGit: boolean; git: GitRepositoryState }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/status`);
}

export async function getGitBranches(projectId: string): Promise<{ branches: GitBranchInfo[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/branches`);
}

export async function getGitLog(projectId: string): Promise<{ entries: GitLogEntry[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/log`);
}

export async function getGitDiff(
  projectId: string,
  path: string,
  staged = false,
): Promise<GitDiffResponse> {
  const url = new URL(`${API_BASE}/api/projects/${projectId}/git/diff`, typeof window !== "undefined" ? window.location.origin : "http://localhost");
  url.searchParams.set("path", path);
  if (staged) {
    url.searchParams.set("staged", "true");
  }
  return fetchJson(url.toString());
}

export async function getGitUiPreferences(projectId: string): Promise<GitUiPreferences> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/preferences/git-ui`);
}

export async function updateGitUiPreferences(
  projectId: string,
  showHunkMap: boolean,
): Promise<{ ok: boolean; showHunkMap: boolean }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/preferences/git-ui`, {
    method: "PUT",
    body: JSON.stringify({ show_hunk_map: showHunkMap }),
  });
}

export async function stageGitPaths(projectId: string, paths: string[]) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/stage`, {
    method: "POST",
    body: JSON.stringify({ paths }),
  });
}

export async function unstageGitPaths(projectId: string, paths: string[]) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/unstage`, {
    method: "POST",
    body: JSON.stringify({ paths }),
  });
}

export async function commitGit(projectId: string, message: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/commit`, {
    method: "POST",
    body: JSON.stringify({ message }),
  });
}

export async function createGitBranch(projectId: string, name: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/branch`, {
    method: "POST",
    body: JSON.stringify({ name }),
  });
}

export async function checkoutGitBranch(projectId: string, name: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/checkout`, {
    method: "POST",
    body: JSON.stringify({ name }),
  });
}

export async function pullGit(projectId: string, remote?: string, branch?: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/pull`, {
    method: "POST",
    body: JSON.stringify({ remote, branch }),
  });
}

export async function pushGit(projectId: string, remote?: string, branch?: string) {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/git/push`, {
    method: "POST",
    body: JSON.stringify({ remote, branch }),
  });
}

export async function getGitHubAccount(): Promise<{ account: GitHubAccountStatus }> {
  return fetchJson(`${API_BASE}/api/github/account`);
}

export async function listUserGitHubRepositories(): Promise<{ repositories: GitHubRepositorySummary[] }> {
  return fetchJson(`${API_BASE}/api/github/repositories`);
}

export async function connectGitHub(
  returnTo?: string,
): Promise<{ url: string }> {
  return fetchJson(`${API_BASE}/api/github/connect`, {
    method: "POST",
    body: JSON.stringify({ returnTo }),
  });
}

export async function disconnectGitHub(): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/github/account`, {
    method: "DELETE",
  });
}

export async function getProjectGitHubStatus(
  projectId: string,
): Promise<{ github: GitHubRemoteStatus }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/status`);
}

export async function listProjectGitHubRepositories(
  projectId: string,
): Promise<{ repositories: GitHubRepositorySummary[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/repositories`);
}

export async function cloneProjectGitHubRepository(
  projectId: string,
  payload: { cloneUrl: string },
): Promise<{ ok: boolean; repository: { owner: string; repo: string; cloneUrl: string }; git: GitRepositoryState }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/clone`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function publishGitHubBranch(
  projectId: string,
): Promise<{ ok: boolean; git: GitRepositoryState }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/publish-branch`, {
    method: "POST",
  });
}

/** Orchestrazione completa: git init (se serve) + commit + crea repo GitHub + push.
    Per progetti nuovi NON gia' versionati o gia' git ma senza remote.
    Vedere github.rs::github_publish_project. */
export async function publishProjectToGitHub(
  projectId: string,
  payload: { name: string; description?: string; private?: boolean; commitMessage?: string },
): Promise<{
  ok: boolean;
  // Backend ritorna snake_case (json! macro su valori non-struct).
  full_name: string;
  html_url: string;
  clone_url: string;
  private: boolean;
  default_branch: string;
  pushed: boolean;
}> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/publish`, {
    method: "POST",
    body: JSON.stringify({
      name: payload.name,
      description: payload.description ?? "",
      private: payload.private ?? true,
      commit_message: payload.commitMessage ?? "Initial commit (Nexus)",
    }),
  }, 90_000);
}

export async function createGitHubPullRequest(
  projectId: string,
  payload: { title: string; body?: string; baseBranch?: string },
): Promise<{ created: boolean; pullRequest: GitHubPullRequestSummary }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/github/pull-request`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}
