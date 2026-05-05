---
name: skip-preview-check-remote-deploy
enabled: true
event: stop
action: allow
conditions: []
---

This project deploys to a remote production server; configure DEPLOY_HOST before running deploy scripts.
There is no local dev server. All frontend changes are built on the remote server.
The preview/dev server check does not apply here — always allow stop.
