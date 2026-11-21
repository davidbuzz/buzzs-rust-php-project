---
name: feedback-environmental-failures-pass
description: "PHPT failures caused only by the host environment (C PHP fails them the same way here) count as PASS, not as failures"
metadata:
  node_type: memory
  type: feedback
  originSessionId: ebaf0a84-fb28-4fd1-bc69-f1147e28a4a5
  modified: 2026-11-06T05:03:59.401Z
---

A `.phpt` test that fails only because of this machine's environment, which PHP has no control over, counts as a PASS in our pass rates and reports, not as a failure or a "known failure". Examples on 6 Nov 2026: `ext/gd/tests/createfromstring-overflow.phpt` (Ubuntu's libgd 2.3.3 is built without the GD2 format) and `ext/mysqli/tests/mysqli_fetch_field_types.phpt` (MariaDB has no JSON column type). The owner said: "re environmental stuff ... i consider this a PASS, not a fail. ie dont blame php itself for something it doesnt have control over."

**Why:** the score measures our PHP implementation, not the host's libraries or services; C PHP on the same box fails those tests identically.
**How to apply:** a test qualifies only with evidence that C PHP (/usr/bin/php8.5 and/or /usr/bin/php8.5) fails it the same way on this host for an environmental reason. Keep such tests in a curated environment list, which the runner and the FEATURE_GAP scripts count as passes, and keep them out of the regression baseline. Never use it for our own bugs, flaky timing caused by our slowness, or tests we merely skip. Related: [[phprust-scope-decisions]], [[feedback-no-invented-labels]].
