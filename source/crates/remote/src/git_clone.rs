//! Clones a GitHub repository on the server, or updates an existing clone. (GitCloneInstaller.cs)

use crate::script::{self, fill, ScriptValues};
use crate::server::{shell_quote, Log, Result, Server};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneTargetKind {
    Missing,
    SameRepository,
    OtherContent,
}

/// The target folder as found before cloning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneState {
    pub kind: CloneTargetKind,
    pub origin: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub changed_files: i32,
    pub behind: i32,
}

// Git separates progress updates with carriage returns. Keep only the meaningful lines.
const QUIET_PROGRESS: &str =
    r#"tr '\r' '\n' | awk '/^Cloning|done\.|^Updating files: .*done|^(warning|fatal|error)|Already up to date|Fast-forward|files? changed/ {print; fflush()}'"#;

const INSPECT: &str = r#"d={{dir}}
if [ ! -d "$d" ] || [ -z "$(ls -A "$d" 2>/dev/null)" ]; then echo "kind=missing"; exit 0; fi
# safe.directory because the folder may belong to another account. The emulator step gives it to "emulator".
origin=$(git -c safe.directory="$d" -C "$d" remote get-url origin 2>/dev/null)
echo "origin=$origin"
if [ -z "$origin" ] || ! git -c safe.directory="$d" -C "$d" rev-parse --show-toplevel 2>/dev/null | grep -qx "$(readlink -f "$d")"; then echo "kind=other"; exit 0; fi
echo "kind=repo"
echo "branch=$(git -c safe.directory="$d" -C "$d" rev-parse --abbrev-ref HEAD 2>/dev/null)"
echo "commit=$(git -c safe.directory="$d" -C "$d" log -1 --format='%h %cd %s' --date=short 2>/dev/null)"
echo "changed=$(git -c safe.directory="$d" -C "$d" status --porcelain --untracked-files=no 2>/dev/null | wc -l)"
timeout 30 git -c safe.directory="$d" -C "$d" fetch --quiet origin 2>/dev/null
echo "behind=$(git -c safe.directory="$d" -C "$d" rev-list --count HEAD..@{u} 2>/dev/null || echo 0)""#;

pub async fn inspect(server: &dyn Server, url: &str, target: &str) -> Result<CloneState> {
    let script = fill(INSPECT, &[("dir", &shell_quote(target))]);
    let result = server.run_as_root(&script).await?;
    let values = ScriptValues::parse(&result.output);
    let kind = match values.get("kind") {
        Some("missing") => CloneTargetKind::Missing,
        Some("repo") if same_repository(values.get("origin"), url) => CloneTargetKind::SameRepository,
        _ => CloneTargetKind::OtherContent,
    };
    Ok(CloneState {
        kind,
        origin: values.string("origin"),
        branch: values.string("branch"),
        commit: values.string("commit"),
        changed_files: values.int("changed") as i32,
        behind: values.int("behind") as i32,
    })
}

/// https and SSH URLs, with or without ".git", match the same repository.
pub fn same_repository(origin: Option<&str>, url: &str) -> bool {
    fn normalize(value: &str) -> String {
        let mut value = value.trim().trim_end_matches('/').to_lowercase();
        if value.ends_with(".git") {
            value.truncate(value.len() - 4);
        }
        match value.find("github.com") {
            Some(at) => value[at..].replace(':', "/"),
            None => value,
        }
    }
    origin.is_some_and(|origin| normalize(origin) == normalize(url))
}

const MOVE_ASIDE: &str = r#"d={{dir}}
backup="$d.bak-$(date +%Y%m%d-%H%M%S)"
mv "$d" "$backup"
echo "  $d moved to $backup"
echo "BACKUP=$backup""#;

/// Renames the folder to <folder>.bak-<time> and returns the new path.
pub async fn move_aside(server: &dyn Server, target: &str, log: Log<'_>) -> Result<String> {
    let body = fill(MOVE_ASIDE, &[("dir", &shell_quote(target))]);
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Moving the folder aside failed", &["BACKUP"], None).await?;
    Ok(values.or_empty("BACKUP").to_string())
}

const RESOLVE_BRANCH: &str = r#"want={{want}}
branch=$({{listRefs}} | grep -ix "$want" | head -1 || true)
[ -n "$branch" ] || fail "Branch $want is not in this repository."
"#;

/// Sets $branch to the branch the remote really has, matched without case: one repository writes it as "dev" and
/// another as "Dev". Empty when no branch was chosen, so the repository's own default is used.
fn resolve_branch(branch: Option<&str>, list_refs: &str) -> String {
    match branch {
        None => "branch=\n".to_string(),
        Some(branch) => fill(RESOLVE_BRANCH, &[("want", &shell_quote(branch)), ("listRefs", list_refs)]),
    }
}

const CLONE: &str = r#"set -o pipefail
if ! command -v git >/dev/null 2>&1; then
  echo "> Installing git"
  apt_update
  apt-get install -y git
fi
d={{dir}}
u={{u}}
install -d -m 755 "$(dirname "$d")"
{{resolve}}
echo "> git clone${branch:+ --branch $branch} {{url}} $d"
git clone ${branch:+--branch "$branch"} --progress "$u" "$d" 2>&1 | {{QuietProgress}}
echo "COMMIT=$(git -c safe.directory="$d" -C "$d" rev-parse --abbrev-ref HEAD) $(git -c safe.directory="$d" -C "$d" log -1 --format='%h %cd %s' --date=short)""#;

