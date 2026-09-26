import { proxyLlama } from "@/lib/llama-proxy";
export const dynamic = "force-dynamic";
export const runtime = "nodejs";
type Context = { params: Promise<{ path: string[] }> };
const handle = async (request: Request, context: Context) =>
  proxyLlama(request, (await context.params).path, process.env.GATEWAY_URL ?? "http://127.0.0.1:8999");
export const GET = handle;
export const POST = handle;
export const PUT = handle;
export const DELETE = handle;
