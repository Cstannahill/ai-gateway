import type { Metadata } from "next";
import { Suspense } from "react";
import { RuntimeProvider } from "@/components/runtime";
import { ChatProvider } from "@/features/chat/state";
import "./globals.css";
export const metadata: Metadata = {
  title: "AI Gateway · Observability",
  description:
    "Chat with your models and investigate inference with AI Gateway.",
  robots: { index: false, follow: false },
};
export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" suppressHydrationWarning>
      <body>
        <Suspense
          fallback={<div className="boot">Opening inference console…</div>}
        >
          <RuntimeProvider>
            <ChatProvider>{children}</ChatProvider>
          </RuntimeProvider>
        </Suspense>
      </body>
    </html>
  );
}
