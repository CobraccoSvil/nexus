//! Registrazione handler dominio: vcs
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        git_archive_dry::GitArchiveDryTool, git_blame::GitBlameTool,
        git_branch_list::GitBranchListTool, git_bundle_verify::GitBundleVerifyTool,
        git_cat_file::GitCatFileTool, git_check_ignore::GitCheckIgnoreTool,
        git_clean_dry::GitCleanDryTool, git_config_list::GitConfigListTool,
        git_count_objects::GitCountObjectsTool, git_describe::GitDescribeTool,
        git_diff::GitDiffTool, git_diff_stat::GitDiffStatTool, git_for_each_ref::GitForEachRefTool,
        git_fsck::GitFsckTool, git_gc_dry::GitGcDryTool, git_grep::GitGrepTool,
        git_log::GitLogTool, git_log_graph::GitLogGraphTool, git_ls_files::GitLsFilesTool,
        git_ls_tree::GitLsTreeTool, git_merge_base::GitMergeBaseTool, git_reflog::GitReflogTool,
        git_remote_list::GitRemoteListTool, git_rev_parse::GitRevParseTool,
        git_shortlog::GitShortlogTool, git_show::GitShowTool, git_show_branch::GitShowBranchTool,
        git_stash_list::GitStashListTool, git_status::GitStatusTool,
        git_submodule_list::GitSubmoduleListTool, git_tag_list::GitTagListTool,
        git_worktree_list::GitWorktreeListTool,
    };

    // Vcs
    c.register_with_handler(
        NexusToolSpec::new(
            "git_status",
            NexusToolCategory::Vcs,
            "Run `git status --porcelain=v2 --branch` and return structured state",
        ),
        Arc::new(GitStatusTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_log",
            NexusToolCategory::Vcs,
            "Run `git log` with structured format and parse commits",
        ),
        Arc::new(GitLogTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_diff",
            NexusToolCategory::Vcs,
            "Run `git diff` with --stat and return structured diff",
        ),
        Arc::new(GitDiffTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_blame",
            NexusToolCategory::Vcs,
            "Run `git blame --porcelain` and parse per-line authorship",
        ),
        Arc::new(GitBlameTool),
    );

    // Fase 9F: VCS batch (4)
    c.register_with_handler(
        NexusToolSpec::new(
            "git_branch_list",
            NexusToolCategory::Vcs,
            "List local and remote git branches with upstream tracking",
        ),
        Arc::new(GitBranchListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_remote_list",
            NexusToolCategory::Vcs,
            "List git remotes with fetch and push URLs",
        ),
        Arc::new(GitRemoteListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_show",
            NexusToolCategory::Vcs,
            "Show a commit with numstat file changes",
        ),
        Arc::new(GitShowTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_tag_list",
            NexusToolCategory::Vcs,
            "List git tags sorted by creator date",
        ),
        Arc::new(GitTagListTool),
    );

    // Fase 9G: VCS batch (4)
    c.register_with_handler(
        NexusToolSpec::new(
            "git_stash_list",
            NexusToolCategory::Vcs,
            "List git stashes with index, branch and message",
        ),
        Arc::new(GitStashListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_grep",
            NexusToolCategory::Vcs,
            "`git grep -n -E` regex search across tracked files",
        ),
        Arc::new(GitGrepTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_describe",
            NexusToolCategory::Vcs,
            "`git describe --tags --long --dirty` parsed into tag/commits/sha",
        ),
        Arc::new(GitDescribeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_shortlog",
            NexusToolCategory::Vcs,
            "`git shortlog -sne` aggregating commits per author",
        ),
        Arc::new(GitShortlogTool),
    );

    // Vcs (Fase 9I)
    c.register_with_handler(
        NexusToolSpec::new(
            "git_rev_parse",
            NexusToolCategory::Vcs,
            "`git rev-parse <ref>` ref → SHA",
        ),
        Arc::new(GitRevParseTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_count_objects",
            NexusToolCategory::Vcs,
            "`git count-objects -v` repo size info",
        ),
        Arc::new(GitCountObjectsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_reflog",
            NexusToolCategory::Vcs,
            "`git reflog -n N` reference log",
        ),
        Arc::new(GitReflogTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_clean_dry",
            NexusToolCategory::Vcs,
            "`git clean -nd` dry-run",
        ),
        Arc::new(GitCleanDryTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_check_ignore",
            NexusToolCategory::Vcs,
            "`git check-ignore -v` for given paths",
        ),
        Arc::new(GitCheckIgnoreTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_ls_files",
            NexusToolCategory::Vcs,
            "`git ls-files` lista file tracciati",
        ),
        Arc::new(GitLsFilesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_ls_tree",
            NexusToolCategory::Vcs,
            "`git ls-tree -r <ref>` lista in commit",
        ),
        Arc::new(GitLsTreeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_cat_file",
            NexusToolCategory::Vcs,
            "`git cat-file -p <ref>` object content (preview)",
        ),
        Arc::new(GitCatFileTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_for_each_ref",
            NexusToolCategory::Vcs,
            "`git for-each-ref` enumera tutte le ref",
        ),
        Arc::new(GitForEachRefTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_merge_base",
            NexusToolCategory::Vcs,
            "`git merge-base a b` common ancestor",
        ),
        Arc::new(GitMergeBaseTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_diff_stat",
            NexusToolCategory::Vcs,
            "`git diff --shortstat <range>` summary",
        ),
        Arc::new(GitDiffStatTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_log_graph",
            NexusToolCategory::Vcs,
            "`git log --oneline --graph -n N`",
        ),
        Arc::new(GitLogGraphTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_show_branch",
            NexusToolCategory::Vcs,
            "`git show-branch --all`",
        ),
        Arc::new(GitShowBranchTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_archive_dry",
            NexusToolCategory::Vcs,
            "Stima dimensione `git archive` (senza scrivere)",
        ),
        Arc::new(GitArchiveDryTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_bundle_verify",
            NexusToolCategory::Vcs,
            "`git bundle verify <path>`",
        ),
        Arc::new(GitBundleVerifyTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_fsck",
            NexusToolCategory::Vcs,
            "`git fsck --no-progress` repo integrity",
        ),
        Arc::new(GitFsckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_gc_dry",
            NexusToolCategory::Vcs,
            "Verifica se `git gc` è necessario (loose objects threshold)",
        ),
        Arc::new(GitGcDryTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_config_list",
            NexusToolCategory::Vcs,
            "`git config --list --local` (sensitive masked)",
        ),
        Arc::new(GitConfigListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_worktree_list",
            NexusToolCategory::Vcs,
            "`git worktree list --porcelain`",
        ),
        Arc::new(GitWorktreeListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "git_submodule_list",
            NexusToolCategory::Vcs,
            "`git submodule status` lista submodule",
        ),
        Arc::new(GitSubmoduleListTool),
    );
}
