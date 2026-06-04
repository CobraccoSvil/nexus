//! Registrazione handler dominio: github
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        gh_issue_close::GhIssueCloseTool, gh_issue_comment::GhIssueCommentTool,
        gh_issue_create::GhIssueCreateTool, gh_issue_list::GhIssueListTool,
        gh_issue_view::GhIssueViewTool, gh_label_list::GhLabelListTool,
        gh_pr_checks::GhPrChecksTool, gh_pr_close::GhPrCloseTool, gh_pr_create::GhPrCreateTool,
        gh_pr_diff::GhPrDiffTool, gh_pr_files::GhPrFilesTool, gh_pr_list::GhPrListTool,
        gh_pr_merge::GhPrMergeTool, gh_pr_review::GhPrReviewTool, gh_pr_view::GhPrViewTool,
        gh_release_create::GhReleaseCreateTool, gh_release_list::GhReleaseListTool,
        gh_release_view::GhReleaseViewTool, gh_repo_clone_url::GhRepoCloneUrlTool,
        gh_repo_fork_list::GhRepoForkListTool, gh_repo_view::GhRepoViewTool,
        gh_run_cancel::GhRunCancelTool, gh_run_list::GhRunListTool, gh_run_logs::GhRunLogsTool,
        gh_run_view::GhRunViewTool, gh_workflow_list::GhWorkflowListTool,
        gh_workflow_run::GhWorkflowRunTool, gh_workflow_view::GhWorkflowViewTool,
    };

    // GitHub (Fase 9C)
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_issue_list",
            NexusToolCategory::GitHub,
            "Run `gh issue list --json` and return parsed issues",
        ),
        Arc::new(GhIssueListTool),
    );

    // GitHub (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_create",
            NexusToolCategory::GitHub,
            "Create a GitHub pull request via `gh pr create`",
        ),
        Arc::new(GhPrCreateTool),
    );

    // Fase 9F: GitHub batch (3)
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_workflow_list",
            NexusToolCategory::GitHub,
            "List GitHub Actions workflows (gh workflow list)",
        ),
        Arc::new(GhWorkflowListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_run_list",
            NexusToolCategory::GitHub,
            "List GitHub Actions runs with success/failure counts",
        ),
        Arc::new(GhRunListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_release_list",
            NexusToolCategory::GitHub,
            "List GitHub releases",
        ),
        Arc::new(GhReleaseListTool),
    );

    // Fase 9G: GitHub batch (3)
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_list",
            NexusToolCategory::GitHub,
            "`gh pr list --json` filtered by state/base",
        ),
        Arc::new(GhPrListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_view",
            NexusToolCategory::GitHub,
            "`gh pr view <num> --json` full PR detail",
        ),
        Arc::new(GhPrViewTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_repo_view",
            NexusToolCategory::GitHub,
            "`gh repo view --json` repository metadata",
        ),
        Arc::new(GhRepoViewTool),
    );

    // GitHub (Fase 9J)
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_issue_view",
            NexusToolCategory::GitHub,
            "`gh issue view <num> --json`",
        ),
        Arc::new(GhIssueViewTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_issue_create",
            NexusToolCategory::GitHub,
            "`gh issue create --title --body`",
        ),
        Arc::new(GhIssueCreateTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_issue_close",
            NexusToolCategory::GitHub,
            "`gh issue close <num>`",
        ),
        Arc::new(GhIssueCloseTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_issue_comment",
            NexusToolCategory::GitHub,
            "`gh issue comment <num> --body`",
        ),
        Arc::new(GhIssueCommentTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_close",
            NexusToolCategory::GitHub,
            "`gh pr close <num>`",
        ),
        Arc::new(GhPrCloseTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_merge",
            NexusToolCategory::GitHub,
            "`gh pr merge <num> --squash|--merge|--rebase`",
        ),
        Arc::new(GhPrMergeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_review",
            NexusToolCategory::GitHub,
            "`gh pr review <num>` approve/request-changes/comment",
        ),
        Arc::new(GhPrReviewTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_diff",
            NexusToolCategory::GitHub,
            "`gh pr diff <num>` con conteggio +/-",
        ),
        Arc::new(GhPrDiffTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_checks",
            NexusToolCategory::GitHub,
            "`gh pr checks <num>` pass/fail/pending",
        ),
        Arc::new(GhPrChecksTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_pr_files",
            NexusToolCategory::GitHub,
            "`gh pr view <num> --json files`",
        ),
        Arc::new(GhPrFilesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_workflow_view",
            NexusToolCategory::GitHub,
            "`gh workflow view <name>`",
        ),
        Arc::new(GhWorkflowViewTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_workflow_run",
            NexusToolCategory::GitHub,
            "`gh workflow run <name> --ref`",
        ),
        Arc::new(GhWorkflowRunTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_run_view",
            NexusToolCategory::GitHub,
            "`gh run view <id> --json`",
        ),
        Arc::new(GhRunViewTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_run_logs",
            NexusToolCategory::GitHub,
            "`gh run view <id> --log`",
        ),
        Arc::new(GhRunLogsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_run_cancel",
            NexusToolCategory::GitHub,
            "`gh run cancel <id>`",
        ),
        Arc::new(GhRunCancelTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_release_view",
            NexusToolCategory::GitHub,
            "`gh release view <tag> --json`",
        ),
        Arc::new(GhReleaseViewTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_release_create",
            NexusToolCategory::GitHub,
            "`gh release create <tag> --title --notes`",
        ),
        Arc::new(GhReleaseCreateTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_repo_clone_url",
            NexusToolCategory::GitHub,
            "`gh repo view --json url,sshUrl`",
        ),
        Arc::new(GhRepoCloneUrlTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_repo_fork_list",
            NexusToolCategory::GitHub,
            "`gh repo view --json forkCount,parent`",
        ),
        Arc::new(GhRepoForkListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "gh_label_list",
            NexusToolCategory::GitHub,
            "`gh label list --json`",
        ),
        Arc::new(GhLabelListTool),
    );
}
