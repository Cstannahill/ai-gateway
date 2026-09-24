import { proxyRead } from "@/lib/server-proxy";
export const dynamic = "force-dynamic";
export async function GET(
  request: Request,
  context: { params: Promise<{ path: string[] }> },
) {
  return proxyRead(
    request,
    (await context.params).path,
    process.env.GATEWAY_URL ?? "http://127.0.0.1:8999",
  );
}
