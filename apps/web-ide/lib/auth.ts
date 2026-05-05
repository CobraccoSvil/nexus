const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

export interface User {
  id: string;
  email: string;
  display_name: string;
  github_username: string | null;
  avatar_url: string | null;
  role: string;
  githubConnected?: boolean;
  githubConnectionStatus?: string;
  githubScopes?: string[];
}

export async function fetchMe(): Promise<User | null> {
  try {
    const res = await fetch(`${API_BASE}/api/me`, { credentials: "include" });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

export async function logout(): Promise<void> {
  await fetch(`${API_BASE}/auth/logout`, { method: "POST", credentials: "include" });
  window.location.href = "/login";
}
