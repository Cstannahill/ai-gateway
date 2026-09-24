import { Console } from "@/components/console";
export default async function Page({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  return <Console requestId={(await params).id} />;
}
