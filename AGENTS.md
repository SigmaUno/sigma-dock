# Repository workflow

For each GitHub issue, create a dedicated branch from the current default branch and open a pull request for its changes. Reference the issue in the PR, and use `Fixes #NUMBER` only when the PR completes it. Do not commit or push issue changes directly to the default branch. Keep unrelated work and other contributors' changes out of the PR.

Build and validation workflows run only on pushed tags. Do not add branch-push, pull-request or manual build triggers. Crate publication and macOS distribution use matching `v*` version tags. The manual version-preparation workflow only creates a branch; open and review its PR before tagging the merged commit.

Run checks appropriate to the change locally before opening the PR and include the results in its description. Do not move existing release tags.
