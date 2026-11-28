// A small router over the history API: the path is the state, links are plain <a>.
// A reverse proxy serves index.html for every path that is not a file.

let path = $state(location.pathname);
let query = $state(new URLSearchParams(location.search));

export const route = {
  get path() {
    return path;
  },
  get query() {
    return query;
  },
};

export function navigate(to: string, replace = false) {
  const url = new URL(to, location.origin);
  if (replace) history.replaceState(null, "", url);
  else history.pushState(null, "", url);
  path = url.pathname;
  query = url.searchParams;
  window.scrollTo(0, 0);
}

addEventListener("popstate", () => {
  path = location.pathname;
  query = new URLSearchParams(location.search);
});

// Same-origin links navigate without a reload; modified clicks open a tab as usual.
addEventListener("click", (event) => {
  if (event.defaultPrevented || event.button !== 0) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  const link = (event.target as Element | null)?.closest("a");
  if (!link || link.target || link.hasAttribute("download")) return;
  const url = new URL(link.href);
  if (url.origin !== location.origin || url.pathname.startsWith("/v1")) return;
  event.preventDefault();
  navigate(url.pathname + url.search);
});

/** Matches `/agents/:alias` style patterns; `null` when the path does not fit. */
export function match(pattern: string, against = path): Record<string, string> | null {
  const want = pattern.split("/");
  const got = against.split("/");
  if (want.length !== got.length) return null;
  const params: Record<string, string> = {};
  for (const [i, part] of want.entries()) {
    const value = got[i] ?? "";
    if (part.startsWith(":")) params[part.slice(1)] = decodeURIComponent(value);
    else if (part !== value) return null;
  }
  return params;
}