/// Installs git if needed, clones the branch (None: the repository's default), and returns the branch and commit.
pub async fn clone(server: &dyn Server, url: &str, target: &str, branch: Option<&str>, log: Log<'_>) -> Result<String> {
    let resolve = resolve_branch(branch, r#"git ls-remote --heads "$u" | sed 's#.*refs/heads/##'"#);
    let body = fill(
        CLONE,
        &[
            ("dir", &shell_quote(target)),
            ("u", &shell_quote(url)),
            ("resolve", &resolve),
            ("url", url),
            ("QuietProgress", QUIET_PROGRESS),
        ],
    );
    commit_after(server, &script::with_prelude(&body), log, "Cloning failed").await
}

const UPDATE: &str = r#"set -o pipefail
d={{dir}}
# With AtomCMS the Octane step adds --base=/client/ to package.json. Undo only that for the pull,
# since the Octane step and build.sh put it back.
if [ -f "$d/package.json" ] && grep -q -- '--base=' "$d/package.json" &&
   sed -E 's#("(start|build)": "vite) --base=[^ "]+#\1#' "$d/package.json" | cmp -s - <(git -c safe.directory="$d" -C "$d" show HEAD:package.json 2>/dev/null); then
  git -c safe.directory="$d" -C "$d" checkout -- package.json
  echo "  package.json: --base for /client/ undone for the pull (the Octane step sets it again)"
fi
# The Octane update tool writes a new yarn.lock, and the pull may bring its own.
if git -c safe.directory="$d" -C "$d" ls-files --error-unmatch yarn.lock >/dev/null 2>&1 &&
   [ -n "$(git -c safe.directory="$d" -C "$d" status --porcelain -- yarn.lock)" ]; then
  git -c safe.directory="$d" -C "$d" checkout -- yarn.lock
  echo "  yarn.lock: the locally generated one is replaced by the repository's for the pull"
fi
changed=$(git -c safe.directory="$d" -C "$d" status --porcelain --untracked-files=no)
if [ -n "$changed" ]; then
  echo "$changed" | head -20
  fail "Files tracked by git were changed in $d. Commit or undo those changes first, so they are not lost."
fi
echo "> git fetch origin"
git -c safe.directory="$d" -C "$d" fetch --quiet --prune origin
{{resolve}}
current=$(git -c safe.directory="$d" -C "$d" rev-parse --abbrev-ref HEAD)
if [ -n "$branch" ] && [ "$(echo "$current" | tr 'A-Z' 'a-z')" != "$(echo "$branch" | tr 'A-Z' 'a-z')" ]; then
  echo "> git checkout $branch  (was $current)"
  if git -c safe.directory="$d" -C "$d" show-ref --verify --quiet "refs/heads/$branch"; then
    git -c safe.directory="$d" -C "$d" checkout "$branch"
  else
    git -c safe.directory="$d" -C "$d" checkout --track "origin/$branch"
  fi
fi
echo "> git pull --ff-only"
git -c safe.directory="$d" -C "$d" pull --ff-only --progress 2>&1 | {{QuietProgress}}
echo "COMMIT=$(git -c safe.directory="$d" -C "$d" rev-parse --abbrev-ref HEAD) $(git -c safe.directory="$d" -C "$d" log -1 --format='%h %cd %s' --date=short)""#;

/// Fast-forwards the clone, refusing if tracked files were changed. Switches to `branch` first when the clone is on
/// another one (None: stay on the branch it is on). Returns the branch and commit.
pub async fn update(server: &dyn Server, target: &str, branch: Option<&str>, log: Log<'_>) -> Result<String> {
    let resolve = resolve_branch(branch, r#"git -c safe.directory="$d" -C "$d" for-each-ref --format='%(refname:lstrip=3)' refs/remotes/origin"#);
    let body = fill(UPDATE, &[("dir", &shell_quote(target)), ("resolve", &resolve), ("QuietProgress", QUIET_PROGRESS)]);
    commit_after(server, &script::with_prelude(&body), log, "Updating failed").await
}

async fn commit_after(server: &dyn Server, script: &str, log: Log<'_>, failure: &str) -> Result<String> {
    let values = script::run_capturing(server, script, log, failure, &["COMMIT"], None).await?;
    Ok(values.or_empty("COMMIT").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_repository_urls() {
        let url = "https://github.com/ObjectRetros/atomcms.git";
        assert!(same_repository(Some("https://github.com/objectretros/atomcms"), url));
        assert!(same_repository(Some("git@github.com:ObjectRetros/atomcms.git"), url));
        assert!(same_repository(Some(" https://github.com/ObjectRetros/atomcms/ "), url));
        assert!(!same_repository(Some("https://github.com/ObjectRetros/other.git"), url));
        assert!(!same_repository(None, url));
        assert!(same_repository(Some("https://gitea.local/x.git"), "https://gitea.local/x"));
    }

    #[test]
    fn branches() {
        assert_eq!(resolve_branch(None, "x"), "branch=\n");
        assert_eq!(
            resolve_branch(Some("dev"), "list"),
            "want='dev'\nbranch=$(list | grep -ix \"$want\" | head -1 || true)\n[ -n \"$branch\" ] || fail \"Branch $want is not in this repository.\"\n"
        );
        assert_eq!(fill("{{a}}{{b}}", &[("a", "{{b}}"), ("b", "1")]), "{{b}}1");
    }
}
