import { API_BASE, fetchJson } from "./_shared";

// ── Admin Users Management ─────────────────────────────────────────────────

export interface AdminUser {
  id: string;
  email: string;
  displayName: string;
  githubUsername?: string;
  avatarUrl?: string;
  role: "viewer" | "editor" | "admin";
  createdAt: string;
  lastActivityAt?: string;
}

export interface UserProject {
  projectId: string;
  projectName: string;
  role: string;
}

export interface AdminUserWithProjects extends AdminUser {
  projectCount: number;
  projects: UserProject[];
}

export interface ProjectMember {
  userId: string;
  email: string;
  displayName: string;
  githubUsername?: string;
  avatarUrl?: string;
  role: string;
  createdAt: string;
}

export async function listAdminUsers(page: number = 1, limit: number = 20): Promise<{ users: AdminUser[]; total: number; page: number; limit: number }> {
  return fetchJson(`${API_BASE}/api/admin/users?page=${page}&limit=${limit}`);
}

export async function getAdminUser(userId: string): Promise<AdminUserWithProjects> {
  return fetchJson(`${API_BASE}/api/admin/users/${userId}`);
}

export async function updateAdminUser(userId: string, data: { email?: string; displayName?: string }): Promise<AdminUser> {
  return fetchJson(`${API_BASE}/api/admin/users/${userId}`, {
    method: "PUT",
    body: JSON.stringify(data),
  });
}

export async function updateAdminUserRole(userId: string, role: string): Promise<AdminUser> {
  return fetchJson(`${API_BASE}/api/admin/users/${userId}/role`, {
    method: "PUT",
    body: JSON.stringify({ role }),
  });
}

export async function deleteAdminUser(userId: string): Promise<void> {
  await fetchJson(`${API_BASE}/api/admin/users/${userId}`, { method: "DELETE" });
}

export async function searchAdminUsers(query: string): Promise<{ users: AdminUser[] }> {
  return fetchJson(`${API_BASE}/api/admin/users/search?q=${encodeURIComponent(query)}`);
}

// ── Admin Projects Listing ─────────────────────────────────────────────────

export interface AdminProjectSummary {
  id: string;
  name: string;
  slug: string;
  ownerUserId: string;
  ownerEmail?: string | null;
  memberCount: number;
}

export async function listAdminProjects(): Promise<{ projects: AdminProjectSummary[] }> {
  return fetchJson(`${API_BASE}/api/admin/projects`);
}

// ── Admin Project Members ──────────────────────────────────────────────────

export async function getProjectMembers(projectId: string): Promise<{ projectId: string; members: ProjectMember[] }> {
  return fetchJson(`${API_BASE}/api/admin/projects/${projectId}/members`);
}

export async function addProjectMember(projectId: string, userId: string, role: string): Promise<ProjectMember> {
  return fetchJson(`${API_BASE}/api/admin/projects/${projectId}/members`, {
    method: "POST",
    body: JSON.stringify({ user_id: userId, role }),
  });
}

export async function updateProjectMember(projectId: string, userId: string, role: string): Promise<ProjectMember> {
  return fetchJson(`${API_BASE}/api/admin/projects/${projectId}/members/${userId}`, {
    method: "PUT",
    body: JSON.stringify({ role }),
  });
}

export async function removeProjectMember(projectId: string, userId: string): Promise<void> {
  await fetchJson(`${API_BASE}/api/admin/projects/${projectId}/members/${userId}`, { method: "DELETE" });
}

// ── Project Porting ──────────────────────────────────────────────────────────
// Nota: `PortDetail` e' dichiarata in due punti del client legacy con campi
// complementari (table/projectId/projectName/oldPath/newPath + id). Mantenute
// entrambe qui nello STESSO modulo cosi' il declaration merging di TypeScript
// produce l'identico tipo unificato di prima del refactor.

export interface PortDetail {
  id: string;
  table: string;
  projectId: string;
  projectName: string;
  oldPath: string;
  newPath: string;
}

export async function portProjects(
  oldBase: string,
  newBase: string,
  preview: boolean,
): Promise<{ details: PortDetail[]; workspacesUpdated: number; repositoriesUpdated: number; projectsBaseRootUpdated: boolean; error?: string }> {
  return fetchJson(`${API_BASE}/api/admin/port-projects`, {
    method: "POST",
    body: JSON.stringify({ old_base: oldBase, new_base: newBase, preview }),
  });
}

// ── Admin Project Porting (shape complementare, merge con PortDetail sopra) ──

export interface PortDetail {
  table: string;
  id: string;
  oldPath: string;
  newPath: string;
}

export interface PortProjectsResult {
  dryRun: boolean;
  projectsBaseRootUpdated: boolean;
  workspacesUpdated: number;
  repositoriesUpdated: number;
  details: PortDetail[];
  error?: string;
}
