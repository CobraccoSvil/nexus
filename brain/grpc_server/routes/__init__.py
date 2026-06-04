"""Router REST del Neural Core, suddivisi per responsabilita'.

Ogni modulo espone un `APIRouter` (`router`) incluso da
`brain/grpc_server/app.py`. I gruppi:
  - core      : health, classify, route-model, embed, search, providers, complete, reload-settings
  - vision    : /vision/describe, /vision/compare
  - agent     : project-analyze, subagent, clarifications, batch-analyze, agent run/stream/state
  - terminal  : websocket /ws/terminal
"""
