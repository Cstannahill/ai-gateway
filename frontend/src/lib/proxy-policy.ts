const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
export const isRequestId = (s: string) => uuid.test(s);
export function proxyPath(segments: string[], query: URLSearchParams) {
  const path = "/" + segments.join("/");
  const list = path === "/v1/analytics/requests";
  const fixed = [
    "/health",
    "/metrics",
    "/providers",
    "/v1/models",
    "/v1/analytics/providers/comparison",
    "/v1/analytics/fallbacks",
  ];
  const detail =
    segments.length >= 4 &&
    segments[0] === "v1" &&
    segments[1] === "analytics" &&
    segments[2] === "requests" &&
    uuid.test(segments[3]) &&
    (segments.length === 4 ||
      (segments.length === 5 && segments[4] === "timeline"));
  if (!list && !fixed.includes(path) && !detail)
    throw new Error("Read endpoint is not allowed.");
  for (const [key, value] of query) {
    if (
      !list ||
      !["limit", "offset", "provider", "model", "success"].includes(key) ||
      query.getAll(key).length !== 1
    )
      throw new Error("Unsupported or duplicate query parameter.");
    if (key === "limit" && (!/^\d+$/.test(value) || +value < 1 || +value > 500))
      throw new Error("Limit must be 1–500.");
    if (
      key === "offset" &&
      (!/^\d+$/.test(value) ||
        !Number.isSafeInteger(+value) ||
        +value > 1000000)
    )
      throw new Error("Offset must be 0–1000000.");
    if (key === "success" && !["true", "false"].includes(value))
      throw new Error("Success must be true or false.");
    if (
      ["model", "provider"].includes(key) &&
      (!value.length || value.length > 256 || /[\x00-\x1f]/.test(value))
    )
      throw new Error("Invalid filter value.");
  }
  return path + (query.size ? "?" + query.toString() : "");
}
