## Goal
Manager passes a prompt/task description to the agent inside the VM,
not just a raw shell command.

## Approach
- Write task file to workspace/.seguro/task.md (or task.json)
- Agent entrypoint reads the task file and executes it
- Support for task metadata: priority, context files, constraints

## Acceptance
- Manager writes task spec, agent picks it up and executes autonomously
