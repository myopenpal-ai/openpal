import { describe, expect, it } from "vitest";
import { checkLink, isLocalHost, namedHost, parseWebLink } from "./linkSafety";

const check = (href: string, text = "a link") => checkLink(parseWebLink(href)!, text);

describe("parseWebLink", () => {
  it("accepts http(s) in any case, and nothing else", () => {
    expect(parseWebLink("https://example.com")?.href).toBe("https://example.com/");
    expect(parseWebLink("HTTPS://Example.COM/a")?.href).toBe("https://example.com/a");
    expect(parseWebLink("http://example.com")).not.toBeNull();
    for (const href of ["mailto:a@example.com", "/relative", "README.md", "//example.com", "", undefined, "#top", "javascript:alert(1)", "file:///etc/passwd"]) {
      expect(parseWebLink(href)).toBeNull();
    }
  });
});

describe("checkLink", () => {
  it("shows an IDN host in its xn-- form", () => {
    expect(check("https://exämple.com/").host).toBe("xn--exmple-cua.com");
  });

  it("shows the real host behind a user@ prefix", () => {
    const link = check("https://trusted.com@evil.example/path");
    expect(link.host).toBe("evil.example");
    expect(link.href).toBe("https://trusted.com@evil.example/path");
  });

  it("keeps the port in the host", () => {
    expect(check("http://example.com:8080/").host).toBe("example.com:8080");
  });

  it("keeps the whole address, query and all", () => {
    expect(check("https://evil.example/c?d=my%20secret#x").href).toBe("https://evil.example/c?d=my%20secret#x");
  });

  it("flags link text that names a different host", () => {
    expect(check("https://evil.example/", "https://bank.com/login").shownHost).toBe("bank.com");
    expect(check("https://evil.example/", "www.bank.com").shownHost).toBe("www.bank.com");
    expect(check("https://evil.example/", "see hxxps://bank.com for more").shownHost).toBe("bank.com");
  });

  it("does not flag link text naming the same host", () => {
    expect(check("https://bank.com/x", "https://bank.com").shownHost).toBeNull();
    expect(check("http://www.bank.com", "www.bank.com").shownHost).toBeNull();
    expect(check("https://bank.com/", "(https://BANK.com).").shownHost).toBeNull();
    expect(check("https://xn--exmple-cua.com/", "https://exämple.com").shownHost).toBeNull();
  });

  it("never flags text that doesn't look like an address", () => {
    expect(check("https://evil.example/", "README.md").shownHost).toBeNull();
    expect(check("https://evil.example/", "bank.com").shownHost).toBeNull();
    expect(check("https://evil.example/", "the docs").shownHost).toBeNull();
  });

  it("marks local destinations", () => {
    expect(check("http://127.0.0.1:8080/").local).toBe(true);
    expect(check("https://example.com/").local).toBe(false);
  });
});

describe("namedHost", () => {
  it("reads the host after a user@ prefix in the text too", () => {
    expect(namedHost("https://user@bank.com/")).toBe("bank.com");
  });

  it("falls back to the raw text when it isn't a parseable host", () => {
    expect(namedHost("https://bank com")).toBe("bank");
    expect(namedHost("https://")).toBeNull();
  });
});

describe("isLocalHost", () => {
  it("recognizes loopback, private and link-local addresses", () => {
    const local = [
      "localhost", "LOCALHOST.", "app.localhost", "printer.local",
      "127.0.0.1", "127.8.9.10", "0.0.0.0", "10.1.2.3", "172.16.0.1", "172.31.255.255",
      "192.168.1.1", "169.254.169.254", "[::1]", "[::]", "[fd12:3456::1]", "[fc00::1]",
      "[fe80::1]", "[febf::1]", "[::ffff:7f00:1]", "[::ffff:c0a8:101]",
    ];
    for (const host of local) expect(isLocalHost(host), host).toBe(true);
  });

  it("leaves public addresses and names alone", () => {
    const remote = [
      "example.com", "8.8.8.8", "172.15.0.1", "172.32.0.1", "192.169.0.1", "11.0.0.1",
      "[2001:db8::1]", "[fe00::1]", "[fec0::1]", "[::ffff:808:808]", "localhost.example.com",
    ];
    for (const host of remote) expect(isLocalHost(host), host).toBe(false);
  });

  it("sees through the parser's normalization of odd IPv4 forms", () => {
    expect(isLocalHost(new URL("http://2130706433/").hostname)).toBe(true);
    expect(isLocalHost(new URL("http://0x7f.1/").hostname)).toBe(true);
    expect(isLocalHost(new URL("http://[::ffff:127.0.0.1]/").hostname)).toBe(true);
  });
});
