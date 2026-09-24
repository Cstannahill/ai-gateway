import { proxyChat } from "@/lib/chat-proxy";
export const dynamic = "force-dynamic";
export const runtime = "nodejs";
export async function POST(request: Request) {
  return proxyChat(request, process.env.GATEWAY_URL ?? "http://127.0.0.1:8999");
}
