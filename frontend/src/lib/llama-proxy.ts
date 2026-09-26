import { request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";

const jsonHeaders = { "Cache-Control": "no-store", "Content-Type": "application/json" };
const fail = (status: number, code: string, message: string) =>
  Response.json({ error: { type: code, code, message } }, { status, headers: jsonHeaders });
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const routes: Record<string, RegExp[]> = {
  GET: [/^status$/, /^logs$/, /^models$/, /^models\/scan$/, /^defaults$/],
  POST: [/^start$/, /^stop$/, /^restart$/, /^models$/, /^models\/validate$/, /^args\/parse$/],
  PUT: [/^models\/[0-9a-f-]+$/i, /^defaults$/],
  DELETE: [/^models\/[0-9a-f-]+$/i],
};
const bodyRoutes = new Set(["POST:start", "POST:models", "POST:models/validate", "POST:args/parse", "PUT:defaults"]);

export async function proxyLlama(request: Request, segments: string[], gatewayUrl: string, timeoutMs = 15000) {
  const method = request.method.toUpperCase();
  const path = segments.join("/");
  if ((method === "PUT" || method === "DELETE") && path.startsWith("models/") && !uuid.test(path.slice(7)))
    return fail(400, "invalid_request", "Model id must be a UUID.");
  if (!Object.values(routes).some((patterns) => patterns.some((pattern) => pattern.test(path))))
    return fail(404, "not_found", "Unknown llama-server route.");
  if (!routes[method]?.some((pattern) => pattern.test(path)))
    return fail(405, "method_not_allowed", "Method is not allowed for this route.");
  const url = new URL(request.url);
  const expectedOrigin = `${url.protocol}//${request.headers.get("host") ?? url.host}`;
  const origin = request.headers.get("origin");
  const site = request.headers.get("sec-fetch-site");
  if ((origin && origin !== expectedOrigin) || (site && !["same-origin", "none"].includes(site)))
    return fail(403, "origin_forbidden", "Request must originate from this console.");
  if (!origin && site !== "same-origin")
    return fail(403, "origin_forbidden", "A same-origin browser request is required.");
  if (path !== "logs" && url.search) return fail(400, "invalid_request", "Unexpected query parameters.");
  if (path === "logs") {
    for (const [key, value] of url.searchParams) {
      if (!(["after", "limit"].includes(key)) || url.searchParams.getAll(key).length !== 1 ||
          !/^(0|[1-9]\d*)$/.test(value) || !Number.isSafeInteger(Number(value)) ||
          (key === "limit" && (Number(value) < 1 || Number(value) > 2000)))
        return fail(400, "invalid_request", "Invalid logs query parameters.");
    }
  }
  let base: URL;
  try {
    base = new URL(gatewayUrl);
    if (!["http:", "https:"].includes(base.protocol) || base.username || base.password || base.pathname !== "/" || base.search || base.hash) throw new Error();
  } catch {
    return fail(500, "gateway_configuration", "GATEWAY_URL must be an HTTP(S) origin without credentials or a path.");
  }
  const needsBody = bodyRoutes.has(`${method}:${path}`) || (method === "PUT" && path.startsWith("models/"));
  let body: string | undefined;
  if (needsBody) {
    if (request.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json")
      return fail(415, "unsupported_media_type", "This route requires application/json.");
    if (Number(request.headers.get("content-length")) > 65536)
      return fail(413, "request_too_large", "JSON body exceeds 64 KiB.");
    try {
      const reader = request.body?.getReader();
      if (!reader) return fail(400, "invalid_request", "JSON body is required.");
      const chunks: Uint8Array[] = [];
      let size = 0;
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) break;
        size += chunk.value.byteLength;
        if (size > 65536) { await reader.cancel(); return fail(413, "request_too_large", "JSON body exceeds 64 KiB."); }
        chunks.push(chunk.value);
      }
      const bytes = new Uint8Array(size);
      let offset = 0;
      for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
      const parsed = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error();
      body = JSON.stringify(parsed);
    } catch {
      return fail(400, "invalid_request", "Malformed JSON object.");
    }
  } else if (request.body) {
    return fail(400, "invalid_request", "This route does not accept a body.");
  }
  try {
    const destination = new URL(`/llama-server/${path}${url.search}`, base);
    return await new Promise<Response>((resolve, reject) => {
      const send = destination.protocol === "https:" ? httpsRequest : httpRequest;
      const upstream = send(destination, {
        method,
        headers: body === undefined ? { accept: "application/json" } : { accept: "application/json", "content-type": "application/json" },
        signal: request.signal,
        timeout: timeoutMs,
      }, (response) => {
        const status = response.statusCode ?? 502;
        if (status >= 300 && status < 400) {
          response.destroy();
          resolve(fail(502, "gateway_redirect", "Gateway redirected a llama-server request."));
          return;
        }
        const chunks: Buffer[] = [];
        let size = 0;
        response.on("data", (chunk: Buffer) => {
          size += chunk.byteLength;
          if (size > 8 * 1024 * 1024) {
            response.destroy();
            resolve(fail(502, "response_too_large", "Gateway response exceeds 8 MiB."));
          } else chunks.push(chunk);
        });
        response.on("end", () => {
          if (status === 204) resolve(new Response(null, { status: 204, headers: { "Cache-Control": "no-store" } }));
          else resolve(new Response(Buffer.concat(chunks), { status, headers: { "Cache-Control": "no-store", "Content-Type": response.headers["content-type"]?.startsWith("application/json") ? "application/json" : "text/plain; charset=utf-8" } }));
        });
        response.on("error", reject);
      });
      upstream.on("timeout", () => upstream.destroy(new Error("Gateway request timed out.")));
      upstream.on("error", reject);
      upstream.end(body);
    });
  } catch {
    return fail(502, "gateway_unreachable", "Cannot reach the gateway. Check GATEWAY_URL and that the Rust server is running.");
  }
}
