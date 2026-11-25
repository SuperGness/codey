(() => {
  if (
    window !== window.top ||
    location.href !== "app://-/index.html" ||
    performance.getEntriesByType("navigation")[0]?.type === "reload"
  ) return;

  const shownKey = "codey-startup-shown";
  if (sessionStorage.getItem(shownKey)) return;

  const findLoader = () => {
    const root = document.getElementById("root");
    const loader = document.querySelector(
      "#root > .startup-loader, body[data-desktop-starting] > .startup-loader"
    );
    if (
      !root || !loader ||
      loader.matches(".desktop-switch-loader") ||
      !loader.querySelector(":scope > .startup-loader__logo") ||
      !loader.querySelector(":scope > .startup-loader__status[hidden]")
    ) return false;

    sessionStorage.setItem(shownKey, "1");
    const motion = document.documentElement.dataset.startupReducedMotion;
    if (
      motion !== "on" &&
      (motion === "off" || !matchMedia("(prefers-reduced-motion: reduce)").matches)
    ) {
      playStartup(loader, root);
    }
    return true;
  };

  // The document-start script runs before the HTML parser creates Codex's loader.
  if (!findLoader() && document.readyState === "loading") {
    const parser = new MutationObserver(() => {
      if (findLoader()) parser.disconnect();
    });
    parser.observe(document, { childList: true, subtree: true });
    document.addEventListener("DOMContentLoaded", () => {
      parser.disconnect();
      if (!sessionStorage.getItem(shownKey)) findLoader();
    }, { once: true });
  }

  function playStartup(loader, root) {
    const style = document.createElement("style");
    style.textContent = `
      #root > .startup-loader[data-codey-startup] { position: relative; }
      .startup-loader[data-codey-startup] > .codey-startup-video {
        position: absolute;
        inset: 0;
        pointer-events: none;
        visibility: hidden;
      }
      .startup-loader[data-codey-startup] > .codey-startup-video > video {
        position: absolute;
        inset: 0;
        width: 100%;
        height: 100%;
        object-fit: cover;
        object-position: left center;
        background: #110914;
      }
      .startup-loader[data-codey-startup="visible"]:not(.desktop-switch-loader,
        :has(.startup-loader__status:not([hidden]), [data-startup-recovery])) {
        > .codey-startup-video { visibility: visible; }
        /* Electron checks the logo's visible box before showing the window. */
        > .startup-loader__logo {
          background: transparent;
          &::after { display: none; }
        }
      }
    `;
    const stage = document.createElement("div");
    stage.className = "codey-startup-video";
    stage.setAttribute("aria-hidden", "true");
    loader.dataset.codeyStartup = "pending";
    loader.append(style, stage);

    const intro = makeVideo("intro.mp4");
    const loop = makeVideo("loop.mp4");
    let startFrame = 0;
    loop.loop = true;
    loop.style.opacity = "0";
    stage.append(intro, loop);
    const observer = new MutationObserver(() => {
      // Codex moves this same node out of #root before removing it at first paint.
      if (!stage.isConnected) stop();
    });
    observer.observe(root, { childList: true });
    observer.observe(document.body, { childList: true });
    observer.observe(loader, { childList: true });
    window.addEventListener("pagehide", stop, { once: true });

    function makeVideo(file) {
      const video = document.createElement("video");
      video.muted = true;
      video.playsInline = true;
      video.preload = "auto";
      video.src = new URL("/__codey-startup/" + file, location.href).href;
      video.addEventListener("error", stop);
      return video;
    }

    function stop() {
      observer.disconnect();
      cancelAnimationFrame(startFrame);
      document.removeEventListener("visibilitychange", startIntro);
      window.removeEventListener("pagehide", stop);
      delete loader.dataset.codeyStartup;
      stage.remove();
      style.remove();
      for (const video of [intro, loop]) {
        video.removeEventListener("error", stop);
        video.pause();
        video.removeAttribute("src");
        video.load();
      }
    }

    function startIntro() {
      if (
        document.hidden || startFrame ||
        intro.readyState < HTMLMediaElement.HAVE_CURRENT_DATA
      ) return;

      loader.dataset.codeyStartup = "visible";
      // Present the paused first frame before the media clock starts.
      startFrame = requestAnimationFrame(() => {
        startFrame = requestAnimationFrame(() => {
          startFrame = 0;
          if (document.hidden) return;
          document.removeEventListener("visibilitychange", startIntro);
          intro.play().catch(stop);
        });
      });
    }

    intro.addEventListener("loadeddata", startIntro, { once: true });
    document.addEventListener("visibilitychange", startIntro);
    intro.addEventListener("ended", () => {
      if (!stage.isConnected) return;
      loop.addEventListener("playing", () => {
        loop.style.opacity = "1";
        intro.remove();
      }, { once: true });
      loop.play().catch(stop);
    }, { once: true });
  }
})();
