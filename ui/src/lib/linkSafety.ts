/**
 * What the link-confirmation dialog shows about a link in rendered Markdown.
 * A reply can be steered by hidden instructions into packing conversation text
 * into a link's address, so the user sees where it really goes before it opens.
 */
export interface LinkCheck {
  /** The normalized address: what the dialog shows and what opens. */
  href: string;
  /** The destination as the browser resolves it: an IDN host in its `xn--`
   * form, any `user@` prefix dropped, the port kept. */
  host: string;
  /** The host the link's text names, when that text looks like an address
   * and names a different host than the destination. */
  shownHost: string | null;
  /** Whether the destination is this computer or the local network. */
  local: boolean;
}

/** An http(s) link as the URL parser sees it, or `null` for anything else —
 * relative, protocol-relative, `mailto:`, empty. The scheme is matched the
 * way the parser does it, so `HTTPS://` counts. */
export function parseWebLink(href: string | undefined): URL | null {
  if (!href) return null;
  try {
    const url = new URL(href);
    return url.protocol === "http:" || url.protocol === "https:" ? url : null;
  } catch {
    return null;
  }
}

export function checkLink(url: URL, linkText: string): LinkCheck {
  const destination = bareHost(url.hostname);
  const named = namedHost(linkText);
  return {
    href: url.href,
    host: url.host,
    shownHost: named !== null && named !== destination ? named : null,
    local: isLocalHost(url.hostname),
  };
}

const bareHost = (host: string) => host.toLowerCase().replace(/\.$/, "");

// Punctuation that sits around an address in prose, not inside its host.
const TRAILING_PUNCTUATION = /[.,;:!?)\]}'"»>]+$/;

/**
 * The host a link's text claims, if the text looks like an address: it
 * contains `://` or starts with `www.`. Text like `README.md` claims nothing.
 * Whatever scheme the text shows is parsed as http, so `hxxps://` or a bare
 * `://` still yields the host it names.
 */
export function namedHost(text: string): string | null {
  const trimmed = text.trim();
  const scheme = trimmed.indexOf("://");
  let address: string;
  if (scheme !== -1) address = trimmed.slice(scheme + 3);
  else if (/^www\./i.test(trimmed)) address = trimmed;
  else return null;
  address = address.split(/\s/)[0].replace(TRAILING_PUNCTUATION, "");
  try {
    const host = bareHost(new URL(`http://${address}`).hostname);
    if (host) return host;
  } catch {
    // Not a parseable host — fall back to the text itself below.
  }
  const raw = address.split(/[/?#\\]/)[0];
  return raw ? bareHost(raw.slice(raw.lastIndexOf("@") + 1)) : null;
}

/**
 * Loopback, private and link-local destinations — this computer or the local
 * network. Takes a hostname as the URL parser serializes it, which has already
 * turned forms like `http://2130706433/` into `127.0.0.1` and brackets IPv6.
 */
export function isLocalHost(hostname: string): boolean {
  const host = bareHost(hostname);
  if (host === "localhost" || host.endsWith(".localhost") || host.endsWith(".local")) return true;
  const v4 = /^(\d+)\.(\d+)\.(\d+)\.(\d+)$/.exec(host);
  if (v4) return isLocalV4(Number(v4[1]), Number(v4[2]));
  if (host.startsWith("[") && host.endsWith("]")) return isLocalV6(host.slice(1, -1));
  return false;
}

function isLocalV4(a: number, b: number): boolean {
  return (
    a === 0 || // "this network"
    a === 127 || // loopback
    a === 10 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    (a === 169 && b === 254) // link-local
  );
}

function isLocalV6(address: string): boolean {
  if (address === "::1" || address === "::") return true;
  // IPv4-mapped, which the parser serializes in hex: [::ffff:7f00:1].
  const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(address);
  if (mapped) {
    const high = parseInt(mapped[1], 16);
    return isLocalV4(high >> 8, high & 0xff);
  }
  const first = address.split(":")[0];
  if (first.length !== 4) return false;
  return /^f[cd]/.test(first) || /^fe[89ab]/.test(first); // fc00::/7 unique local, fe80::/10 link-local
}
