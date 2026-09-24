import test from "node:test";
import assert from "node:assert/strict";
import { validate } from "../src/lib/api.ts";
import {
  demoResponse,
  demoRequests,
  demoDetail,
  pricingCases,
} from "../src/lib/demo.ts";

test("all API-shaped fixtures satisfy the current transport schema", () => {
  for (const path of [
    "/health",
    "/metrics",
    "/providers",
    "/v1/models",
    "/v1/analytics/requests",
    "/v1/analytics/providers/comparison",
    "/v1/analytics/fallbacks",
    `/v1/analytics/requests/${demoRequests[0].request_id}`,
    `/v1/analytics/requests/${demoRequests[0].request_id}/timeline`,
  ])
    assert.doesNotThrow(() => validate(demoResponse(path), path));
});
test("malformed successful payloads fail as contract errors before render", () => {
  assert.throws(() => validate({ requests: [null] }, "/v1/analytics/requests"));
  assert.throws(() =>
    validate(
      { requests: [{ ...demoRequests[0], received_at: "not a timestamp" }] },
      "/v1/analytics/requests",
    ),
  );
  assert.throws(() =>
    validate(
      { data: [{ id: "model", providers: null, capabilities: null }] },
      "/v1/models",
    ),
  );
  assert.throws(() =>
    validate(
      { providers: [{ provider: "x", total_attempts: "one" }] },
      "/v1/analytics/providers/comparison",
    ),
  );
});
test("unknown reasoning visibility and absent content remain readable data", () => {
  const detail = demoDetail(demoRequests[1].request_id)!;
  detail.generations[0].reasoning_visibility = "vendor_private";
  detail.generations[0].content = null;
  assert.doesNotThrow(() =>
    validate(detail, `/v1/analytics/requests/${detail.request.request_id}`),
  );
});
test("future price fixtures preserve missing versus explicit zero", () => {
  assert.equal(pricingCases[0].estimated_cost, null);
  assert.equal(pricingCases[1].estimated_cost, 0);
  assert.notEqual(pricingCases[0].configured, pricingCases[1].configured);
});
