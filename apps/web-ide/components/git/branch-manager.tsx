"use client";

import {
  checkoutGitBranch,
  createGitBranch,
  type GitBranchInfo,
  type UserProjectDetails,
} from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";
import { useState } from "react";

function buttonStyle(tc: ReturnType<typeof useThemeColors>, disabled: boolean) {
  return {
    padding: "7px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: disabled ? tc.bgCard : tc.accentBg,
    color: tc.text,
    cursor: disabled ? "not-allowed" : "pointer",
  };
}

function inputStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    flex: 1,
    padding: "7px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
  };
}

interface BranchManagerProps {
  project: UserProjectDetails;
  branches: GitBranchInfo[];
  busy: boolean;
  runAction: (action: () => Promise<unknown>) => Promise<void>;
}

export function BranchManager({ project, branches, busy, runAction }: BranchManagerProps) {
  const tc = useThemeColors();
  const [newBranch, setNewBranch] = useState("");

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <div style={{ display: "flex", gap: 8 }}>
        <input
          value={newBranch}
          onChange={(event) => setNewBranch(event.target.value)}
          placeholder="Nuovo branch"
          style={inputStyle(tc)}
        />
        <button
          disabled={busy || !project.canManageGit || !newBranch.trim()}
          onClick={() =>
            runAction(async () => {
              await createGitBranch(project.id, newBranch.trim());
              await checkoutGitBranch(project.id, newBranch.trim());
              setNewBranch("");
            })
          }
          title="Crea branch — crea e passa al nuovo branch"
          style={buttonStyle(tc, busy || !project.canManageGit || !newBranch.trim())}
        >
          ⎇
        </button>
      </div>

      <div>
        <div style={{ color: tc.textSecondary, marginBottom: 8 }}>Branch</div>
        <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
          {branches.map((branch) => (
            <button
              key={branch.name}
              disabled={busy || !project.canManageGit || branch.isCurrent}
              onClick={() => runAction(() => checkoutGitBranch(project.id, branch.name))}
              style={{
                ...buttonStyle(tc, busy || !project.canManageGit || branch.isCurrent),
                background: branch.isCurrent ? tc.accentBg : tc.bgCard,
              }}
            >
              {branch.name}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
