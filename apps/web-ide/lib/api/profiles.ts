import { API_BASE, fetchJson } from "./_shared";

// ── Profili utente (GPT/Gem style) ────────────────────────────────────────

export interface UserProfile {
  id: string;
  userId: string;
  name: string;
  description?: string;
  avatarEmoji: string;
  systemPrompt: string;
  defaultProvider?: string;
  defaultModel?: string;
  defaultAutomation?: "study" | "confirm" | "automatic";
  isDefault: boolean;
  isSystem: boolean;
  sourceTemplateKey?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface CreateProfilePayload {
  name: string;
  description?: string;
  avatarEmoji?: string;
  systemPrompt?: string;
  defaultProvider?: string;
  defaultModel?: string;
  defaultAutomation?: string;
}

export type UpdateProfilePayload = Partial<CreateProfilePayload>;

export async function getProfiles(): Promise<{ profiles: UserProfile[] }> {
  return fetchJson(`${API_BASE}/api/profiles`);
}

export async function createProfile(payload: CreateProfilePayload): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/profiles`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function updateProfile(id: string, payload: UpdateProfilePayload): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/profiles/${id}`, {
    method: "PUT",
    body: JSON.stringify(payload),
  });
}

export async function deleteProfile(id: string): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/profiles/${id}`, { method: "DELETE" });
}

export async function setDefaultProfile(id: string): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/profiles/${id}/default`, { method: "POST" });
}

// ── Admin profile management ────────────────────────────────────────────────

export async function adminListProfiles(): Promise<{ profiles: UserProfile[] }> {
  return fetchJson(`${API_BASE}/api/admin/profiles`);
}

export async function adminCreateProfile(payload: CreateProfilePayload): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/admin/profiles`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function adminUpdateProfile(id: string, payload: UpdateProfilePayload): Promise<UserProfile> {
  return fetchJson(`${API_BASE}/api/admin/profiles/${id}`, {
    method: "PUT",
    body: JSON.stringify(payload),
  });
}

export async function adminDeleteProfile(id: string): Promise<{ ok: boolean }> {
  return fetchJson(`${API_BASE}/api/admin/profiles/${id}`, { method: "DELETE" });
}

export interface GlobalMcpServer {
  id: string;
  name: string;
  description?: string;
  transport: string;
  scope: string;
  enabled: boolean;
}

export async function adminListUserProfiles(): Promise<{ profiles: (UserProfile & { userEmail?: string })[] }> {
  return fetchJson(`${API_BASE}/api/admin/user-profiles`);
}

export async function adminListGlobalMcpServers(): Promise<{ servers: GlobalMcpServer[] }> {
  return fetchJson(`${API_BASE}/api/admin/global-mcp-servers`);
}

export async function adminGetProfileMcpServers(profileId: string): Promise<{ servers: GlobalMcpServer[] }> {
  return fetchJson(`${API_BASE}/api/admin/profiles/${profileId}/mcp-servers`);
}

export async function adminSetProfileMcpServers(
  profileId: string,
  mcpServerIds: string[],
): Promise<{ ok: boolean; count: number }> {
  return fetchJson(`${API_BASE}/api/admin/profiles/${profileId}/mcp-servers`, {
    method: "PUT",
    body: JSON.stringify({ mcpServerIds }),
  });
}
