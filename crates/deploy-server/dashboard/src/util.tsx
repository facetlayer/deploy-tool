import { useEffect, useState } from "react";

/** The server hands back SQLite `datetime` strings, not unix seconds. */
export function parseTs(value: string | null | undefined): number | null {
  if (!value) return null;
  // SQLite writes "2026-08-31 12:00:00" with no zone; the deploy server stores
  // UTC, so say so rather than letting the browser read it as local time.
  const normalized = /Z|[+-]\d\d:?\d\d$/.test(value)
    ? value
    : `${value.replace(" ", "T")}Z`;
  const ms = Date.parse(normalized);
  return Number.isNaN(ms) ? null : Math.floor(ms / 1000);
}

export function timeAgo(ts: number | null): string {
  if (!ts) return "never";
  const s = Math.floor(Date.now() / 1000 - ts);
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  if (s < 30 * 86400) return `${Math.floor(s / 86400)}d ago`;
  return new Date(ts * 1000).toISOString().slice(0, 10);
}

export function fmtTime(ts: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function Time({ at, exact }: { at: string | null | undefined; exact?: boolean }) {
  const ts = parseTs(at);
  return <span className="mono" title={fmtTime(ts)}>{exact ? fmtTime(ts) : timeAgo(ts)}</span>;
}

/** Simple hash router: "#/projects/api" → ["projects", "api"]. */
export function useRoute(): string[] {
  const parse = () => window.location.hash.replace(/^#\/?/, "").split("/").filter(Boolean).map(decodeURIComponent);
  const [route, setRoute] = useState<string[]>(parse);
  useEffect(() => {
    const onChange = () => setRoute(parse());
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return route;
}

export function navigate(path: string) {
  window.location.hash = path.startsWith("#") ? path : `#/${path.replace(/^\//, "")}`;
}

export function Link({ to, children, className }: { to: string; children: React.ReactNode; className?: string }) {
  return <a className={className} href={`#/${to.replace(/^\//, "")}`}>{children}</a>;
}

export function Badge({ kind, children }: { kind: string; children: React.ReactNode }) {
  return <span className={`badge badge-${kind}`}>{children}</span>;
}

export function useAsync<T>(fn: () => Promise<T>, deps: unknown[]): { data: T | null; error: string | null; reload: () => void } {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let cancelled = false;
    fn().then((d) => { if (!cancelled) { setData(d); setError(null); } })
      .catch((e) => { if (!cancelled) setError(e.message); });
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);
  return { data, error, reload: () => setTick((t) => t + 1) };
}

export function ErrorBox({ error }: { error: string | null }) {
  return error ? <div className="error-box">{error}</div> : null;
}

export function Empty({ children }: { children: React.ReactNode }) {
  return <div className="empty">{children}</div>;
}
