import test from "node:test";
import assert from "node:assert/strict";
import {
  milliseconds,
  ratio,
  deltaSample,
  relativeMs,
  timelineRows,
  usageWarnings,
  csvCell,
  transition,
} from "../src/lib/metrics.ts";
import { proxyPath } from "../src/lib/proxy-policy.ts";

test("microseconds, missing values and zero remain distinct", () => {
  assert.equal(milliseconds(1200), 1.2);
  assert.equal(milliseconds(null), null);
  assert.equal(milliseconds(0), 0);
  assert.equal(ratio(1, 0), null);
  assert.equal(ratio(0, 4), 0);
});
test("counter resets and outage intervals are gaps, not negative traffic", () => {
  assert.equal(
    deltaSample(
      { count: 12, uptime: 20, at: 1000 },
      { count: 15, uptime: 25, at: 6000 },
    ),
    3,
  );
  assert.equal(
    deltaSample(
      { count: 12, uptime: 20, at: 1000 },
      { count: 1, uptime: 2, at: 6000 },
    ),
    null,
  );
  assert.equal(
    deltaSample(
      { count: 12, uptime: 20, at: 1000 },
      { count: 20, uptime: 40, at: 21000 },
    ),
    null,
  );
});
test("nanosecond timestamp subtraction preserves submillisecond observations", () => {
  assert.equal(
    relativeMs(
      "2026-09-23T00:00:00.000100300Z",
      "2026-09-23T00:00:00.000000100Z",
    ),
    0.1002,
  );
});
test("chunk gaps never cross attempts or synthetic observations", () => {
  const e = (
    attempt_number: number,
    sequence: number,
    ms: string,
    synthetic = false,
  ) => ({
    attempt_number,
    sequence,
    timestamp: `2026-09-23T00:00:00.${ms}Z`,
    synthetic,
    chunk_type: "content",
    byte_count: 3,
  });
  const rows = timelineRows(
    [
      e(2, 1, "030"),
      e(1, 2, "020"),
      e(1, 1, "010"),
      e(1, 3, "021", true),
      e(1, 4, "040"),
    ],
    "2026-09-23T00:00:00Z",
  );
  assert.deepEqual(
    rows.map((r) => r.gap),
    [null, 10, null, null, null],
  );
});
test("usage flags invalid subsets, ambiguous zeros, and repeated updates", () => {
  const u = {
    attempt_number: 1,
    prompt_tokens: 0,
    completion_tokens: 5,
    total_tokens: 5,
    cached_tokens: 2,
    reasoning_tokens: 8,
  };
  assert.ok(usageWarnings(u, 2).length >= 3);
});
test("retry and deployment switch are distinct", () => {
  assert.equal(
    transition({ provider: "a", model: "m" }, { provider: "a", model: "m" }),
    "Same-deployment retry",
  );
  assert.equal(
    transition({ provider: "a", model: "m" }, { provider: "a", model: "n" }),
    "Model switch",
  );
  assert.equal(
    transition({ provider: "a", model: "m" }, { provider: "b", model: "m" }),
    "Provider switch",
  );
});
test("CSV formulas are escaped", () => {
  assert.equal(csvCell("=SUM(A1)"), '"\'=SUM(A1)"');
  assert.equal(csvCell('a"b'), '"a""b"');
});
test("proxy permits only bounded documented read paths and query parameters", () => {
  assert.equal(
    proxyPath(
      ["v1", "analytics", "requests"],
      new URLSearchParams("limit=50&success=false"),
    ),
    "/v1/analytics/requests?limit=50&success=false",
  );
  for (const query of [
    "url=http://evil",
    "limit=501",
    "offset=-1",
    "success=maybe",
    "from=2026-01-01",
    "limit=1&limit=2",
  ])
    assert.throws(() =>
      proxyPath(["v1", "analytics", "requests"], new URLSearchParams(query)),
    );
  assert.throws(() =>
    proxyPath(["v1", "chat", "completions"], new URLSearchParams()),
  );
  assert.throws(() => proxyPath(["health"], new URLSearchParams("model=x")));
});
