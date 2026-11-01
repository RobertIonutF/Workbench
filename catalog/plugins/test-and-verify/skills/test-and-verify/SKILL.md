---
name: test-and-verify
description: "Run project checks and review changes against real results."
---

Identify the project's documented checks and dependency setup. Inspect the actual diff and choose the smallest relevant checks that exercise its behavior. Execute checks only through approved command tools; retain exit codes and actual failure details. If a check fails, diagnose the concrete cause and apply a focused fix when authorized. Run the affected check again. Review final changes for unintended edits. Report which commands ran, their outcomes, and any checks not performed. Do not infer runtime success from source inspection alone.
