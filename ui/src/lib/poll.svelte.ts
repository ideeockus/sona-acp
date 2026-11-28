// Live data by polling (D98): load now, then every few seconds while the tab is seen.

export function poll(load: () => Promise<void>, everyMs = 4000) {
  $effect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      if (!document.hidden) {
        try {
          await load();
        } catch {
          // The next tick tries again; a 401 already went to login.
        }
      }
      if (!stopped) timer = setTimeout(tick, everyMs);
    };
    void tick();
    const onVisible = () => {
      if (!document.hidden) {
        clearTimeout(timer);
        void tick();
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      stopped = true;
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  });
}
