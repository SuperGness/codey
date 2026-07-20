(() => {
  window.__codeyPetControlShieldCleanup?.();

  const enabled = "__CODEY_SLIM_PET__" === "true";
  const petControlIds = new Set([
    "settings.personalization.pets.openPet",
    "settings.personalization.pets.tuckAwayPet",
    "codex.profileFooter.showPet",
    "codex.profileFooter.hidePet",
    "codex.command.openPetOverlay",
    "codex.command.tuckAwayPetOverlay",
    "openAvatarOverlay",
    "tuckAwayAvatarOverlay",
    "avatar-overlay-open",
  ]);
  const fallbackLabelPattern = /^(?:wake pet|show pet|tuck away pet|hide pet|唤醒宠物|显示宠物|收起宠物|隐藏宠物|喚醒寵物|顯示寵物|收起寵物|隱藏寵物)$/i;
  const reactInternalKeyPattern = /^__(?:reactProps|reactFiber|reactInternalInstance)\$.*/;

  const containsPetControlId = (value, depth = 0, seen = new WeakSet()) => {
    if (typeof value === "string") return petControlIds.has(value);
    if (!value || typeof value !== "object" || depth > 7 || seen.has(value)) return false;
    seen.add(value);
    for (const [key, child] of Object.entries(value)) {
      if (["return", "child", "sibling", "stateNode", "_owner"].includes(key)) continue;
      if (containsPetControlId(child, depth + 1, seen)) return true;
    }
    return false;
  };

  const isPetControl = (control) => {
    if (!(control instanceof HTMLElement)) return false;
    const descriptor = [
      control.getAttribute("aria-label"),
      control.getAttribute("title"),
      control.textContent,
    ].filter(Boolean).join(" ").replace(/\s+/g, " ").trim();
    if (fallbackLabelPattern.test(descriptor)) return true;

    return Object.keys(control)
      .filter((key) => reactInternalKeyPattern.test(key))
      .some((key) => {
        try {
          const internal = control[key];
          return containsPetControlId(internal?.memoizedProps ?? internal);
        } catch {
          return false;
        }
      });
  };

  const block = () => {
    if (!enabled) return 0;
    let blocked = 0;
    document.querySelectorAll("button, [role=button], [role=menuitem]").forEach((control) => {
      if (!isPetControl(control)) return;
      control.setAttribute("data-codey-pet-control-blocked", "true");
      control.setAttribute("aria-hidden", "true");
      control.setAttribute("tabindex", "-1");
      control.setAttribute("inert", "");
      control.style.setProperty("display", "none", "important");
      if ("disabled" in control) control.disabled = true;
      blocked += 1;
    });
    return blocked;
  };

  const stopPetControlEvent = (event) => {
    if (!enabled) return;
    const control = event.target instanceof Element
      ? event.target.closest("button, [role=button], [role=menuitem]")
      : null;
    if (!isPetControl(control)) return;
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation?.();
  };

  const eventNames = ["pointerdown", "click", "keydown"];
  eventNames.forEach((eventName) => {
    document.addEventListener(eventName, stopPetControlEvent, true);
  });
  window.__codeyBlockNativePetControls = block;
  window.__codeyPetControlShield = Object.freeze({ enabled, block, isPetControl });
  window.__codeyPetControlShieldCleanup = () => {
    eventNames.forEach((eventName) => {
      document.removeEventListener(eventName, stopPetControlEvent, true);
    });
    delete window.__codeyBlockNativePetControls;
    delete window.__codeyPetControlShield;
    delete window.__codeyPetControlShieldCleanup;
  };
  block();
})();
