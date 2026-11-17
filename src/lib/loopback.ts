// Re-exported by orgPolicy.ts; its own module so providers.ts can use it
// without importing orgPolicy's IPC.

/**
 * Whether `url` is an http(s) URL whose host is `localhost`, in 127.0.0.0/8, or
 * `::1`. Mirrors `org_policy::is_loopback_url`; both are pinned by
 * `loopbackVectors.json`.
 */
export function isLoopbackUrl(url: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url.trim());
  } catch {
    return false;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return false;
  // Already normalised by the parser: lowercase, IPv4 as dotted quads, IPv6
  // compressed in brackets.
  const host = parsed.hostname;
  return host === "localhost" || host === "[::1]" || /^127\.\d+\.\d+\.\d+$/.test(host);
}
