(() => {
  window.__codeyPetControlShieldCleanup?.();

  const enabled = ["__CODEY_SLIM_PET__"][0] === "true";
  // Codex renamed the pet entry points to mini; keys below cover the current
  // mini naming, the older pets naming, and the ChatGPT-side pet manager, so
  // any supported client build keeps the shield active.
  const petControlIds = new Set([
    "settings.mini.show",
    "settings.mini.hide",
    "settings.nav.miniAndPets",
    "settings.personalization.mini.wake",
    "settings.personalization.mini.tuckAway",
    "codex.profileFooter.showMini",
    "codex.profileFooter.hideMini",
    "codex.command.showMiniOverlay",
    "codex.command.hideMiniOverlay",
    "codex.command.wakeMiniOverlay",
    "codex.commandDescription.showMiniOverlay",
    "codex.commandDescription.tuckAwayMiniOverlay",
    "codex.localConversation.remoteHostedPip.sendToPet",
    "settings.appearance.pets",
    "settings.personalization.pets",
    "settings.pets",
    "settings.nav.pets",
    "settings.section.pets",
    "settings.personalization.pets.openPet",
    "settings.personalization.pets.tuckAwayPet",
    "codex.profileFooter.showPet",
    "codex.profileFooter.hidePet",
    "codex.command.openPetOverlay",
    "codex.command.tuckAwayPetOverlay",
  ]);
  const petControlIdPrefixes = [
    "settings.mini.",
    "settings.nav.mini",
    "settings.personalization.mini.",
    "settings.chatGpt.personalization.pet.",
    "settings.appearance.pets.",
    "settings.nav.pets.",
    "settings.personalization.pets.",
    "settings.section.pets.",
    "settings.pets.",
  ];
  // Exact-match only: matching "mini" as a substring would also hide model
  // options such as "GPT-5.4-Mini" in the model picker.
  const petControlLabels = [
    "pet", "pets", "wake pet", "show pet", "tuck away pet", "hide pet",
    "refresh custom pets", "create your own pet", "open custom pets folder",
    "宠物", "唤醒宠物", "显示宠物", "收起宠物", "隐藏宠物",
    "刷新自定义宠物", "创建自己的宠物", "打开自定义宠物文件夹",
    "寵物", "喚醒寵物", "顯示寵物", "收起寵物", "隱藏寵物",
    "重新整理自訂寵物", "建立自己的寵物", "開啟自訂寵物資料夾",
    "mini", "minis", "wake mini", "show mini", "tuck away mini", "hide mini",
    "refresh custom minis", "create your own mini", "open custom minis folder",
    "迷你", "唤醒 Mini", "显示 Mini", "收起 Mini", "隐藏 Mini",
    "刷新自定义 Mini", "创建自己的 Mini", "打开自定义 Mini 文件夹",
    "喚醒 Mini", "顯示 Mini", "隱藏 Mini",
    "重新整理自訂 Mini", "建立自己的 Mini", "開啟自訂 Mini 資料夾",
  ];
  const fallbackLabelPattern = new RegExp(
    `^(?:${petControlLabels.join("|")})$`,
    "i",
  );
  const controlSelector = "button, [role=button], [role=menuitem], [role=option], [role=tab]";

  const isPetControlId = (value) =>
    petControlIds.has(value)
      || petControlIdPrefixes.some((prefix) => value.startsWith(prefix));

  const isPetControl = (control) => {
    if (!(control instanceof HTMLElement)) return false;
    const descriptor = window.__codeyMutationDispatcher.controlDescriptor(control);
    if (fallbackLabelPattern.test(descriptor)) return true;

    // Deliberately not memoised. React reuses host elements and swaps both
    // __reactProps$ and __reactFiber$ independently, and the walk below reads
    // every matching key, so no cheap identity token covers the whole verdict.
    // At ~3 µs per control the shared walk is not worth a fail-open cache on a shield
    // that has to fail closed; the observer throttling above is what keeps this
    // off the streaming hot path.
    return window.__codeySharedRuntime.reactInternalGraphIncludes(
      control,
      isPetControlId,
    );
  };

  const controlsWithin = window.__codeyMutationDispatcher.controlsWithin;

  const block = (root = document) => {
    if (!enabled) return 0;
    let blocked = 0;
    controlsWithin(root, controlSelector).forEach((control) => {
      if (!isPetControl(control)) return;
      const fullyBlocked = control.getAttribute("data-codey-pet-control-blocked") === "true"
        && control.getAttribute("aria-hidden") === "true"
        && control.getAttribute("tabindex") === "-1"
        && control.getAttribute("inert") !== null
        && String(control.style.display || "").startsWith("none")
        && (!("disabled" in control) || control.disabled);
      if (!fullyBlocked) {
        control.setAttribute("data-codey-pet-control-blocked", "true");
        control.setAttribute("aria-hidden", "true");
        control.setAttribute("tabindex", "-1");
        control.setAttribute("inert", "");
        control.style.setProperty("display", "none", "important");
        if ("disabled" in control && !control.disabled) control.disabled = true;
      }
      blocked += 1;
    });
    return blocked;
  };

  if (!enabled) {
    window.__codeyBlockNativePetControls = () => 0;
    window.__codeyPetControlShield = Object.freeze({ enabled, block: () => 0, isPetControl });
    window.__codeyPetControlShieldCleanup = () => {
      delete window.__codeyBlockNativePetControls;
      delete window.__codeyPetControlShield;
      delete window.__codeyPetControlShieldCleanup;
    };
    return;
  }

  const shieldLifecycle = window.__codeyMutationDispatcher?.createShieldLifecycle({
    attributeFilter: ["aria-label", "role", "title"],
    block,
    eventSelector: controlSelector,
    isControl: isPetControl,
  });
  window.__codeyBlockNativePetControls = block;
  window.__codeyPetControlShield = Object.freeze({ enabled, block, isPetControl });
  window.__codeyPetControlShieldCleanup = () => {
    shieldLifecycle?.cleanup();
    delete window.__codeyBlockNativePetControls;
    delete window.__codeyPetControlShield;
    delete window.__codeyPetControlShieldCleanup;
  };
  block();
})();
