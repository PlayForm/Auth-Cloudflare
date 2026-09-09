---
name: cloudflare-dev-workflow
description: "Reverse-PR git workflow for the Cloudflare plugin - feat-dev/trunk integration, Source remote, no direct pushes."
version: 0.0.1
author: PlayForm
license: CC0-1.0
platforms: [linux, macos]
metadata:
  hermes:
    tags: [git, workflow, cloudflare, plugin]
---

# Cloudflare plugin - Git workflow

> [!IMPORTANT]
>
> Reverse-PR flow, same as Aphrodite's `git-feat-dev-workflow`. `feat-dev` is
> the local integration branch; `trunk` tracks the Source remote. Never commit
> to `trunk` directly.

```sh
# Sync
git fetch Source && git checkout trunk && git pull Source Current
git checkout feat-dev && git merge trunk

# Feature work
git checkout -b feat/<topic>
# ... commits ...
git checkout feat-dev && git merge --no-ff feat/<topic>

# Ship
git push Source feat-dev:Current
```

> [!WARNING]
>
> The Source remote is `https://github.com/PlayForm/Hermes-Cloudflare.git`
> (branch `Current`, remote name `Source`) - mirror this exactly when adding
> remotes to a fresh clone.
