"use client";

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";
import { useI18n } from "../../../lib/i18n";
import { formatDate } from "../../../lib/format";
import * as api from "../../../lib/api-client";
import { useGlobalDialog } from "../../../components/global-dialog-provider";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";

interface User {
  id: string;
  email: string;
  displayName: string;
  githubUsername?: string;
  avatarUrl?: string;
  role: "viewer" | "editor" | "admin";
  createdAt: string;
  projectCount?: number;
}

export default function UsersPage() {
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();
  const { t } = useI18n();
  const [users, setUsers] = useState<User[]>([]);
  const [loading, setLoading] = useState(true);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [error, setError] = useState<string | null>(null);
  const [selectedUser, setSelectedUser] = useState<User | null>(null);
  const [showRoleDialog, setShowRoleDialog] = useState(false);
  const [newRole, setNewRole] = useState<"viewer" | "editor" | "admin">("viewer");

  const loadUsers = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const response = await api.listAdminUsers(page, 20);
      setUsers(response.users);
      setTotal(response.total);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load users");
    } finally {
      setLoading(false);
    }
  }, [page]);

  useEffect(() => {
    loadUsers();
  }, [page, loadUsers]);

  const handleDeleteUser = async (userId: string) => {
    const ok = await confirmDialog(
      "Sei sicuro di voler eliminare questo utente?",
      "Elimina utente",
    );
    if (!ok) return;
    try {
      await api.deleteAdminUser(userId);
      loadUsers();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delete user");
    }
  };

  const handleChangeRole = async () => {
    if (!selectedUser) return;
    try {
      await api.updateAdminUserRole(selectedUser.id, newRole);
      setShowRoleDialog(false);
      setSelectedUser(null);
      loadUsers();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to update role");
    }
  };

  const handleSelectUser = (user: User) => {
    setSelectedUser(user);
    setNewRole(user.role);
    setShowRoleDialog(true);
  };

  const totalPages = Math.ceil(total / 20);

  return (
    <div>
      <AdminPageHeader title={t("admin.users")} description={t("admin.users.desc")} />

      {error && (
        <div
          style={{
            padding: "12px 16px",
            borderRadius: 8,
            background: "#fee",
            color: "#c00",
            fontSize: 13,
            marginBottom: 16,
          }}
        >
          {error}
        </div>
      )}

      <div
        style={{
          borderRadius: 12,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          overflow: "hidden",
        }}
      >
        {loading ? (
          <div style={{ padding: "40px 24px", textAlign: "center", color: tc.textMuted }}>
            Caricamento utenti...
          </div>
        ) : users.length === 0 ? (
          <div style={{ padding: "40px 24px", textAlign: "center", color: tc.textMuted }}>
            {t("admin.users.noUsers")}
          </div>
        ) : (
          <>
            <table style={{ width: "100%", borderCollapse: "collapse" }}>
              <thead>
                <tr
                  style={{
                    borderBottom: `1px solid ${tc.border}`,
                    background: tc.bgHover,
                  }}
                >
                  <th
                    style={{
                      padding: "12px 16px",
                      textAlign: "left",
                      fontWeight: 600,
                      fontSize: 12,
                      color: tc.textMuted,
                    }}
                  >
                    Email
                  </th>
                  <th
                    style={{
                      padding: "12px 16px",
                      textAlign: "left",
                      fontWeight: 600,
                      fontSize: 12,
                      color: tc.textMuted,
                    }}
                  >
                    Nome
                  </th>
                  <th
                    style={{
                      padding: "12px 16px",
                      textAlign: "left",
                      fontWeight: 600,
                      fontSize: 12,
                      color: tc.textMuted,
                    }}
                  >
                    Ruolo
                  </th>
                  <th
                    style={{
                      padding: "12px 16px",
                      textAlign: "left",
                      fontWeight: 600,
                      fontSize: 12,
                      color: tc.textMuted,
                    }}
                  >
                    Data Creazione
                  </th>
                  <th
                    style={{
                      padding: "12px 16px",
                      textAlign: "right",
                      fontWeight: 600,
                      fontSize: 12,
                      color: tc.textMuted,
                    }}
                  >
                    Azioni
                  </th>
                </tr>
              </thead>
              <tbody>
                {users.map((user) => (
                  <tr
                    key={user.id}
                    style={{
                      borderBottom: `1px solid ${tc.border}`,
                    }}
                  >
                    <td style={{ padding: "12px 16px", fontSize: 13, color: tc.text }}>
                      {user.email}
                    </td>
                    <td style={{ padding: "12px 16px", fontSize: 13, color: tc.text }}>
                      {user.displayName}
                    </td>
                    <td style={{ padding: "12px 16px", fontSize: 13, color: tc.text }}>
                      <span
                        style={{
                          padding: "4px 8px",
                          borderRadius: 4,
                          fontSize: 11,
                          fontWeight: 600,
                          background:
                            user.role === "admin"
                              ? "#fee"
                              : user.role === "editor"
                                ? "#eff"
                                : "#f5f5f5",
                          color:
                            user.role === "admin"
                              ? "#c00"
                              : user.role === "editor"
                                ? "#080"
                                : tc.textMuted,
                        }}
                      >
                        {user.role}
                      </span>
                    </td>
                    <td style={{ padding: "12px 16px", fontSize: 13, color: tc.textMuted }}>
                      {formatDate(user.createdAt)}
                    </td>
                    <td style={{ padding: "12px 16px", textAlign: "right" }}>
                      <button
                        onClick={() => handleSelectUser(user)}
                        style={{
                          padding: "4px 8px",
                          marginRight: 8,
                          background: tc.bgHover,
                          border: `1px solid ${tc.border}`,
                          borderRadius: 4,
                          fontSize: 12,
                          cursor: "pointer",
                          color: tc.text,
                        }}
                      >
                        {t("admin.users.edit")}
                      </button>
                      <button
                        onClick={() => handleDeleteUser(user.id)}
                        style={{
                          padding: "4px 8px",
                          background: "#fee",
                          border: "1px solid #fcc",
                          borderRadius: 4,
                          fontSize: 12,
                          cursor: "pointer",
                          color: "#c00",
                        }}
                      >
                        {t("admin.users.delete")}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>

            {totalPages > 1 && (
              <div
                style={{
                  padding: "12px 16px",
                  borderTop: `1px solid ${tc.border}`,
                  display: "flex",
                  justifyContent: "space-between",
                  alignItems: "center",
                  fontSize: 12,
                  color: tc.textMuted,
                }}
              >
                <span>
                  Pagina {page} di {totalPages}
                </span>
                <div>
                  <button
                    onClick={() => setPage(Math.max(1, page - 1))}
                    disabled={page === 1}
                    style={{
                      padding: "4px 8px",
                      marginRight: 8,
                      background: page === 1 ? tc.bgHover : tc.bgCard,
                      border: `1px solid ${tc.border}`,
                      borderRadius: 4,
                      cursor: page === 1 ? "default" : "pointer",
                      color: tc.text,
                      opacity: page === 1 ? 0.5 : 1,
                    }}
                  >
                    Precedente
                  </button>
                  <button
                    onClick={() => setPage(Math.min(totalPages, page + 1))}
                    disabled={page === totalPages}
                    style={{
                      padding: "4px 8px",
                      background: page === totalPages ? tc.bgHover : tc.bgCard,
                      border: `1px solid ${tc.border}`,
                      borderRadius: 4,
                      cursor: page === totalPages ? "default" : "pointer",
                      color: tc.text,
                      opacity: page === totalPages ? 0.5 : 1,
                    }}
                  >
                    Successivo
                  </button>
                </div>
              </div>
            )}
          </>
        )}
      </div>

      {/* Role dialog */}
      {showRoleDialog && selectedUser && (
        <div
          style={{
            position: "fixed",
            top: 0,
            left: 0,
            right: 0,
            bottom: 0,
            background: "rgba(0,0,0,0.5)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 1000,
          }}
          onClick={() => setShowRoleDialog(false)}
        >
          <div
            style={{
              background: tc.bg,
              borderRadius: 12,
              padding: 24,
              maxWidth: 400,
              width: "90%",
              border: `1px solid ${tc.border}`,
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <h2 style={{ fontSize: 16, fontWeight: 600, marginBottom: 16, color: tc.text }}>
              Cambia Ruolo
            </h2>
            <p style={{ fontSize: 13, color: tc.textMuted, marginBottom: 16 }}>
              Utente: {selectedUser.email}
            </p>
            <select
              value={newRole}
              onChange={(e) => setNewRole(e.target.value as "viewer" | "editor" | "admin")}
              style={{
                width: "100%",
                padding: "8px 12px",
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: tc.bgCard,
                color: tc.text,
                marginBottom: 16,
                fontSize: 13,
              }}
            >
              <option value="viewer">Viewer</option>
              <option value="editor">Editor</option>
              <option value="admin">Admin</option>
            </select>
            <div style={{ display: "flex", gap: 12, justifyContent: "flex-end" }}>
              <button
                onClick={() => setShowRoleDialog(false)}
                style={{
                  padding: "8px 16px",
                  borderRadius: 6,
                  border: `1px solid ${tc.border}`,
                  background: tc.bgHover,
                  color: tc.text,
                  cursor: "pointer",
                  fontSize: 13,
                }}
              >
                Annulla
              </button>
              <button
                onClick={handleChangeRole}
                style={{
                  padding: "8px 16px",
                  borderRadius: 6,
                  border: "none",
                  background: "#0066cc",
                  color: "#fff",
                  cursor: "pointer",
                  fontSize: 13,
                }}
              >
                Salva
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
