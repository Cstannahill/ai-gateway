import { validateChatRequest } from "./chat-protocol.ts";

const fail = (status: number, message: string) =>
  Response.json(
    { error: { message } },
    { status, headers: { "Cache-Control": "no-store" } },
  );
export async function proxyChat(request: Request, gatewayUrl: string) {
  // Explicit same-origin JSON POST prevents another website driving local inference.
  // Next normalizes loopback names in request.url. Host retains the browser-facing
  // authority; browsers cannot override it. Do not trust X-Forwarded-Host here.
  const requestUrl = new URL(request.url);
  const expectedOrigin = `${requestUrl.protocol}//${request.headers.get("host") ?? requestUrl.host}`;
  if (
    request.headers.get("origin") !== expectedOrigin ||
    request.headers.get("sec-fetch-site") === "cross-site"
  )
    return fail(403, "Chat requests must originate from this console.");
  if (
    request.headers.get("content-type")?.split(";")[0].trim() !==
    "application/json"
  )
    return fail(415, "Chat requires application/json.");
  let base: URL;
  try {
    base = new URL(gatewayUrl);
    if (
      !["http:", "https:"].includes(base.protocol) ||
      base.username ||
      base.password ||
      base.search ||
      base.hash ||
      base.pathname !== "/"
    )
      throw new Error();
  } catch {
    return fail(
      500,
      "GATEWAY_URL must be an HTTP(S) origin without credentials or a path.",
    );
  }
  let body;
  try {
    const reader = request.body?.getReader();
    if (!reader) return fail(400, "A chat request is required.");
    let size = 0,
      text = "";
    const decoder = new TextDecoder();
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > 1024 * 1024) {
          await reader.cancel();
          return fail(
            413,
            "Conversation exceeds the 1 MiB request limit. Export it and start a new chat.",
          );
        }
        text += decoder.decode(value, { stream: true });
      }
    } finally {
      reader.releaseLock();
    }
    body = validateChatRequest(JSON.parse(text + decoder.decode()));
  } catch (e) {
    return fail(400, e instanceof Error ? e.message : "Invalid chat request.");
  }
  const controller = new AbortController();
  const abort = () => controller.abort();
  request.signal.addEventListener("abort", abort, { once: true });
  if (request.signal.aborted) abort();
  const timer = setTimeout(abort, 600000);
  const cleanup = () => {
    clearTimeout(timer);
    request.signal.removeEventListener("abort", abort);
  };
  try {
    const upstream = await fetch(new URL("/v1/chat/completions", base), {
      method: "POST",
      body: JSON.stringify(body),
      signal: controller.signal,
      headers: {
        "Content-Type": "application/json",
        Accept: "text/event-stream",
      },
      cache: "no-store",
      redirect: "error",
    });
    if (!upstream.ok) {
      // Bounded error reads; do not expose upstream headers or cookies.
      const reader = upstream.body?.getReader();
      let message = `Gateway returned HTTP ${upstream.status}.`,
        raw = "",
        size = 0;
      if (reader) {
        const decoder = new TextDecoder();
        try {
          while (size < 65536) {
            const chunk = await reader.read();
            if (chunk.done) break;
            size += chunk.value.byteLength;
            raw += decoder.decode(chunk.value, { stream: true });
          }
        } finally {
          await reader.cancel().catch(() => {});
        }
        try {
          const error = JSON.parse(raw);
          if (typeof error.error?.message === "string")
            message = error.error.message;
        } catch {
          /* Keep HTTP status. */
        }
      }
      cleanup();
      return fail(upstream.status, message);
    }
    if (
      !upstream.body ||
      !upstream.headers.get("content-type")?.includes("text/event-stream")
    ) {
      await upstream.body?.cancel();
      cleanup();
      return fail(502, "Gateway did not return a chat stream.");
    }
    const reader = upstream.body.getReader();
    let bytes = 0;
    const stream = new ReadableStream<Uint8Array>({
      async pull(output) {
        try {
          const { done, value } = await reader.read();
          if (done) {
            cleanup();
            output.close();
            return;
          }
          bytes += value.byteLength;
          if (bytes > 8 * 1024 * 1024)
            throw new Error("Response exceeds 8 MiB.");
          output.enqueue(value);
        } catch (e) {
          abort();
          cleanup();
          output.error(e);
        }
      },
      async cancel() {
        abort();
        cleanup();
        await reader.cancel().catch(() => {});
      },
    });
    return new Response(stream, {
      headers: {
        "Content-Type": "text/event-stream; charset=utf-8",
        "Cache-Control": "no-store",
        "X-Accel-Buffering": "no",
      },
    });
  } catch {
    cleanup();
    return fail(
      502,
      controller.signal.aborted
        ? "Chat stopped or exceeded the 10-minute time limit."
        : "Cannot reach the gateway. Check GATEWAY_URL and that the Rust server is running.",
    );
  }
}
