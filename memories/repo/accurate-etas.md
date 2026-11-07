# Accurate ETAs Rule

Accurate ETAs are essential to the user. Optimistic, under-estimated, or vague guessing ETAs are strictly unacceptable.

## Guidelines for ETA Calculation
1. **Never Lowball or Guess:** When workloads can time out (e.g., 15s ceiling per test) or execute at variable speeds, do not assume best-case sub-second execution.
2. **Calculate from Concrete Metrics:**
   * Count total items, completed items, and remaining items explicitly.
   * Inspect the recent per-item execution times (e.g., if tests are timing out at 15s, compute remaining timeout-prone items × 15s).
   * Present a realistic upper-bound or conservative range rather than a low single number.
3. **State Clear Assumptions:**
   * Break down how the ETA was derived (e.g., "$N$ items remaining; $K$ timing out @ 15s + $M$ passing @ 0.5s = $T$ min").
