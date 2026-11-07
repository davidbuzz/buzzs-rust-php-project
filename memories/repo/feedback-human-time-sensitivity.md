# Human is HIGHLY Sensitive to Wasted Time

- **Core Rule:** The user is **HIGHLY sensitive to wasted time**. Every second matters.
- **Fail Fast:** Never allow dead, spinning, or hanging processes to burn time. Timeouts must be tight (e.g. 2.5–3.0s, not 15s or 60s).
- **No Pointless Waiting:** If a workload, test, or process exceeds reasonable runtime for its task, kill it immediately and fail fast. Never let dozens of tests accumulate hundreds of seconds of dead waiting.
- **Accurate & Transparent:** Provide concrete, realistic assessments and act swiftly on root causes.
