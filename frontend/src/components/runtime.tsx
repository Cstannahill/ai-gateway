"use client";
import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import {
  QueryClient,
  QueryClientProvider,
  useQuery,
} from "@tanstack/react-query";
import { useSearchParams } from "next/navigation";
import { ApiError, readApi } from "@/lib/api";
type Runtime = {
  mode: "live" | "demo";
  paused: boolean;
  setPaused: (v: boolean) => void;
  zone: "UTC" | "local";
  setZone: (v: "UTC" | "local") => void;
};
const Context = createContext<Runtime>({
  mode: "live",
  paused: false,
  setPaused: () => {},
  zone: "UTC",
  setZone: () => {},
});
export const useRuntime = () => useContext(Context);
export function RuntimeProvider({ children }: { children: ReactNode }) {
  const search = useSearchParams();
  const mode = search.get("demo") === "1" ? "demo" : "live";
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            refetchOnWindowFocus: false,
            retry: (count, error) =>
              count < 2 &&
              (!(error instanceof ApiError) ||
                error.status === 0 ||
                error.status >= 500),
            retryDelay: (n) => Math.min(1000 * 2 ** n, 10000),
            gcTime: 60000,
          },
        },
      }),
  );
  const [paused, setPaused] = useState(false),
    [zone, setZone] = useState<"UTC" | "local">("UTC");
  useEffect(() => {
    client.removeQueries({ predicate: (q) => q.queryKey[0] !== mode });
  }, [client, mode]);
  return (
    <QueryClientProvider client={client}>
      <Context.Provider value={{ mode, paused, setPaused, zone, setZone }}>
        {children}
      </Context.Provider>
    </QueryClientProvider>
  );
}
export function useRead<T>(
  path: string,
  interval = 0,
  enabled = true,
  namespace = "read",
) {
  const { mode, paused } = useRuntime();
  return useQuery<T, ApiError>({
    queryKey: [mode, namespace, path],
    queryFn: ({ signal }) => readApi<T>(path, mode, signal),
    enabled,
    staleTime: interval || 30000,
    refetchInterval: (q) =>
      paused || !interval || mode === "demo"
        ? false
        : Math.min(
            interval * 2 ** Math.min(q.state.fetchFailureCount, 4),
            60000,
          ),
    refetchIntervalInBackground: false,
  });
}
