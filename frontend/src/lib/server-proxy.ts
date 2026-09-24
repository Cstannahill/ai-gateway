import { proxyPath } from "./proxy-policy.ts";
const headers = {
  "Cache-Control": "no-store",
  "Content-Type": "application/json",
};
const fail = (status: number, code: string, message: string) =>
  Response.json({ error: { code, message } }, { status, headers });
export async function proxyRead(
  request: Request,
  segments: string[],
  gatewayUrl: string,
) {
  let path: string;
  try {
    path = proxyPath(segments, new URL(request.url).searchParams);
  } catch (e) {
    return fail(400, "invalid_read_request", String((e as Error).message));
  }
  try {
    const base = new URL(gatewayUrl);
    if (
      !["http:", "https:"].includes(base.protocol) ||
      base.username ||
      base.password ||
      base.search ||
      base.hash ||
      base.pathname !== "/"
    )
      return fail(
        500,
        "gateway_configuration",
        "GATEWAY_URL must be an HTTP(S) origin without credentials or a path.",
      );
    const upstream = await fetch(new URL(path, base), {
      signal: AbortSignal.any([request.signal, AbortSignal.timeout(12000)]),
      cache: "no-store",
      redirect: "error",
      headers: { Accept: "application/json" },
    });
    // Bound unpaginated detail/timeline responses before parsing in the browser.
    const reader = upstream.body?.getReader();
    const chunks: Uint8Array[] = [];
    let length = 0;
    if (reader) {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        length += value.byteLength;
        if (length > 8 * 1024 * 1024) {
          await reader.cancel();
          return fail(
            413,
            "response_too_large",
            "This record exceeds the 8 MiB read limit. A paginated backend export is required.",
          );
        }
        chunks.push(value);
      }
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const c of chunks) {
      bytes.set(c, offset);
      offset += c.length;
    }
    const text = new TextDecoder().decode(bytes);
    try {
      JSON.parse(text);
    } catch {
      return fail(
        502,
        "invalid_gateway_response",
        "Gateway returned a non-JSON response.",
      );
    }
    return new Response(text, { status: upstream.status, headers });
  } catch {
    return fail(
      502,
      "gateway_unreachable",
      "Cannot reach the gateway. Check GATEWAY_URL and that the Rust server is running.",
    );
  }
}
